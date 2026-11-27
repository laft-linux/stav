//! Merging contributions into the final value.
//!
//! Contributions form a trie keyed by attribute paths. Merging has three
//! phases. Structure registers declared defaults and selects priorities,
//! then validates static competitors of late whole values with `E0504`.
//! The dependency graph includes every contribution the merge may evaluate,
//! including lower whole values and possible fallback defaults. Finalization
//! computes attributes on demand, evaluates participating contributions after
//! their dependencies, and memoizes the resulting values.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use super::body::{
    Body, Contrib, DefaultFragment, Deferred, DeferredForm, Kind, Parts, Pending, Prio, Written,
};
use super::call::Building;
use super::env::Env;
use super::val::{Key, Val};
use super::{Eval, MAX_VALUE_DEPTH, R, Stop};
use crate::check::FileId;
use crate::check::graph;
use crate::check::model::{SelfRead, UnitId};
use crate::check::types::{StructId, SumId, Type, TypeId};
use crate::diagnostic::{Diagnostic, FileDiagnostic};
use crate::span::{SourceSpan, Span};
use crate::value::AttrPath;

/// A node of the contribution trie: one attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NodeId(u32);

impl NodeId {
    pub(crate) const ROOT: NodeId = NodeId(0);

    fn index(self) -> usize {
        self.0 as usize
    }
}

/// One step of an attribute path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Seg {
    /// A struct field, or a payload field below a variant.
    Field(u32),
    /// A map key: the string, or the variant name of a sum key.
    Key(Rc<str>),
    /// A computed entry's instance, used only in the static dependency graph.
    Placeholder(u32),
    /// The payload of a variant of a tagged sum.
    Variant(u32),
}

/// What `self` refers to at one depth.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Frame {
    /// A block instance of the merged value.
    Node(NodeId),
    /// A struct value being built, by its index on the building stack.
    Building(usize),
}

/// Values computed once per evaluation.
#[derive(Debug, Default)]
pub(crate) struct Caches {
    pub(crate) building: Vec<Building>,
    /// The `self` reads of each field default.
    pub(crate) reads: BTreeMap<(StructId, usize), Rc<Vec<SelfRead>>>,
    /// Read-free default fragments, independent of instance frames and paths.
    pub(crate) defaults: BTreeMap<(UnitId, usize), DefaultFragment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Shape {
    /// Not reached by the structure phase.
    Open,
    /// An option that is `none`.
    Absent,
    /// A struct or map that nothing sets.
    Unset,
    /// Contributions that conflict, already reported.
    Conflict,
    Struct(StructId),
    /// A map with the given key type.
    Map(TypeId),
    /// A sum selected by tagged blocks, with the selected variant once it
    /// is known.
    Tagged(Option<u32>),
    /// The payload of the selected variant of a tagged sum.
    Payload(SumId, u32),
    Leaf,
}

#[derive(Debug, Clone)]
enum State {
    Open,
    Busy,
    Done(Val),
    Missing,
}

#[derive(Debug, Clone)]
struct Node {
    parent: Option<NodeId>,
    seg: Option<Seg>,
    depth: u32,
    children: BTreeMap<Seg, NodeId>,
    contribs: Vec<u32>,
    /// Declared defaults retained for the static dependency graph.
    checks: Vec<u32>,
    ty: TypeId,
    shape: Shape,
    winners: Vec<u32>,
    /// Deferred contributions to a struct, map, option or tagged sum,
    /// which must be evaluated before anything inside it is final.
    pending: Vec<u32>,
    defaults: bool,
    /// Whether possible fallback defaults were registered for this instance.
    checked_defaults: bool,
    /// Whether a late whole value was registered here before evaluation.
    late_whole: bool,
    /// The strongest whole value that replaced lower parts at this boundary.
    replacement: Option<Prio>,
    state: State,
    coverage: Cell<[Option<bool>; 4]>,
}

impl Node {
    fn new(parent: Option<NodeId>, seg: Option<Seg>, depth: u32) -> Self {
        Self {
            parent,
            seg,
            depth,
            children: BTreeMap::new(),
            contribs: Vec::new(),
            checks: Vec::new(),
            ty: TypeId::ERROR,
            shape: Shape::Open,
            winners: Vec::new(),
            pending: Vec::new(),
            defaults: false,
            checked_defaults: false,
            late_whole: false,
            replacement: None,
            state: State::Open,
            coverage: Cell::new([None; 4]),
        }
    }
}

/// The contribution trie.
#[derive(Debug, Clone, Default)]
pub(crate) struct Merge {
    pub(crate) contribs: Vec<Contrib>,
    nodes: Vec<Node>,
    locations: Vec<NodeId>,
    /// Attribute values expanded into sum selections, distinct from blocks.
    attributes: BTreeSet<u32>,
    /// Whether static whole validation has completed.
    static_checked: bool,
    /// Creator witnesses reused during structure and revalidated after mutations.
    fallback_creators: Option<RefCell<BTreeMap<NodeId, (NodeId, u32)>>>,
    /// Whether this trie is a disposable graph containing placeholder instances.
    pub(crate) graph_only: bool,
}

impl Merge {
    fn node(&self, n: NodeId) -> &Node {
        &self.nodes[n.index()]
    }

    fn node_mut(&mut self, n: NodeId) -> &mut Node {
        &mut self.nodes[n.index()]
    }

    fn invalidate_coverage(&self, n: NodeId, priority: Option<Prio>) {
        let mut current = Some(n);
        while let Some(n) = current {
            let node = self.node(n);
            let mut cache = node.coverage.get();
            for (i, entry) in cache.iter_mut().enumerate() {
                if priority.is_none_or(|p| i < p as usize) {
                    *entry = None;
                }
            }
            node.coverage.set(cache);
            current = node.parent;
        }
    }

    fn set_type(&mut self, n: NodeId, ty: TypeId) {
        if self.node(n).ty != ty {
            self.node_mut(n).ty = ty;
            self.invalidate_coverage(n, None);
        }
    }

    fn set_shape(&mut self, n: NodeId, shape: Shape) {
        let node = self.node(n);
        let complete_before = matches!(node.shape, Shape::Struct(_) | Shape::Payload(..));
        let complete_after = matches!(shape, Shape::Struct(_) | Shape::Payload(..));
        // Coverage reads shape only for complete struct and payload markers.
        let coverage_changes = complete_before != complete_after
            && node.contribs.iter().any(|&id| {
                let contrib = self.contrib(id);
                contrib.complete && matches!(contrib.kind, Kind::Present)
            });
        self.node_mut(n).shape = shape;
        if coverage_changes {
            self.invalidate_coverage(n, None);
        }
    }

    fn set_kind(&mut self, id: u32, kind: Kind) {
        let priority = self.contrib(id).prio;
        self.contribs[id as usize].kind = kind;
        self.invalidate_coverage(self.locations[id as usize], Some(priority));
    }

    pub(crate) fn child(&self, n: NodeId, seg: &Seg) -> Option<NodeId> {
        self.nodes.get(n.index())?.children.get(seg).copied()
    }

    fn child_or_new(&mut self, n: NodeId, seg: Seg) -> NodeId {
        if let Some(found) = self.child(n, &seg) {
            return found;
        }
        let id = NodeId(u32::try_from(self.nodes.len()).unwrap_or(u32::MAX));
        let depth = self.node(n).depth + 1;
        self.nodes
            .push(Node::new(Some(n), Some(seg.clone()), depth));
        self.node_mut(n).children.insert(seg, id);
        self.invalidate_coverage(n, None);
        id
    }

    fn insert(&mut self, base: NodeId, path: &[Seg], contrib: Contrib) -> NodeId {
        let mut n = base;
        for seg in path {
            n = self.child_or_new(n, seg.clone());
        }
        let id = u32::try_from(self.contribs.len()).unwrap_or(u32::MAX);
        let priority = contrib.prio;
        self.contribs.push(contrib);
        self.locations.push(n);
        self.invalidate_coverage(n, Some(priority));
        self.node_mut(n).contribs.push(id);
        n
    }

    /// Records a possible fallback without evaluating or selecting it.
    fn insert_check(&mut self, base: NodeId, path: &[Seg], contrib: Contrib) {
        let mut n = base;
        for seg in path {
            n = self.child_or_new(n, seg.clone());
        }
        let id = u32::try_from(self.contribs.len()).unwrap_or(u32::MAX);
        let priority = contrib.prio;
        self.contribs.push(contrib);
        self.locations.push(n);
        self.invalidate_coverage(n, Some(priority));
        self.node_mut(n).checks.push(id);
    }

    /// Returns suspended defaults under `n`, excluding `n` itself.
    fn checks_below(&self, n: NodeId) -> Vec<u32> {
        let mut checks = Vec::new();
        let mut stack: Vec<NodeId> = self.node(n).children.values().copied().collect();
        while let Some(c) = stack.pop() {
            let node = self.node(c);
            checks.extend(node.checks.iter().copied());
            stack.extend(node.children.values().copied());
        }
        checks
    }

    fn static_at(&self, n: NodeId) -> Vec<u32> {
        self.node(n).contribs.clone()
    }

    fn path(&self, n: NodeId) -> Vec<Seg> {
        let mut segs = Vec::new();
        let mut current = Some(n);
        while let Some(c) = current {
            let node = self.node(c);
            if let Some(seg) = &node.seg {
                segs.push(seg.clone());
            }
            current = node.parent;
        }
        segs.reverse();
        segs
    }

    fn resolve(&self, path: &[Seg]) -> Option<NodeId> {
        let mut n = NodeId::ROOT;
        for seg in path {
            n = self.child(n, seg)?;
        }
        Some(n)
    }

    fn contrib(&self, id: u32) -> &Contrib {
        &self.contribs[id as usize]
    }

    /// Returns whether a contribution exists at or under `n`.
    fn is_set(&self, n: NodeId) -> bool {
        let mut stack = vec![n];
        while let Some(c) = stack.pop() {
            let node = self.node(c);
            if node
                .contribs
                .iter()
                .any(|&id| !matches!(self.contrib(id).kind, Kind::Spent))
            {
                return true;
            }
            stack.extend(node.children.values().copied());
        }
        false
    }

    /// Returns the contributions under `n`, not at it.
    fn below(&self, n: NodeId) -> Vec<u32> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.node(n).children.values().copied().collect();
        while let Some(c) = stack.pop() {
            let node = self.node(c);
            out.extend(
                node.contribs
                    .iter()
                    .copied()
                    .filter(|&id| !matches!(self.contrib(id).kind, Kind::Spent)),
            );
            stack.extend(node.children.values().copied());
        }
        out.sort_unstable();
        out
    }

    /// Returns the nodes the structure phase reached from `top`, `top`
    /// first, each before the nodes inside it.
    fn reached(&self, top: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = vec![top];
        while let Some(n) = stack.pop() {
            out.push(n);
            let mut inner = self.inside(n);
            inner.reverse();
            stack.extend(inner);
        }
        out
    }

    /// Returns the nodes whose values make up the value of `n`.
    fn inside(&self, n: NodeId) -> Vec<NodeId> {
        let node = self.node(n);
        match node.shape {
            Shape::Struct(_) | Shape::Payload(..) => node
                .children
                .iter()
                .filter(|(seg, _)| matches!(seg, Seg::Field(_)))
                .map(|(_, &c)| c)
                .collect(),
            Shape::Map(..) => node
                .children
                .iter()
                .filter(|(seg, _)| matches!(seg, Seg::Key(_) | Seg::Placeholder(_)))
                .map(|(_, &c)| c)
                .collect(),
            Shape::Tagged(Some(v)) => self.child(n, &Seg::Variant(v)).into_iter().collect(),
            _ => Vec::new(),
        }
    }
}

/// Whether an optional attribute is present.
enum Presence {
    Absent,
    Present,
    Conflict(Vec<u32>),
}

impl<'c> Eval<'c> {
    /// Structures all contributions, validates static whole competitors,
    /// checks every dependency that may run, and finalizes the merged value.
    pub(crate) fn merge_all(&mut self, pending: Vec<Pending>) -> R<Val> {
        self.merge.nodes.push(Node::new(None, None, 0));
        for p in pending {
            if p.path.len() > MAX_VALUE_DEPTH as usize {
                let at = p.contrib.at();
                return Err(self.fail(
                    at,
                    "E0612",
                    format!("an attribute path is longer than {MAX_VALUE_DEPTH} steps"),
                ));
            }
            self.merge.insert(NodeId::ROOT, &p.path, p.contrib);
        }
        self.static_paths(NodeId::ROOT, self.c.root_ty, false);
        self.merge.fallback_creators = Some(RefCell::new(BTreeMap::new()));
        self.structure(NodeId::ROOT, self.c.root_ty)?;
        self.static_paths(NodeId::ROOT, self.c.root_ty, true);
        if !self.errors.is_empty() {
            return Ok(Val::None);
        }
        self.prepare_default_shapes()?;
        self.static_paths(NodeId::ROOT, self.c.root_ty, true);
        self.merge.static_checked = true;
        if !self.errors.is_empty() {
            return Ok(Val::None);
        }
        let mut fallback_owners: BTreeSet<NodeId> = self
            .merge
            .nodes
            .iter()
            .filter(|node| !node.checks.is_empty())
            .filter_map(|node| node.parent)
            .collect();
        let mut creators = BTreeMap::new();
        fallback_owners.retain(|&owner| {
            self.potential_fallback_with(owner, |boundary| {
                *creators
                    .entry(boundary)
                    .or_insert_with(|| self.fallback_creator(boundary))
            })
        });
        let order = self.check_cycles(NodeId::ROOT, &fallback_owners)?;
        self.merge.fallback_creators = None;
        if !self.errors.is_empty() {
            return Ok(Val::None);
        }
        for n in order {
            let node = self.merge.node(n);
            let deferred = !node.pending.is_empty()
                || node
                    .winners
                    .iter()
                    .any(|&id| matches!(self.merge.contrib(id).kind, Kind::Deferred(_)))
                || !node.checks.is_empty()
                    && node
                        .parent
                        .is_some_and(|owner| fallback_owners.contains(&owner))
                    && !self.covered(n, Prio::Declared);
            if deferred {
                self.finalize(n)?;
            }
        }
        self.finalize(NodeId::ROOT)
    }

    /// Exposes participating fixed default shapes before the dependency graph.
    /// Defaults below unresolved late wholes remain suspended fallbacks.
    /// At one non-optional map path both whole maps may contribute keys.
    fn prepare_default_shapes(&mut self) -> R<()> {
        let mut index = 0;
        while index < self.merge.nodes.len() {
            let n = NodeId(u32::try_from(index).unwrap_or(u32::MAX));
            index += 1;
            let mut current = Some(n);
            let mut waiting = false;
            while let Some(c) = current {
                let ancestor = self.merge.node(c);
                let shared_map = c == n
                    && matches!(
                        self.c.types.get(self.c.types.peel(ancestor.ty)),
                        Type::Map(_, _)
                    );
                waiting |= !shared_map
                    && ancestor.pending.iter().any(|&id| {
                        matches!(&self.merge.contrib(id).kind, Kind::Deferred(d) if d.is_late())
                            && !self.covered(c, self.merge.contrib(id).prio)
                    });
                current = ancestor.parent;
            }
            if waiting {
                continue;
            }
            loop {
                let candidate = self.merge.node(n).pending.iter().copied().find(|&id| {
                    let contrib = self.merge.contrib(id);
                    contrib.prio == Prio::Declared
                        && matches!(&contrib.kind, Kind::Deferred(d) if d.form == DeferredForm::Value && !d.is_late())
                        && !self.covered(n, contrib.prio)
                });
                let Some(id) = candidate else { break };
                let contrib = self.merge.contrib(id).clone();
                let Kind::Deferred(deferred) = &contrib.kind else {
                    break;
                };
                let out = self.run_deferred(deferred, &contrib)?;
                self.merge.set_kind(id, Kind::Spent);
                for p in out {
                    self.merge.insert(n, &p.path, p.contrib);
                }
                self.refresh_default_checks(n);
                let ty = self.merge.node(n).ty;
                self.structure(n, ty)?;
            }
        }
        Ok(())
    }

    /// Rebuilds fallbacks whose eligibility changed when a fixed shape arrived.
    fn refresh_default_checks(&mut self, top: NodeId) {
        let path = self.merge.path(top);
        let mut stack = vec![top];
        while let Some(n) = stack.pop() {
            let contribs = &self.merge.contribs;
            let node = &mut self.merge.nodes[n.index()];
            node.checks.retain(|&id| {
                contribs[id as usize]
                    .default_owner
                    .as_deref()
                    .is_none_or(|owner| !owner.starts_with(&path))
            });
            node.checked_defaults = false;
            node.late_whole = false;
            stack.extend(node.children.values().copied());
        }
    }

    /// Returns a place for diagnostics about `n`: its first contribution,
    /// or the first one inside it, or the first one of an enclosing node.
    pub(crate) fn node_at(&self, n: NodeId) -> SourceSpan {
        let merge = &self.merge;
        let first = |ids: &[u32]| {
            ids.iter()
                .map(|&id| merge.contrib(id))
                .find(|c| c.prio != Prio::Declared)
                .or_else(|| ids.first().map(|&id| merge.contrib(id)))
                .map(Contrib::at)
        };
        if let Some(at) = first(&merge.node(n).contribs) {
            return at;
        }
        if n != NodeId::ROOT
            && let Some(at) = first(&merge.below(n))
        {
            return at;
        }
        let mut current = merge.node(n).parent;
        while let Some(c) = current {
            if let Some(at) = first(&merge.node(c).contribs) {
                return at;
            }
            current = merge.node(c).parent;
        }
        SourceSpan {
            file: self
                .c
                .entries
                .first()
                .map_or(FileId::new(0), |&u| self.unit_file(u)),
            span: Span { start: 0, end: 0 },
        }
    }

    fn structure(&mut self, n: NodeId, ty: TypeId) -> R<()> {
        let at = self.node_at(n);
        if self.merge.node(n).depth > MAX_VALUE_DEPTH {
            return Err(self.fail(
                at,
                "E0612",
                format!("a value nests more than {MAX_VALUE_DEPTH} levels deep"),
            ));
        }
        self.enter(at)?;
        let result = self.structure_inner(n, ty);
        self.leave();
        result
    }

    fn structure_inner(&mut self, n: NodeId, ty: TypeId) -> R<()> {
        self.merge.set_type(n, ty);
        {
            let node = self.merge.node_mut(n);
            node.winners.clear();
            node.pending.clear();
        }
        let types = &self.c.types;
        let option = types.unwrap_option(ty).is_some();
        let inner = types.unwrap_option(ty).unwrap_or_else(|| types.peel(ty));
        if let Type::Sum(s) = types.get(inner)
            && types.sum(*s).is_nullary()
        {
            let ids = self.merge.node(n).contribs.clone();
            for id in ids {
                if let Kind::Select(variant) = self.merge.contrib(id).kind {
                    self.merge.set_kind(id, Kind::Value(Val::Tag(*s, variant)));
                }
            }
        }
        let structural = matches!(
            types.get(inner),
            Type::Struct(_) | Type::Map(_, _) | Type::Data
        ) || matches!(types.get(inner), Type::Sum(s) if !types.sum(*s).is_nullary());
        self.classify_wholes(n);
        if option {
            match self.presence(n) {
                Presence::Absent => {
                    self.merge.set_shape(n, Shape::Absent);
                    return Ok(());
                }
                Presence::Conflict(ids) => {
                    let whole_conflict = !self.merge.static_checked
                        && structural
                        && self.whole_conflict(n).is_some();
                    if !whole_conflict {
                        self.conflict(n, &ids, "E0501");
                    }
                    self.merge.set_shape(n, Shape::Conflict);
                    return Ok(());
                }
                Presence::Present => {}
            }
        }
        let deferred: Vec<u32> = self
            .merge
            .node(n)
            .contribs
            .iter()
            .copied()
            .filter(|&id| matches!(self.merge.contrib(id).kind, Kind::Deferred(_)))
            .collect();
        match self.c.types.get(inner).clone() {
            Type::Struct(s) => {
                if !option
                    && n != NodeId::ROOT
                    && !self.merge.is_set(n)
                    && (self.merge.static_checked || self.merge.node(n).checks.is_empty())
                {
                    self.merge.set_shape(n, Shape::Unset);
                    return Ok(());
                }
                self.merge.node_mut(n).pending = deferred;
                self.merge.set_shape(n, Shape::Struct(s));
                let waiting = self.enclosing_whole_pending(n);
                if !waiting || !self.merge.static_checked {
                    self.add_defaults(n, s, waiting)?;
                }
                let fields: Vec<TypeId> = self
                    .c
                    .types
                    .structure(s)
                    .fields
                    .iter()
                    .map(|f| f.ty)
                    .collect();
                for (i, field_ty) in fields.into_iter().enumerate() {
                    let child = self.merge.child_or_new(n, field_seg(i));
                    self.structure(child, field_ty)?;
                }
            }
            Type::Map(k, v) => {
                if !option
                    && !self.merge.is_set(n)
                    && (self.merge.static_checked || self.merge.node(n).checks.is_empty())
                {
                    self.merge.set_shape(n, Shape::Unset);
                    return Ok(());
                }
                self.merge.node_mut(n).pending = deferred;
                self.merge.set_shape(n, Shape::Map(k));
                let children: Vec<NodeId> = self
                    .merge
                    .node(n)
                    .children
                    .iter()
                    .filter(|(seg, _)| matches!(seg, Seg::Key(_) | Seg::Placeholder(_)))
                    .map(|(_, &c)| c)
                    .collect();
                for child in children {
                    self.structure(child, v)?;
                }
            }
            Type::Sum(s) if self.has_selection(n) => self.tagged(n, s, deferred)?,
            _ => {
                let candidates: Vec<u32> = self
                    .merge
                    .node(n)
                    .contribs
                    .iter()
                    .copied()
                    .filter(|&id| match &self.merge.contrib(id).kind {
                        Kind::Value(Val::None) => !option,
                        Kind::Value(_) | Kind::Deferred(_) => true,
                        _ => false,
                    })
                    .collect();
                let top = candidates
                    .iter()
                    .map(|&id| self.merge.contrib(id).prio)
                    .max();
                let mut winners: Vec<u32> = candidates
                    .into_iter()
                    .filter(|&id| Some(self.merge.contrib(id).prio) == top)
                    .collect();
                winners.sort_by_key(|&id| self.merge.contrib(id).seq);
                let node = self.merge.node_mut(n);
                node.winners = winners;
                self.merge.set_shape(n, Shape::Leaf);
            }
        }
        Ok(())
    }

    /// Types fixed paths before coverage queries, optionally validating wholes.
    /// Absent options and unselected variants are included, but late values are not.
    fn static_paths(&mut self, n: NodeId, ty: TypeId, validate: bool) {
        self.merge.set_type(n, ty);
        self.classify_wholes(n);
        let inner = self
            .c
            .types
            .unwrap_option(ty)
            .unwrap_or_else(|| self.c.types.peel(ty));
        if validate
            && (matches!(
                self.c.types.get(inner),
                Type::Struct(_) | Type::Map(_, _) | Type::Data
            ) || matches!(
                self.c.types.get(inner),
                Type::Sum(s) if !self.c.types.sum(*s).is_nullary()
            ))
            && let Some(ids) = self.whole_conflict(n)
        {
            self.conflict(n, &ids, "E0504");
        }
        let children: Vec<(Seg, NodeId)> = self
            .merge
            .node(n)
            .children
            .iter()
            .map(|(seg, &child)| (seg.clone(), child))
            .collect();
        for (seg, child) in children {
            let child_ty = match (seg, self.c.types.get(inner)) {
                (Seg::Field(i), Type::Struct(s)) => self
                    .c
                    .types
                    .structure(*s)
                    .fields
                    .get(i as usize)
                    .map(|f| f.ty),
                (Seg::Key(_) | Seg::Placeholder(_), Type::Map(_, v)) => Some(*v),
                (Seg::Variant(v), Type::Sum(s)) => {
                    let params = self
                        .c
                        .types
                        .sum(*s)
                        .variants
                        .get(v as usize)
                        .and_then(|variant| variant.fields.as_ref());
                    let fields: Vec<(usize, TypeId)> = params
                        .into_iter()
                        .flatten()
                        .enumerate()
                        .map(|(i, field)| (i, field.ty))
                        .collect();
                    for (i, ty) in fields {
                        if let Some(field) = self.merge.child(child, &field_seg(i)) {
                            self.static_paths(field, ty, validate);
                        }
                    }
                    None
                }
                _ => None,
            };
            if let Some(ty) = child_ty {
                self.static_paths(child, ty, validate);
            }
        }
    }

    /// Classifies only expressions that genuinely read `self` as late wholes.
    fn classify_wholes(&mut self, n: NodeId) {
        let types = &self.c.types;
        let node = self.merge.node(n);
        let inner = types
            .unwrap_option(node.ty)
            .unwrap_or_else(|| types.peel(node.ty));
        let structural = matches!(
            types.get(inner),
            Type::Struct(_) | Type::Map(_, _) | Type::Data
        ) || matches!(types.get(inner), Type::Sum(s) if !types.sum(*s).is_nullary());
        let late = structural
            && node.contribs.iter().chain(&node.checks).any(
                |&id| matches!(&self.merge.contrib(id).kind, Kind::Deferred(d) if d.is_late()),
            );
        self.merge.node_mut(n).late_whole |= late;
    }

    /// Returns a late whole and its forbidden static competitors.
    fn whole_conflict(&self, n: NodeId) -> Option<Vec<u32>> {
        let at = self.merge.static_at(n);
        for &id in &at {
            let contrib = self.merge.contrib(id);
            if !matches!(&contrib.kind, Kind::Deferred(d) if d.is_late()) {
                continue;
            }
            let mut competitors: Vec<u32> = at
                .iter()
                .copied()
                .chain(self.merge.below(n))
                .filter(|&other| {
                    other != id
                        && self.merge.contrib(other).prio != Prio::Declared
                        && !(self.c.types.unwrap_option(self.merge.node(n).ty).is_some()
                            && at.contains(&other)
                            && matches!(self.merge.contrib(other).kind, Kind::Value(Val::None)))
                        && self.merge.contrib(other).prio <= contrib.prio
                        && !matches!(self.merge.contrib(other).kind, Kind::Spent)
                })
                .collect();
            if !competitors.is_empty() {
                competitors.sort_by_key(|&other| self.merge.contrib(other).seq);
                let mut ids = vec![id];
                ids.extend(competitors);
                return Some(ids);
            }
        }
        None
    }

    /// Defers implicit defaults until enclosing whole values have supplied
    /// their fields, so those defaults cannot become provisional winners.
    fn enclosing_whole_pending(&self, n: NodeId) -> bool {
        let mut current = Some(n);
        while let Some(c) = current {
            let node = self.merge.node(c);
            if matches!(
                node.shape,
                Shape::Struct(_) | Shape::Map(_) | Shape::Tagged(_)
            ) && node.pending.iter().any(|&id| {
                let contrib = self.merge.contrib(id);
                if matches!(&contrib.kind, Kind::Deferred(d) if !d.is_late())
                    && self.covered(c, contrib.prio)
                {
                    return false;
                }
                !self.merge.static_checked || c == n || !self.covered_path(c, n, contrib.prio)
            }) {
                return true;
            }
            if matches!(node.seg, Some(Seg::Placeholder(_))) {
                break;
            }
            current = node.parent;
        }
        false
    }

    /// Whether higher contributions, including supplied whole-value parts,
    /// determine every part of `n`. Four priority slots make each node's
    /// recursive coverage scan run once per unchanged set of higher contributions.
    /// Mutations invalidate only affected nodes and ancestors, never value
    /// memoization or winner selection.
    fn covered(&self, n: NodeId, priority: Prio) -> bool {
        if let Some(found) = self.merge.node(n).coverage.get()[priority as usize] {
            return found;
        }
        let result = self.covered_inner(n, priority);
        let node = self.merge.node(n);
        let mut cache = node.coverage.get();
        cache[priority as usize] = Some(result);
        node.coverage.set(cache);
        result
    }

    fn covered_inner(&self, n: NodeId, priority: Prio) -> bool {
        let node = self.merge.node(n);
        let option = self.c.types.unwrap_option(node.ty);
        let inner = option.unwrap_or_else(|| self.c.types.peel(node.ty));
        if option.is_some()
            && node.contribs.iter().any(|&id| {
                let c = self.merge.contrib(id);
                c.prio > priority && matches!(c.kind, Kind::Value(Val::None) | Kind::Deferred(_))
            })
        {
            return true;
        }
        if matches!(self.c.types.get(inner), Type::Map(_, _)) {
            return false;
        }
        if node.contribs.iter().any(|&id| {
            let c = self.merge.contrib(id);
            c.prio > priority
                && match &c.kind {
                    Kind::Value(_) | Kind::Deferred(_) => true,
                    Kind::Present => {
                        c.complete && matches!(node.shape, Shape::Struct(_) | Shape::Payload(..))
                    }
                    Kind::Select(_) => {
                        self.merge.attributes.contains(&id)
                            || matches!(
                                self.c.types.get(inner),
                                Type::Sum(s) if self.c.types.sum(*s).is_nullary()
                            )
                    }
                    _ => false,
                }
        }) {
            return true;
        }
        let Type::Struct(s) = self.c.types.get(inner) else {
            return false;
        };
        let fields = &self.c.types.structure(*s).fields;
        if fields.is_empty() {
            return node.contribs.iter().any(|&id| {
                let c = self.merge.contrib(id);
                c.prio > priority && matches!(c.kind, Kind::Present)
            });
        }
        fields.iter().enumerate().all(|(i, _)| {
            self.merge
                .child(n, &field_seg(i))
                .is_some_and(|child| self.covered(child, priority))
        })
    }

    /// Whether a higher contribution replaces lower parts at this path.
    fn replaced(&self, n: NodeId, priority: Prio) -> bool {
        self.merge.node(n).replacement.is_some_and(|p| p > priority) || self.covered(n, priority)
    }

    /// Records a replacement even if its value has no children here.
    fn replace_lower(&mut self, n: NodeId, priority: Prio) {
        if self
            .merge
            .node(n)
            .replacement
            .is_some_and(|p| p >= priority)
        {
            return;
        }
        self.merge.node_mut(n).replacement = Some(priority);
        let ids: Vec<u32> = self
            .merge
            .node(n)
            .contribs
            .iter()
            .copied()
            .chain(self.merge.below(n))
            .collect();
        for id in ids {
            if self.merge.contrib(id).prio < priority {
                self.merge.set_kind(id, Kind::Spent);
            }
        }
    }

    /// Whether a higher contribution between `base` and `n`
    /// makes `n` independent of the whole value at `base`.
    fn covered_path(&self, base: NodeId, n: NodeId, priority: Prio) -> bool {
        let mut current = Some(n);
        while let Some(c) = current {
            if self.replaced(c, priority) {
                return true;
            }
            if c == base {
                break;
            }
            current = self.merge.node(c).parent;
        }
        false
    }

    /// Adds a whole value's supplied parts without replacing missing map keys.
    fn insert_generated(&mut self, base: NodeId, pending: Pending) {
        let mut n = base;
        if self.replaced(n, pending.contrib.prio) {
            return;
        }
        for seg in &pending.path {
            n = self.merge.child_or_new(n, seg.clone());
            if self.replaced(n, pending.contrib.prio) {
                return;
            }
            if pending.contrib.complete && matches!(seg, Seg::Key(_) | Seg::Placeholder(_)) {
                self.replace_lower(n, pending.contrib.prio);
            }
        }
        let node = self.merge.node(n);
        let inner = self
            .c
            .types
            .unwrap_option(node.ty)
            .unwrap_or_else(|| self.c.types.peel(node.ty));
        if pending.contrib.complete
            && matches!(pending.contrib.kind, Kind::Present)
            && (self.c.types.as_struct(inner).is_some() || matches!(node.shape, Shape::Payload(..)))
        {
            self.replace_lower(n, pending.contrib.prio);
        }
        self.merge.insert(n, &[], pending.contrib);
    }

    /// Whether a whole value has supplied this complete struct instance.
    /// In the graph, an opaque expression constructs its own defaults, so its
    /// result cannot leave omitted fields for the placeholder's defaults.
    fn supplied(&self, n: NodeId) -> bool {
        self.merge.node(n).contribs.iter().any(|id| {
            let c = self.merge.contrib(*id);
            c.complete && matches!(c.kind, Kind::Present)
                || self.merge.graph_only
                    && matches!(&c.kind, Kind::Deferred(d) if d.form == DeferredForm::Value)
        })
    }

    /// Whether an instance can be created where its enclosing whole lacks it.
    fn potential_fallback(&self, n: NodeId) -> bool {
        self.potential_fallback_with(n, |boundary| self.fallback_creator(boundary))
    }

    fn potential_fallback_with(&self, n: NodeId, mut creator: impl FnMut(NodeId) -> bool) -> bool {
        let mut current = Some(n);
        let mut boundaries = Vec::new();
        while let Some(c) = current {
            let node = self.merge.node(c);
            let option = self.c.types.unwrap_option(node.ty).is_some();
            if option {
                boundaries.push(c);
            }
            let computed_map = node.contribs.iter().chain(&node.checks).any(|&id| {
                matches!(&self.merge.contrib(id).kind, Kind::Deferred(d) if d.form == DeferredForm::MapLiteral)
            });
            if node.late_whole || computed_map {
                return !boundaries.is_empty() && boundaries.into_iter().all(&mut creator);
            }
            if !option
                && matches!(
                    node.seg,
                    Some(Seg::Key(_) | Seg::Placeholder(_) | Seg::Variant(_))
                )
            {
                boundaries.push(c);
            }
            current = node.parent;
        }
        false
    }

    /// Finds applicable contributions that can create an absent instance.
    /// An instance's own defaults cannot create it, but a conditional default
    /// of an enclosing instance can. A genuine whole supplies complete values.
    fn fallback_creator(&self, boundary: NodeId) -> bool {
        let path = self.merge.path(boundary);
        let applicable = |n: NodeId, id: u32| {
            let contrib = self.merge.contrib(id);
            !matches!(contrib.kind, Kind::Spent)
                && !contrib
                    .default_owner
                    .as_deref()
                    .is_some_and(|owner| owner.starts_with(&path))
                && !self.declared_replaced(n, id)
                && !(n == boundary
                    && (matches!(contrib.kind, Kind::Value(Val::None))
                        || matches!(&contrib.kind, Kind::Deferred(d) if d.is_late())))
        };
        // A witness survives additions only while it remains attached and
        // applicable. Rechecking also observes spent or replaced defaults,
        // newly typed boundaries and refreshed suspended checks.
        if let Some(cache) = &self.merge.fallback_creators {
            let witness = cache.borrow().get(&boundary).copied();
            if let Some((n, id)) = witness {
                let node = self.merge.node(n);
                let attached = node.contribs.contains(&id) || node.checks.contains(&id);
                let selection = self.merge.node(boundary).parent != Some(n)
                    || matches!(
                        (&self.merge.node(boundary).seg, &self.merge.contrib(id).kind),
                        (Some(Seg::Variant(v)), Kind::Select(selected)) if v == selected
                    );
                if attached && selection && applicable(n, id) {
                    return true;
                }
            }
        }
        let remember = |n, id| {
            if let Some(cache) = &self.merge.fallback_creators {
                cache.borrow_mut().insert(boundary, (n, id));
            }
            true
        };
        let node = self.merge.node(boundary);
        if let (Some(Seg::Variant(variant)), Some(parent)) = (&node.seg, node.parent)
            && let Some(id) = self
                .merge
                .node(parent)
                .contribs
                .iter()
                .chain(&self.merge.node(parent).checks)
                .copied()
                .find(|&id| {
                    matches!(self.merge.contrib(id).kind, Kind::Select(v) if v == *variant)
                        && applicable(parent, id)
                })
        {
            return remember(parent, id);
        }
        let mut stack = vec![boundary];
        while let Some(n) = stack.pop() {
            let node = self.merge.node(n);
            if let Some(id) = node
                .contribs
                .iter()
                .chain(&node.checks)
                .copied()
                .find(|&id| applicable(n, id))
            {
                return remember(n, id);
            }
            stack.extend(node.children.values().copied());
        }
        false
    }

    fn has_selection(&self, n: NodeId) -> bool {
        self.merge
            .node(n)
            .contribs
            .iter()
            .any(|&id| matches!(self.merge.contrib(id).kind, Kind::Select(_)))
    }

    /// Decides whether an optional attribute is present: when a
    /// contribution sets it, or something inside it, with a higher
    /// priority than every `none`.
    fn presence(&self, n: NodeId) -> Presence {
        let merge = &self.merge;
        let mut none: Vec<u32> = Vec::new();
        let mut present: Vec<u32> = Vec::new();
        let node = merge.node(n);
        let checks = node.checks.iter().filter(|_| !merge.static_checked);
        for &id in node.contribs.iter().chain(checks) {
            if !merge.static_checked && self.declared_replaced(n, id) {
                continue;
            }
            match merge.contrib(id).kind {
                Kind::Value(Val::None) => none.push(id),
                Kind::Spent => {}
                _ => present.push(id),
            }
        }
        present.extend(merge.below(n).into_iter().filter(|&id| {
            merge.static_checked || !self.declared_replaced(merge.locations[id as usize], id)
        }));
        if !merge.static_checked {
            present.extend(merge.checks_below(n));
        }
        let top = |ids: &[u32]| ids.iter().map(|&id| merge.contrib(id).prio).max();
        match (top(&none), top(&present)) {
            (_, None) => Presence::Absent,
            (None, Some(_)) => Presence::Present,
            (Some(a), Some(b)) if a > b => Presence::Absent,
            (Some(a), Some(b)) if a < b => Presence::Present,
            (Some(a), _) => {
                let mut ids: Vec<u32> = none
                    .into_iter()
                    .chain(present)
                    .filter(|&id| merge.contrib(id).prio == a)
                    .collect();
                ids.sort_by_key(|&id| merge.contrib(id).seq);
                Presence::Conflict(ids)
            }
        }
    }

    /// Whether a higher late boundary always removes an enclosing default.
    fn declared_replaced(&self, n: NodeId, id: u32) -> bool {
        let contrib = self.merge.contrib(id);
        let Some(owner) = contrib.default_owner.as_ref() else {
            return false;
        };
        let owner = self.merge.resolve(owner);
        let mut current = Some(n);
        while let Some(c) = current {
            let node = self.merge.node(c);
            if owner == Some(c) {
                break;
            }
            let types = &self.c.types;
            let inner = types
                .unwrap_option(node.ty)
                .unwrap_or_else(|| types.peel(node.ty));
            if (types.unwrap_option(node.ty).is_some()
                || matches!(
                    types.get(inner),
                    Type::Struct(_) | Type::Sum(_) | Type::Data
                ))
                && node.contribs.iter().chain(&node.checks).any(|&other| {
                    let candidate = self.merge.contrib(other);
                    candidate.prio > contrib.prio
                        && matches!(&candidate.kind, Kind::Deferred(d) if d.is_late())
                })
            {
                return true;
            }
            current = node.parent;
        }
        false
    }

    /// Registers or installs defaults with identical eligibility and suspension.
    fn instance_defaults(
        &mut self,
        n: NodeId,
        file: FileId,
        span: Span,
        fields: Vec<(TypeId, Option<usize>)>,
        validation_only: bool,
    ) -> R<()> {
        if validation_only && !self.potential_fallback(n)
            || (!validation_only || self.merge.graph_only) && self.supplied(n)
        {
            return Ok(());
        }
        let node = self.merge.node_mut(n);
        let installed = if validation_only {
            &mut node.checked_defaults
        } else {
            &mut node.defaults
        };
        if std::mem::replace(installed, true) {
            return Ok(());
        }
        let Some(unit) = self.file_unit(file) else {
            return Err(self.bug(file, span, "a declaration has no unit"));
        };
        let owner: Rc<[Seg]> = self.merge.path(n).into();
        let body = Body {
            unit,
            file,
            base: Vec::new(),
            frames: vec![owner.to_vec()],
            env: Env::default(),
            via: None,
            defer_values: true,
        };
        for (i, (ty, default)) in fields.into_iter().enumerate() {
            let Some(default) = default else { continue };
            let child = self.merge.child_or_new(n, field_seg(i));
            if validation_only && !self.merge.node(child).checks.is_empty()
                || self.merge.node(child).contribs.iter().any(|&id| {
                    let c = self.merge.contrib(id);
                    c.prio == Prio::Declared
                        && !matches!(c.kind, Kind::Spent)
                        && !self.declared_replaced(child, id)
                })
            {
                continue;
            }
            let span = self.tree(file).node(default).map_or(span, |e| e.span());
            let template = Contrib {
                prio: Prio::Declared,
                kind: Kind::Present,
                file,
                span,
                via: None,
                seq: 0,
                parts: None,
                complete: false,
                default_owner: Some(owner.clone()),
            };
            let e = self.expr_at(unit, default)?;
            let mut out = Vec::new();
            self.contribute(&body, Vec::new(), ty, e, &template, &mut out)?;
            for p in out {
                if validation_only {
                    self.merge.insert_check(child, &p.path, p.contrib);
                } else {
                    self.merge.insert(child, &p.path, p.contrib);
                }
            }
        }
        Ok(())
    }

    fn add_defaults(&mut self, n: NodeId, s: StructId, validation_only: bool) -> R<()> {
        let def = self.c.types.structure(s);
        let fields = def.fields.iter().map(|f| (f.ty, f.default)).collect();
        self.instance_defaults(n, def.file, def.span, fields, validation_only)
    }

    /// Expands an attribute sum value into its selection and payload.
    fn expand_sum_payload(&mut self, n: NodeId, s: SumId, id: u32) {
        let contrib = self.merge.contrib(id).clone();
        let Kind::Value(value) = &contrib.kind else {
            return;
        };
        let (variant, fields, set) = match value {
            Val::Tag(_, v) => (*v, Vec::new(), Vec::new()),
            Val::Variant(v) => (v.index, v.fields.clone(), v.set.clone()),
            _ => return,
        };
        self.merge.set_kind(id, Kind::Select(variant));
        self.merge.attributes.insert(id);
        self.merge.invalidate_coverage(n, Some(contrib.prio));
        let params: Vec<(String, TypeId)> = self
            .c
            .types
            .sum(s)
            .variants
            .get(variant as usize)
            .and_then(|v| v.fields.as_ref())
            .map(|f| f.iter().map(|p| (p.name.clone(), p.ty)).collect())
            .unwrap_or_default();
        let payload = self.merge.child_or_new(n, Seg::Variant(variant));
        let template = contrib.payload();
        let complete = contrib.complete;
        let mut out = Vec::new();
        if complete {
            out.push(Pending {
                path: Vec::new(),
                contrib: template.with(Kind::Present),
            });
        }
        for (i, ((name, ty), value)) in params.into_iter().zip(fields).enumerate() {
            if complete || set.get(i).copied().unwrap_or(true) {
                let template = template.part(&name);
                self.decompose(vec![field_seg(i)], ty, value, &template, &mut out);
            }
        }
        for p in out {
            if complete {
                self.insert_generated(payload, p);
            } else {
                self.merge.insert(payload, &p.path, p.contrib);
            }
        }
    }

    /// Structures a sum selected by tagged blocks. Whole values become a
    /// selection and payload contributions at their own priority.
    fn tagged(&mut self, n: NodeId, s: SumId, deferred: Vec<u32>) -> R<()> {
        let ids = self.merge.node(n).contribs.clone();
        for id in ids {
            self.expand_sum_payload(n, s, id);
        }
        let selections: Vec<u32> = self
            .merge
            .node(n)
            .contribs
            .iter()
            .copied()
            .filter(|&id| matches!(self.merge.contrib(id).kind, Kind::Select(_)))
            .collect();
        let top = selections
            .iter()
            .map(|&id| self.merge.contrib(id).prio)
            .max();
        let undecided = deferred
            .iter()
            .any(|&id| Some(self.merge.contrib(id).prio) >= top);
        self.merge.node_mut(n).pending = deferred;
        if undecided {
            self.merge.set_shape(n, Shape::Tagged(None));
            return Ok(());
        }
        let mut winners: Vec<u32> = selections
            .into_iter()
            .filter(|&id| Some(self.merge.contrib(id).prio) == top)
            .collect();
        winners.sort_by_key(|&id| self.merge.contrib(id).seq);
        let variants: Vec<u32> = winners
            .iter()
            .filter_map(|&id| match self.merge.contrib(id).kind {
                Kind::Select(v) => Some(v),
                _ => None,
            })
            .collect();
        let Some(&variant) = variants.first() else {
            self.merge.set_shape(n, Shape::Tagged(None));
            return Ok(());
        };
        if variants.iter().any(|&v| v != variant) {
            self.conflict(n, &winners, "E0502");
            self.merge.set_shape(n, Shape::Conflict);
            return Ok(());
        }
        self.merge.set_shape(n, Shape::Tagged(Some(variant)));
        let payload = self.merge.child_or_new(n, Seg::Variant(variant));
        self.payload(payload, s, variant)
    }

    fn payload(&mut self, n: NodeId, s: SumId, variant: u32) -> R<()> {
        self.merge.set_type(n, TypeId::ERROR);
        self.merge.set_shape(n, Shape::Payload(s, variant));
        let params: Vec<TypeId> = self
            .c
            .types
            .sum(s)
            .variants
            .get(variant as usize)
            .and_then(|v| v.fields.as_ref())
            .map(|f| f.iter().map(|p| p.ty).collect())
            .unwrap_or_default();
        let waiting = self.enclosing_whole_pending(n);
        if !waiting || !self.merge.static_checked {
            let def = self.c.types.sum(s);
            let fields = def
                .variants
                .get(variant as usize)
                .and_then(|v| v.fields.as_ref())
                .into_iter()
                .flatten()
                .map(|f| (f.ty, f.default))
                .collect();
            self.instance_defaults(n, def.file, def.span, fields, waiting)?;
        }
        for (i, ty) in params.into_iter().enumerate() {
            let child = self.merge.child_or_new(n, field_seg(i));
            self.structure(child, ty)?;
        }
        Ok(())
    }

    /// Returns the merged value of an attribute, computing it on first use.
    pub(crate) fn finalize(&mut self, n: NodeId) -> R<Val> {
        match &self.merge.node(n).state {
            State::Done(v) => return Ok(v.clone()),
            State::Missing => return Ok(Val::None),
            State::Busy => {
                let at = self.node_at(n);
                let path = self.display_path(n);
                return Err(self.fail(
                    at,
                    "E0607",
                    format!("`self` dependencies form a cycle through `{path}`"),
                ));
            }
            State::Open => {}
        }
        let at = self.node_at(n);
        self.enter(at)?;
        self.merge.node_mut(n).state = State::Busy;
        let result = self.finalize_inner(n);
        self.leave();
        match result {
            Ok(Some(value)) => {
                self.merge.node_mut(n).state = State::Done(value.clone());
                Ok(value)
            }
            Ok(None) => {
                self.merge.node_mut(n).state = State::Missing;
                Ok(Val::None)
            }
            Err(e) => {
                self.merge.node_mut(n).state = State::Open;
                Err(e)
            }
        }
    }

    /// Returns whether an attribute has no value after finalizing.
    fn is_missing(&self, n: NodeId) -> bool {
        matches!(self.merge.node(n).state, State::Missing)
    }

    fn finalize_inner(&mut self, n: NodeId) -> R<Option<Val>> {
        self.settle(n)?;
        let at = self.node_at(n);
        match self.merge.node(n).shape.clone() {
            Shape::Open | Shape::Unset => Ok(None),
            Shape::Absent => Ok(self.merge.is_set(n).then_some(Val::None)),
            Shape::Conflict => Ok(Some(Val::None)),
            Shape::Struct(s) => {
                let count = self.c.types.structure(s).fields.len();
                let mut values = Vec::with_capacity(count);
                for i in 0..count {
                    let child = self.merge.child_or_new(n, field_seg(i));
                    let value = self.finalize(child)?;
                    let field = &self.c.types.structure(s).fields[i];
                    if self.is_missing(child) && field.is_required(&self.c.types) {
                        self.missing(n, child);
                    }
                    values.push(value);
                }
                self.new_struct(s, values, at).map(Some)
            }
            Shape::Map(k) => {
                let children: Vec<(Seg, NodeId)> = self
                    .merge
                    .node(n)
                    .children
                    .iter()
                    .map(|(s, &c)| (s.clone(), c))
                    .collect();
                let key_sum = self.c.types.as_sum(k);
                let mut entries = Vec::with_capacity(children.len());
                for (seg, child) in children {
                    let Seg::Key(text) = seg else { continue };
                    let value = self.finalize(child)?;
                    if self.is_missing(child) {
                        continue;
                    }
                    let key = match key_sum {
                        Some(sum) => match self.c.types.sum(sum).variant(&text) {
                            Some((index, _)) => self.new_variant(sum, index, None, at)?,
                            None => {
                                let at = self.node_at(child);
                                return Err(self.fail(
                                    at,
                                    "E0604",
                                    format!(
                                        "`{text}` is not a variant of `{}`",
                                        self.c.types.sum(sum).name
                                    ),
                                ));
                            }
                        },
                        None => Val::Str(text.clone()),
                    };
                    entries.push((key, value));
                }
                self.new_map(entries, at).map(Some)
            }
            Shape::Tagged(Some(v)) => match self.merge.child(n, &Seg::Variant(v)) {
                Some(child) => self.finalize(child).map(Some),
                None => Ok(None),
            },
            Shape::Tagged(None) => Ok(None),
            Shape::Payload(s, variant) => {
                let params = self
                    .c
                    .types
                    .sum(s)
                    .variants
                    .get(variant as usize)
                    .and_then(|v| v.fields.clone());
                let Some(params) = params else {
                    return self.new_variant(s, variant as usize, None, at).map(Some);
                };
                let mut values = Vec::with_capacity(params.len());
                for (i, param) in params.iter().enumerate() {
                    let child = self.merge.child_or_new(n, field_seg(i));
                    let value = self.finalize(child)?;
                    if self.is_missing(child)
                        && param.default.is_none()
                        && self.c.types.unwrap_option(param.ty).is_none()
                    {
                        self.missing(n, child);
                    }
                    values.push(value);
                }
                self.new_variant(s, variant as usize, Some(values), at)
                    .map(Some)
            }
            Shape::Leaf => self.leaf(n),
        }
    }

    fn missing(&mut self, parent: NodeId, child: NodeId) {
        let at = if parent == NodeId::ROOT {
            SourceSpan {
                file: self
                    .c
                    .entries
                    .first()
                    .map_or(FileId::new(0), |&u| self.unit_file(u)),
                span: Span { start: 0, end: 0 },
            }
        } else {
            self.node_at(parent)
        };
        let path = self.display_path(child);
        self.errors.push(FileDiagnostic {
            file: Some(at.file),
            diagnostic: Diagnostic::error(
                "E0801",
                at.span,
                format!("the required field `{path}` has no value"),
            )
            .with_help("set it, or give the field a default"),
        });
    }

    /// Evaluates the deferred contributions to the structs, maps, options
    /// and tagged sums around `n`, outermost first, and merges in what they
    /// contribute.
    fn settle(&mut self, n: NodeId) -> R<()> {
        loop {
            let mut outermost = None;
            let mut current = Some(n);
            while let Some(c) = current {
                if self
                    .merge
                    .node(c)
                    .pending
                    .iter()
                    .any(|&id| !self.covered_path(c, n, self.merge.contrib(id).prio))
                {
                    outermost = Some(c);
                }
                current = self.merge.node(c).parent;
            }
            let Some(c) = outermost else { return Ok(()) };
            let mut pending = std::mem::take(&mut self.merge.node_mut(c).pending);
            pending.sort_by_key(|&id| std::cmp::Reverse(self.merge.contrib(id).prio));
            for id in pending {
                let contrib = self.merge.contrib(id).clone();
                if self.replaced(c, contrib.prio) {
                    self.merge.set_kind(id, Kind::Spent);
                } else if let Kind::Deferred(deferred) = &contrib.kind {
                    let ty = self.merge.node(c).ty;
                    let types = &self.c.types;
                    let inner = types.unwrap_option(ty).unwrap_or_else(|| types.peel(ty));
                    if deferred.is_late()
                        && (types.unwrap_option(ty).is_some()
                            || matches!(
                                types.get(inner),
                                Type::Struct(_) | Type::Sum(_) | Type::Data
                            ))
                    {
                        self.replace_lower(c, contrib.prio);
                    }
                    let out = self.run_deferred(deferred, &contrib)?;
                    self.merge.set_kind(id, Kind::Spent);
                    for p in out {
                        if p.path.is_empty()
                            && matches!(p.contrib.kind, Kind::Value(Val::None))
                            && self.c.types.unwrap_option(self.merge.node(c).ty).is_some()
                        {
                            let ids = self
                                .merge
                                .node(c)
                                .contribs
                                .iter()
                                .copied()
                                .chain(self.merge.below(c))
                                .collect::<Vec<_>>();
                            for other in ids {
                                if self.merge.contrib(other).prio <= contrib.prio {
                                    self.merge.set_kind(other, Kind::Spent);
                                }
                            }
                        }
                        self.insert_generated(c, p);
                    }
                }
                let ty = self.merge.node(c).ty;
                match self.merge.node(c).shape {
                    Shape::Payload(s, v) => self.payload(c, s, v)?,
                    _ => self.structure(c, ty)?,
                }
            }
        }
    }

    fn run_deferred(&mut self, deferred: &Deferred, template: &Contrib) -> R<Vec<Pending>> {
        let mut frames = Vec::with_capacity(deferred.frames.len());
        for path in &deferred.frames {
            match self.merge.resolve(path) {
                Some(node) => frames.push(Frame::Node(node)),
                None => {
                    return Err(self.bug(
                        template.file,
                        template.span,
                        "a block of `self` is missing",
                    ));
                }
            }
        }
        self.evaluate_deferred(deferred, &frames, template)
    }

    /// Merges the winning contributions of an attribute that is neither a
    /// struct, a map nor a tagged sum.
    fn leaf(&mut self, n: NodeId) -> R<Option<Val>> {
        let winners = self.merge.node(n).winners.clone();
        if winners.is_empty() {
            return Ok(None);
        }
        for &id in &winners {
            let contrib = self.merge.contrib(id).clone();
            let Kind::Deferred(deferred) = &contrib.kind else {
                continue;
            };
            let out = self.run_deferred(deferred, &contrib)?;
            let (value, parts) = out
                .into_iter()
                .find(|p| p.path.is_empty())
                .and_then(|p| match p.contrib.kind {
                    Kind::Value(v) => Some((v, p.contrib.parts)),
                    _ => None,
                })
                .unwrap_or((Val::None, None));
            self.merge.set_kind(id, Kind::Value(value));
            self.merge.contribs[id as usize].parts = parts;
        }
        let values: Vec<(Val, u32)> = winners
            .iter()
            .filter_map(|&id| match &self.merge.contrib(id).kind {
                Kind::Value(v) => Some((v.clone(), id)),
                _ => None,
            })
            .collect();
        let ty = self.merge.node(n).ty;
        let types = &self.c.types;
        let inner = types.unwrap_option(ty).unwrap_or_else(|| types.peel(ty));
        let at = self.node_at(n);
        if values.iter().all(|(v, _)| v.is_none()) {
            return Ok(Some(Val::None));
        }
        match self.c.types.get(inner) {
            Type::Data => self.merge_data(n, &mut Vec::new(), values, at).map(Some),
            Type::List(_) | Type::Set(_) if values.iter().all(|(v, _)| !v.is_none()) => {
                let items: Vec<Val> = values
                    .iter()
                    .flat_map(|(v, _)| v.items().iter().cloned())
                    .collect();
                if matches!(self.c.types.get(inner), Type::Set(_)) {
                    self.new_set(items, at).map(Some)
                } else {
                    self.new_list(items, at).map(Some)
                }
            }
            _ => Ok(Some(self.scalar(n, &[], values))),
        }
    }

    /// Returns the value of equal winners, or reports `E0501` and returns
    /// the first.
    fn scalar(&mut self, n: NodeId, keys: &[String], values: Vec<(Val, u32)>) -> Val {
        let Some(first) = values.first().map(|(v, _)| v.clone()) else {
            return Val::None;
        };
        if values.iter().all(|(v, _)| v.equals(&first)) {
            return first;
        }
        let ids: Vec<u32> = values.iter().map(|(_, id)| *id).collect();
        let shown: Vec<Val> = values.into_iter().map(|(v, _)| v).collect();
        self.report_conflict(n, keys, &ids, Some(&shown), "E0501");
        first
    }

    /// Merges `Data` values by their shape: maps and structs per key,
    /// lists by concatenation, sets by union, anything else as a scalar.
    fn merge_data(
        &mut self,
        n: NodeId,
        keys: &mut Vec<String>,
        values: Vec<(Val, u32)>,
        at: SourceSpan,
    ) -> R<Val> {
        let shape = |v: &Val| match v {
            Val::Map(_) | Val::Struct(_) => 0,
            Val::List(_) => 1,
            Val::Set(_) => 2,
            _ => 3,
        };
        let Some(first) = values.first().map(|(v, _)| shape(v)) else {
            return Ok(Val::None);
        };
        if values.iter().any(|(v, _)| shape(v) != first) {
            let ids: Vec<u32> = values.iter().map(|(_, id)| *id).collect();
            let shown: Vec<Val> = values.into_iter().map(|(v, _)| v).collect();
            self.report_conflict(n, keys, &ids, Some(&shown), "E0503");
            return Ok(Val::None);
        }
        match first {
            0 => {
                if let Some((Val::Struct(s), _)) = values.first()
                    && values
                        .iter()
                        .all(|(v, _)| matches!(v, Val::Struct(o) if o.id == s.id))
                {
                    let id = s.id;
                    let count = s.fields.len();
                    let mut fields = Vec::with_capacity(count);
                    for i in 0..count {
                        let name = self.c.types.structure(id).fields[i].name.clone();
                        let column = values
                            .iter()
                            .filter_map(|(v, c)| match v {
                                Val::Struct(s) => s.fields.get(i).map(|f| (f.clone(), *c)),
                                _ => None,
                            })
                            .collect();
                        keys.push(name);
                        let merged = self.merge_data(n, keys, column, at);
                        keys.pop();
                        fields.push(merged?);
                    }
                    return self.new_struct(id, fields, at);
                }
                let mut grouped: BTreeMap<Key, Vec<(Val, u32)>> = BTreeMap::new();
                for (value, id) in &values {
                    let entries = self.entries(value);
                    self.charge(u64::try_from(entries.len()).unwrap_or(u64::MAX), at)?;
                    for (key, item) in entries {
                        grouped.entry(Key(key)).or_default().push((item, *id));
                    }
                }
                let mut entries = Vec::with_capacity(grouped.len());
                for (Key(key), items) in grouped {
                    keys.push(key.text(&self.c.types));
                    let merged = self.merge_data(n, keys, items, at);
                    keys.pop();
                    entries.push((key, merged?));
                }
                self.new_map(entries, at)
            }
            1 => {
                let items = values
                    .iter()
                    .flat_map(|(v, _)| v.items().iter().cloned())
                    .collect();
                self.new_list(items, at)
            }
            2 => {
                let items = values
                    .iter()
                    .flat_map(|(v, _)| v.items().iter().cloned())
                    .collect();
                self.new_set(items, at)
            }
            _ => Ok(self.scalar(n, keys, values)),
        }
    }

    /// Returns the entries of a map, or the fields of a struct keyed by
    /// their names.
    fn entries(&self, value: &Val) -> Vec<(Val, Val)> {
        match value {
            Val::Map(m) => m.entries.clone(),
            Val::Struct(s) => self
                .c
                .types
                .structure(s.id)
                .fields
                .iter()
                .zip(&s.fields)
                .map(|(f, v)| (Val::Str(f.name.as_str().into()), v.clone()))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn conflict(&mut self, n: NodeId, ids: &[u32], code: &'static str) {
        self.report_conflict(n, &[], ids, None, code);
    }

    /// Reports contributions of equal priority that do not agree, citing
    /// every one of them.
    fn report_conflict(
        &mut self,
        n: NodeId,
        keys: &[String],
        ids: &[u32],
        values: Option<&[Val]>,
        code: &'static str,
    ) {
        let mut path = self.attr_path(n);
        for key in keys {
            path = path.join(key.clone());
        }
        let Some(&first) = ids.first() else { return };
        let primary = self.merge.contrib(first).at();
        let (message, help) = match code {
            "E0502" => (
                format!("tagged blocks select different variants of `{path}`"),
                "keep the blocks of one variant, or select the variant with `force(...)`",
            ),
            "E0504" => (
                format!(
                    "a late whole value for `{path}` has other contributions at equal or lower priority"
                ),
                "remove the other contributions, raise their priority, or compute the whole value without `self`",
            ),
            "E0503" => (
                format!("`Data` values of different shapes for `{path}`"),
                "a higher priority contribution replaces the others: mark one with `force(...)`, or the others with `default(...)`",
            ),
            _ => (
                format!("conflicting values for `{path}`"),
                "keep one of them, or mark the one that should win with `force(...)`",
            ),
        };
        let mut diagnostic = Diagnostic::error(code, primary.span, message).with_help(help);
        for (i, &id) in ids.iter().enumerate() {
            let contrib = self.merge.contrib(id).clone();
            if code == "E0504"
                && matches!(contrib.kind, Kind::Present)
                && ids.iter().any(|&other| {
                    let other = self.merge.contrib(other);
                    other.file == contrib.file
                        && other.span.start >= contrib.span.start
                        && other.span.end <= contrib.span.end
                        && (other.span != contrib.span
                            || matches!(
                                other.kind,
                                Kind::Value(_) | Kind::Deferred(_) | Kind::Select(_)
                            ))
                })
            {
                continue;
            }
            let what = match (values.and_then(|v| v.get(i)), &contrib.kind) {
                (Some(value), _) | (None, Kind::Value(value)) => self.describe(value, code),
                (None, Kind::Select(v)) => {
                    let name = self.variant_name(n, *v);
                    format!("`{name}`")
                }
                (None, Kind::Present | Kind::Spent) => "present".to_owned(),
                (None, Kind::Deferred(_)) => "a value that reads `self`".to_owned(),
            };
            let mut label = format!("{what} ({})", contrib.prio.name());
            if let Some(site) = contrib.via.as_ref().and_then(|v| v.first()) {
                label.push_str(&format!(", included from {}", self.location(*site)));
            }
            diagnostic = diagnostic.with_label(Some(contrib.file), contrib.span, label);
        }
        self.errors.push(FileDiagnostic {
            file: Some(primary.file),
            diagnostic,
        });
    }

    fn describe(&self, value: &Val, code: &str) -> String {
        if code == "E0503" {
            return match value {
                Val::Map(_) | Val::Struct(_) => "a map",
                Val::List(_) => "a list",
                Val::Set(_) => "a set",
                _ => "a scalar",
            }
            .to_owned();
        }
        value
            .to_value(&self.c.types, &self.c.import_roots)
            .describe()
    }

    fn variant_name(&self, n: NodeId, variant: u32) -> String {
        let ty = self.merge.node(n).ty;
        let types = &self.c.types;
        let inner = types.unwrap_option(ty).unwrap_or_else(|| types.peel(ty));
        types
            .as_sum(inner)
            .and_then(|s| types.sum(s).variants.get(variant as usize))
            .map_or_else(|| variant.to_string(), |v| v.name.clone())
    }

    /// Returns `file:line:column` of a place.
    pub(crate) fn location(&self, at: SourceSpan) -> String {
        let file = self.c.program.file(at.file);
        let text = file.parsed.tree.root().text();
        let before = text.get(..at.span.start).unwrap_or(text);
        let line = before.matches('\n').count() + 1;
        let start = before.rfind('\n').map_or(0, |i| i + 1);
        let column = before[start..].chars().count() + 1;
        format!("{}:{line}:{column}", file.name)
    }

    /// Returns the name of the step from the parent of `n` to `n`.
    pub(crate) fn key_name(&self, n: NodeId) -> String {
        let node = self.merge.node(n);
        let Some(parent) = node.parent else {
            return String::new();
        };
        let types = &self.c.types;
        let parent_node = self.merge.node(parent);
        match &node.seg {
            Some(Seg::Key(k)) => k.to_string(),
            Some(Seg::Placeholder(i)) => format!("#{i}"),
            Some(Seg::Variant(v)) => self.variant_name(parent, *v),
            Some(Seg::Field(i)) => {
                let i = *i as usize;
                let name = match parent_node.shape {
                    Shape::Payload(s, v) => types
                        .sum(s)
                        .variants
                        .get(v as usize)
                        .and_then(|v| v.fields.as_ref())
                        .and_then(|f| f.get(i))
                        .map(|p| p.name.clone()),
                    _ => {
                        let ty = parent_node.ty;
                        let inner = types.unwrap_option(ty).unwrap_or_else(|| types.peel(ty));
                        types
                            .as_struct(inner)
                            .and_then(|s| types.structure(s).fields.get(i))
                            .map(|f| f.name.clone())
                    }
                };
                name.unwrap_or_else(|| format!("#{i}"))
            }
            None => String::new(),
        }
    }

    pub(crate) fn attr_path(&self, n: NodeId) -> AttrPath {
        let mut keys = Vec::new();
        let mut current = Some(n);
        while let Some(c) = current {
            if c != NodeId::ROOT {
                keys.push(self.key_name(c));
            }
            current = self.merge.node(c).parent;
        }
        keys.reverse();
        AttrPath::new(keys)
    }

    fn display_path(&self, n: NodeId) -> String {
        if self.merge.graph_only && n != NodeId::ROOT {
            let mut keys = Vec::new();
            let mut current = Some(n);
            while let Some(c) = current {
                if c != NodeId::ROOT {
                    keys.push(match self.merge.node(c).seg {
                        Some(Seg::Placeholder(i)) => format!("#{i}"),
                        _ => AttrPath::new(vec![self.key_name(c)]).to_string(),
                    });
                }
                current = self.merge.node(c).parent;
            }
            keys.reverse();
            return keys.join(".");
        }
        let path = self.attr_path(n);
        if path.keys().is_empty() {
            "the root".to_owned()
        } else {
            path.to_string()
        }
    }

    /// Checks computed-entry instances in a disposable trie, never in the value
    /// being merged. Only participating literals can introduce placeholders.
    fn check_cycles(&mut self, top: NodeId, fallback_owners: &BTreeSet<NodeId>) -> R<Vec<NodeId>> {
        let mut pending = Vec::new();
        for n in self.merge.reached(top) {
            pending.extend(self.computed_literals(n).into_iter().map(|id| (n, id)));
        }
        if pending.is_empty() {
            return self.dependency_order(top, fallback_owners);
        }
        let graph = self.merge.clone();
        let original = std::mem::replace(&mut self.merge, graph);
        let original_len = original.nodes.len();
        self.merge.graph_only = true;
        self.merge.fallback_creators = Some(RefCell::new(BTreeMap::new()));
        let result = (|| {
            let mut expanded = BTreeSet::new();
            while let Some((n, id)) = pending.pop() {
                if !expanded.insert(id) {
                    continue;
                }
                let contrib = self.merge.contrib(id).clone();
                let Kind::Deferred(deferred) = &contrib.kind else {
                    continue;
                };
                let types = &self.c.types;
                let inner = types
                    .unwrap_option(deferred.ty)
                    .unwrap_or_else(|| types.peel(deferred.ty));
                let Type::Map(_, value_ty) = *types.get(inner) else {
                    continue;
                };
                let out = self.computed_instances(deferred, &contrib)?;
                let mut instances = BTreeSet::new();
                for p in out {
                    let Some(seg) = p.path.first() else { continue };
                    let instance = self.merge.child_or_new(n, seg.clone());
                    instances.insert(instance);
                    self.merge.insert(n, &p.path, p.contrib);
                }
                self.merge.static_checked = false;
                for instance in instances {
                    self.structure(instance, value_ty)?;
                    for child in self.merge.reached(instance) {
                        pending.extend(
                            self.computed_literals(child)
                                .into_iter()
                                .map(|id| (child, id)),
                        );
                    }
                }
                self.merge.static_checked = true;
            }
            let mut owners: BTreeSet<NodeId> = self
                .merge
                .nodes
                .iter()
                .filter(|node| !node.checks.is_empty())
                .filter_map(|node| node.parent)
                .collect();
            self.merge.fallback_creators = None;
            let mut creators = BTreeMap::new();
            owners.retain(|&owner| {
                self.potential_fallback_with(owner, |boundary| {
                    *creators
                        .entry(boundary)
                        .or_insert_with(|| self.fallback_creator(boundary))
                })
            });
            self.dependency_order(top, &owners)
        })();
        self.merge = original;
        result.map(|order| {
            order
                .into_iter()
                .filter(|n| n.index() < original_len)
                .collect()
        })
    }

    /// Returns participating computed-key literals at one graph node.
    fn computed_literals(&self, n: NodeId) -> Vec<u32> {
        let node = self.merge.node(n);
        node.pending.iter().chain(&node.winners).chain(&node.checks)
            .copied()
            .filter(|&id| {
                matches!(&self.merge.contrib(id).kind, Kind::Deferred(d) if d.form == DeferredForm::MapLiteral)
                    && !self.declared_replaced(n, id)
                    && !self.covered_path(n, n, self.merge.contrib(id).prio)
                    && (!node.checks.contains(&id)
                        || node.parent.is_some_and(|owner| self.potential_fallback(owner)))
            })
            .collect()
    }

    /// Reports cycles among the deferred contributions the merge may evaluate
    /// at and inside `top`, and stops when there is one. Otherwise returns the
    /// attributes in an order where each comes after the attributes it
    /// depends on. A shared static fallback-owner snapshot avoids rescanning
    /// each owner per field. Finalizing in dependency order never recurses
    /// along a long chain of dependencies.
    fn dependency_order(
        &mut self,
        top: NodeId,
        fallback_owners: &BTreeSet<NodeId>,
    ) -> R<Vec<NodeId>> {
        let nodes = self.merge.reached(top);
        let mut index = BTreeMap::new();
        for (i, &n) in nodes.iter().enumerate() {
            index.insert(n, i);
        }
        let mut edges: Vec<Vec<(usize, Option<u32>)>> = vec![Vec::new(); nodes.len()];
        for (i, &n) in nodes.iter().enumerate() {
            for c in self.merge.inside(n) {
                if let Some(&j) = index.get(&c) {
                    edges[i].push((j, None));
                }
            }
            let node = self.merge.node(n);
            let mut leaf_deferred = node.winners.clone();
            if node
                .parent
                .is_some_and(|owner| fallback_owners.contains(&owner))
                && !self.covered(n, Prio::Declared)
            {
                leaf_deferred.extend(node.checks.iter().copied());
            }
            let structural: Vec<u32> = node.pending.clone();
            for id in leaf_deferred {
                for target in self.read_targets(id) {
                    if let Some(&j) = index.get(&target) {
                        edges[i].push((j, Some(id)));
                    }
                }
            }
            if !structural.is_empty() {
                let below = self.merge.reached(n);
                for id in structural {
                    let targets = self.read_targets(id);
                    let computed_map = matches!(
                        &self.merge.contrib(id).kind,
                        Kind::Deferred(d) if d.form == DeferredForm::MapLiteral
                    );
                    for &m in &below {
                        if computed_map && m != n {
                            continue;
                        }
                        if self.covered_path(n, m, self.merge.contrib(id).prio) {
                            continue;
                        }
                        let Some(&k) = index.get(&m) else { continue };
                        for &target in &targets {
                            if let Some(&j) = index.get(&target) {
                                edges[k].push((j, Some(id)));
                            }
                        }
                    }
                }
            }
        }
        let adj: Vec<Vec<usize>> = edges
            .iter()
            .map(|list| {
                let mut targets: Vec<usize> = list.iter().map(|(t, _)| *t).collect();
                targets.sort_unstable();
                targets.dedup();
                targets
            })
            .collect();
        let mut found: Vec<FileDiagnostic> = Vec::new();
        let components = graph::components(&adj);
        for cycle in graph::cycles_of(&adj, &components) {
            let mut diagnostic: Option<(FileId, Diagnostic)> = None;
            for (k, &from) in cycle.iter().enumerate() {
                let to = cycle[(k + 1) % cycle.len()];
                let Some(id) = edges[from]
                    .iter()
                    .find(|(t, c)| *t == to && c.is_some())
                    .and_then(|(_, c)| *c)
                else {
                    continue;
                };
                let contrib = self.merge.contrib(id).clone();
                let message = format!(
                    "`{}` reads `{}`",
                    self.display_path(nodes[from]),
                    self.display_path(nodes[to])
                );
                diagnostic = Some(match diagnostic {
                    None => (
                        contrib.file,
                        Diagnostic::error(
                            "E0607",
                            contrib.span,
                            "`self` dependencies form a cycle",
                        )
                        .with_label(
                            Some(contrib.file),
                            contrib.span,
                            message,
                        ),
                    ),
                    Some((file, d)) => (
                        file,
                        d.with_label(Some(contrib.file), contrib.span, message),
                    ),
                });
            }
            let Some((file, diagnostic)) = diagnostic else {
                continue;
            };
            let mut chain: Vec<String> =
                cycle.iter().map(|&k| self.display_path(nodes[k])).collect();
            chain.push(self.display_path(nodes[cycle[0]]));
            found.push(FileDiagnostic {
                file: Some(file),
                diagnostic: diagnostic.with_note(format!("cycle: {}", chain.join(" -> "))),
            });
        }
        if found.is_empty() {
            return Ok(components.into_iter().flatten().map(|i| nodes[i]).collect());
        }
        let first = found.remove(0);
        self.errors.extend(found);
        Err(Stop(Box::new(first)))
    }

    /// Returns the attributes a deferred contribution reads through
    /// `self`.
    fn read_targets(&self, id: u32) -> Vec<NodeId> {
        let Kind::Deferred(deferred) = &self.merge.contrib(id).kind else {
            return Vec::new();
        };
        deferred
            .reads
            .iter()
            .filter_map(|read| {
                let frame = deferred.frames.get(read.depth)?;
                let block = self.merge.resolve(frame)?;
                self.merge.child(block, &field_seg(read.field))
            })
            .collect()
    }

    /// Returns the `self` reads of a field default.
    pub(crate) fn default_reads(&mut self, s: StructId, field: usize) -> Rc<Vec<SelfRead>> {
        if let Some(found) = self.caches.reads.get(&(s, field)) {
            return found.clone();
        }
        let def = self.c.types.structure(s);
        let mut reads = Vec::new();
        if let (Some(node), Some(unit)) = (
            def.fields.get(field).and_then(|f| f.default),
            self.file_unit(def.file),
        ) && let crate::check::model::Fact::Expr(fact) = self.fact(unit, node)
        {
            reads = fact.reads.clone();
        }
        let reads = Rc::new(reads);
        self.caches.reads.insert((s, field), reads.clone());
        reads
    }

    /// Returns the merged value of an attribute once it is final.
    pub(crate) fn final_value(&self, n: NodeId) -> Option<Val> {
        match &self.merge.node(n).state {
            State::Done(v) => Some(v.clone()),
            _ => None,
        }
    }

    pub(crate) fn node_type(&self, n: NodeId) -> TypeId {
        self.merge.node(n).ty
    }

    /// Returns the attributes whose values make up the value of `n`.
    pub(crate) fn inside_nodes(&self, n: NodeId) -> Vec<NodeId> {
        self.merge.inside(n)
    }

    /// Returns where each attribute of the final value came from.
    pub(crate) fn walk_spans(&self) -> SpanTree {
        let mut tree = SpanTree::default();
        let mut stack = vec![(NodeId::ROOT, 0usize)];
        tree.nodes.push(SpanNode::default());
        while let Some((n, slot)) = stack.pop() {
            let node = self.merge.node(n);
            let mut ids: Vec<u32> = node.contribs.clone();
            ids.sort_by_key(|&id| {
                let c = self.merge.contrib(id);
                (std::cmp::Reverse(c.prio), c.seq)
            });
            let mut spans: Vec<SourceSpan> = Vec::new();
            for id in ids {
                let at = self.merge.contrib(id).at();
                if !spans.contains(&at) {
                    spans.push(at);
                }
            }
            tree.nodes[slot].spans = spans;
            let prio = node
                .contribs
                .iter()
                .map(|&id| self.merge.contrib(id).prio)
                .max();
            tree.nodes[slot].prio = prio;
            let mut children = Vec::new();
            for c in self.merge.inside(n) {
                let key = self.key_name(c);
                let index = tree.nodes.len();
                tree.nodes.push(SpanNode::default());
                children.push((key, index));
                stack.push((c, index));
            }
            if let Shape::Leaf = node.shape {
                children.extend(self.leaf_spans(&mut tree, n, prio));
            }
            if let Shape::Tagged(Some(_)) = node.shape {
                tree.nodes[slot].transparent = true;
            }
            children.sort();
            tree.nodes[slot].children = children;
        }
        tree
    }
}

/// A place that wrote a value: its span, and where the parts inside it
/// were written when a literal recorded them.
#[derive(Debug, Clone, Copy)]
struct Origin<'a> {
    at: SourceSpan,
    parts: Option<&'a Parts>,
}

impl<'a> Origin<'a> {
    fn new(file: FileId, written: &'a Written) -> Self {
        Self {
            at: SourceSpan {
                file,
                span: written.span,
            },
            parts: written.parts.as_deref(),
        }
    }
}

impl Eval<'_> {
    /// Adds the span nodes inside the attribute `n` that holds a whole
    /// value, and returns them keyed for its node.
    ///
    /// The elements of a list concatenate the winning contributions. Each
    /// element has the span of the element of the list literal that wrote
    /// it, or else of the contribution it came from. An element of a set
    /// has the spans of every element equal to it. Inside an element, and
    /// inside any other value, the fields, keys, elements and variant
    /// payloads that literals wrote have the spans of their entries,
    /// arguments, elements and calls.
    fn leaf_spans(
        &self,
        tree: &mut SpanTree,
        n: NodeId,
        prio: Option<Prio>,
    ) -> Vec<(String, usize)> {
        let Some(value) = self.final_value(n) else {
            return Vec::new();
        };
        let winners = self.merge.node(n).winners.iter().filter_map(|&id| {
            let c = self.merge.contrib(id);
            match &c.kind {
                Kind::Value(v) => Some((c, v)),
                _ => None,
            }
        });
        let (set, items) = match &value {
            Val::List(s) => (false, s),
            Val::Set(s) => (true, s),
            _ => {
                let origins: Vec<Origin<'_>> = winners
                    .map(|(c, _)| Origin {
                        at: c.at(),
                        parts: c.parts.as_deref(),
                    })
                    .collect();
                return self.add_spans(tree, self.inner_origins(&value, &origins), prio);
            }
        };
        let mut written = Vec::new();
        for (c, v) in winners {
            match c.parts.as_deref() {
                Some(Parts::Elements(elements)) if set || elements.len() == v.items().len() => {
                    written.extend(elements.iter().map(|(v, w)| (v, Origin::new(c.file, w))));
                }
                _ => written.extend(v.items().iter().map(|v| {
                    let origin = Origin {
                        at: c.at(),
                        parts: None,
                    };
                    (v, origin)
                })),
            }
        }
        let children = element_origins(set, &items.items, vec![written]);
        self.add_spans(tree, children, prio)
    }

    /// Adds a span node for each part of a value with the places that
    /// wrote it, and the nodes inside each, and returns them keyed for
    /// the node of the value. A part that no place wrote gets no node.
    fn add_spans(
        &self,
        tree: &mut SpanTree,
        children: Vec<(String, &Val, Vec<Origin<'_>>)>,
        prio: Option<Prio>,
    ) -> Vec<(String, usize)> {
        let mut out = Vec::new();
        for (key, value, origins) in children {
            if origins.is_empty() {
                continue;
            }
            let mut seen = BTreeSet::new();
            let spans = origins
                .iter()
                .map(|o| o.at)
                .filter(|&at| seen.insert(at))
                .collect();
            let index = tree.nodes.len();
            tree.nodes.push(SpanNode {
                spans,
                prio,
                ..SpanNode::default()
            });
            let mut inner = self.add_spans(tree, self.inner_origins(value, &origins), prio);
            inner.sort();
            tree.nodes[index].children = inner;
            out.push((key, index));
        }
        out
    }

    /// Returns the parts of `value` with the places that wrote each,
    /// given the places that wrote `value`: the elements of a list or
    /// set, the fields of a struct, the entries of a map, and the variant
    /// of a value with payload, whose parts are its payload fields.
    fn inner_origins<'v, 'a>(
        &self,
        value: &'v Val,
        origins: &[Origin<'a>],
    ) -> Vec<(String, &'v Val, Vec<Origin<'a>>)> {
        let types = &self.c.types;
        let named: Vec<(String, &Val)> = match value {
            Val::List(s) | Val::Set(s) => {
                let groups = origins
                    .iter()
                    .filter_map(|o| match o.parts {
                        Some(Parts::Elements(elements)) => Some(
                            elements
                                .iter()
                                .map(|(v, w)| (v, Origin::new(o.at.file, w)))
                                .collect(),
                        ),
                        _ => None,
                    })
                    .collect();
                return element_origins(matches!(value, Val::Set(_)), &s.items, groups);
            }
            Val::Variant(v) => {
                let Some(variant) = types.sum(v.sum).variants.get(v.index as usize) else {
                    return Vec::new();
                };
                let calls: Vec<Origin<'a>> = origins
                    .iter()
                    .filter_map(|o| match o.parts {
                        Some(Parts::Payload(call)) => Some(Origin::new(o.at.file, call)),
                        _ => None,
                    })
                    .collect();
                if !calls.is_empty() {
                    return vec![(variant.name.clone(), value, calls)];
                }
                let params = variant.fields.iter().flatten();
                params.map(|p| p.name.clone()).zip(&v.fields).collect()
            }
            Val::Struct(s) => {
                let fields = &types.structure(s.id).fields;
                fields
                    .iter()
                    .map(|f| f.name.clone())
                    .zip(&s.fields)
                    .collect()
            }
            Val::Map(m) => m.entries.iter().map(|(k, v)| (k.text(types), v)).collect(),
            _ => return Vec::new(),
        };
        named
            .into_iter()
            .map(|(name, inner)| {
                let found = origins
                    .iter()
                    .filter_map(|o| match o.parts {
                        Some(Parts::Entries(entries)) => {
                            let i = entries
                                .binary_search_by(|(k, _)| k.as_str().cmp(&name))
                                .ok()?;
                            Some(Origin::new(o.at.file, &entries[i].1))
                        }
                        _ => None,
                    })
                    .collect();
                (name, inner, found)
            })
            .collect()
    }
}

/// Returns the elements of a list or set, keyed by position, with the
/// places that wrote each. Each group holds the written elements that
/// make up the whole value. An element of a list gets its element of each
/// group as long as the value, and an element of a set every written
/// element equal to it.
fn element_origins<'v, 'a>(
    set: bool,
    items: &'v [Val],
    groups: Vec<Vec<(&Val, Origin<'a>)>>,
) -> Vec<(String, &'v Val, Vec<Origin<'a>>)> {
    let mut found: Vec<Vec<Origin<'a>>> = vec![Vec::new(); items.len()];
    if set {
        let mut written: Vec<(&Val, Origin<'a>)> = groups.into_iter().flatten().collect();
        written.sort_by(|a, b| a.0.order(b.0));
        for (slot, item) in found.iter_mut().zip(items) {
            let start = written.partition_point(|(v, _)| v.order(item).is_lt());
            let equal = written[start..].iter().take_while(|(v, _)| v.equals(item));
            slot.extend(equal.map(|(_, o)| *o));
        }
    } else {
        for group in groups.into_iter().filter(|g| g.len() == items.len()) {
            for (slot, (_, origin)) in found.iter_mut().zip(group) {
                slot.push(origin);
            }
        }
    }
    items
        .iter()
        .zip(found)
        .enumerate()
        .map(|(i, (item, origins))| (i.to_string(), item, origins))
        .collect()
}

fn field_seg(i: usize) -> Seg {
    Seg::Field(u32::try_from(i).unwrap_or(u32::MAX))
}

/// Where each attribute of an evaluated value came from.
#[derive(Debug, Clone, Default)]
pub(crate) struct SpanTree {
    nodes: Vec<SpanNode>,
}

#[derive(Debug, Clone, Default)]
struct SpanNode {
    children: Vec<(String, usize)>,
    spans: Vec<SourceSpan>,
    /// The highest priority of the contributions to the attribute.
    prio: Option<Prio>,
    /// A tagged sum, whose one child is named by its variant.
    transparent: bool,
}

impl SpanTree {
    /// Returns the node of the attribute at `path`. When the tree ends
    /// before the path does, `inside` says whether the path still names a
    /// part of the value, which then gets the deepest attribute on the
    /// path.
    fn find(&self, path: &AttrPath, inside: bool) -> Option<&SpanNode> {
        let mut current = self.nodes.first()?;
        for key in path.keys() {
            match current
                .children
                .binary_search_by(|(k, _)| k.as_str().cmp(key))
            {
                Ok(i) => current = &self.nodes[current.children[i].1],
                Err(_) if inside => break,
                Err(_) => return None,
            }
        }
        Some(current)
    }

    /// Returns the nodes inside `node`, in the order of their keys.
    fn below<'t>(&'t self, node: &'t SpanNode) -> impl Iterator<Item = &'t SpanNode> {
        let mut stack: Vec<usize> = node.children.iter().rev().map(|(_, i)| *i).collect();
        std::iter::from_fn(move || {
            let i = stack.pop()?;
            stack.extend(self.nodes[i].children.iter().rev().map(|(_, c)| *c));
            Some(&self.nodes[i])
        })
    }

    /// Returns the spans of the attribute at `path`, as
    /// [`Evaluated::spans`](crate::Evaluated::spans) describes.
    pub(crate) fn spans(&self, path: &AttrPath, inside: bool) -> Vec<SourceSpan> {
        let Some(node) = self.find(path, inside) else {
            return Vec::new();
        };
        let mut out = node.spans.clone();
        if !node.spans.is_empty() && !node.transparent {
            return out;
        }
        for inner in self.below(node) {
            for span in &inner.spans {
                if !out.contains(span) {
                    out.push(*span);
                }
            }
        }
        out
    }

    /// Returns the winning priority of the attribute at `path`, as
    /// [`Evaluated::priority`](crate::Evaluated::priority) describes.
    pub(crate) fn priority(&self, path: &AttrPath, inside: bool) -> Option<Prio> {
        let node = self.find(path, inside)?;
        if node.prio > Some(Prio::Declared) && !node.transparent {
            return node.prio;
        }
        self.below(node)
            .filter_map(|n| n.prio)
            .chain(node.prio)
            .max()
    }
}
