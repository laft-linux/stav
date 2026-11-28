//! Opaque fields: the fields a root leaves unchecked, the types that hold
//! them, and the definitions that stop being checked because they read one.
//!
//! A definition is the smallest unit of code that a read makes unchecked:
//! a right-hand side, an entry of a split literal, a `let` value, a
//! function body with its parameter defaults, a default, or the head of a
//! statement. Checking a definition records a mark first. When a read
//! turns up inside it, everything the definition added since the mark is
//! taken back: its diagnostics, its call graph edges, its `self` graph
//! edges and the part files and predicates it queued. The definition keeps
//! only the first read, which the evaluator reports if it ever reaches it.

use std::collections::BTreeMap;
use std::rc::Rc;

use super::host::FieldName;
use super::model::{Priority, Read, ReadKind};
use super::suggest::did_you_mean;
use super::types::{StructId, Type, TypeId};
use super::{Checker, Cx, SelfMode};
use crate::ast::Expr;
use crate::diagnostic::Diagnostic;
use crate::span::Span;

/// What a definition may take back once it turns out to be unchecked.
pub(super) struct Mark {
    diagnostics: usize,
    reads: usize,
    calls: usize,
    edges: Option<usize>,
    ref_uses: usize,
    units: usize,
    parts: usize,
    predicates: usize,
    predicate_args: usize,
    /// The read of the enclosing definition, restored when this one ends.
    outer: Option<Rc<Read>>,
}

impl Checker<'_> {
    /// Finds the structs of the opaque fields of the root. A struct that
    /// does not exist is `E0302`.
    pub(super) fn find_opaque_structs(&mut self, fields: &[FieldName]) {
        for name in fields {
            match self.entry_struct(name.structure()) {
                Some(s) => self
                    .opaque
                    .entry(s)
                    .or_default()
                    .push(name.field().to_owned()),
                None => {
                    let structs: Vec<String> =
                        self.types.structs.iter().map(|s| s.name.clone()).collect();
                    let help = did_you_mean(name.structure(), structs.iter().map(String::as_str));
                    let file = self.program.entries.first().copied();
                    self.report(
                        file,
                        Diagnostic::error(
                            "E0302",
                            Span { start: 0, end: 0 },
                            format!(
                                "the opaque field `{name}` names no struct `{}`",
                                name.structure()
                            ),
                        )
                        .with_optional_help(help),
                    );
                }
            }
        }
    }

    /// Returns whether the root leaves the field `name` of `s` opaque.
    pub(super) fn opaque_requested(&self, s: StructId, name: &str) -> bool {
        self.opaque
            .get(&s)
            .is_some_and(|names| names.iter().any(|n| n == name))
    }

    /// Reports the opaque fields that their struct does not have, with
    /// `E0302`, and finds the types that hold opaque fields.
    pub(super) fn check_opaque_fields(&mut self) {
        let mut errors = Vec::new();
        for (&s, names) in &self.opaque {
            let def = self.types.structure(s);
            for name in names {
                if def.field(name).is_some() {
                    continue;
                }
                let help = did_you_mean(name, def.fields.iter().map(|f| f.name.as_str()));
                errors.push((
                    def.file,
                    Diagnostic::error(
                        "E0302",
                        def.span,
                        format!(
                            "`{}` has no field `{name}`, which the root leaves opaque",
                            def.name
                        ),
                    )
                    .with_optional_help(help),
                ));
            }
        }
        for (file, diagnostic) in errors {
            self.error(file, diagnostic);
        }
        self.find_holders();
    }

    /// Finds, for every struct and sum, the first opaque field that its
    /// values hold.
    fn find_holders(&mut self) {
        if self.opaque.is_empty() {
            return;
        }
        let mut structs = vec![None; self.types.structs.len()];
        let mut sums = vec![None; self.types.sums.len()];
        loop {
            let mut changed = false;
            for (s, def) in self.types.structs.iter().enumerate() {
                if structs[s].is_some() {
                    continue;
                }
                structs[s] = def.fields.iter().enumerate().find_map(|(i, f)| {
                    if f.opaque {
                        Some((StructId::new(s), i))
                    } else {
                        self.held_by(f.ty, &structs, &sums)
                    }
                });
                changed |= structs[s].is_some();
            }
            for (s, def) in self.types.sums.iter().enumerate() {
                if sums[s].is_some() {
                    continue;
                }
                sums[s] = def
                    .variants
                    .iter()
                    .flat_map(|v| v.fields.iter().flatten())
                    .find_map(|f| self.held_by(f.ty, &structs, &sums));
                changed |= sums[s].is_some();
            }
            if !changed {
                break;
            }
        }
        self.holders = (structs, sums);
    }

    fn held_by(
        &self,
        ty: TypeId,
        structs: &[Option<(StructId, usize)>],
        sums: &[Option<(StructId, usize)>],
    ) -> Option<(StructId, usize)> {
        match self.types.get(ty) {
            Type::Struct(s) => structs.get(s.index()).copied().flatten(),
            Type::Sum(s) => sums.get(s.index()).copied().flatten(),
            Type::Option(t) | Type::List(t) | Type::Set(t) | Type::Refined(t, _) => {
                self.held_by(*t, structs, sums)
            }
            Type::Map(k, v) => self
                .held_by(*k, structs, sums)
                .or_else(|| self.held_by(*v, structs, sums)),
            _ => None,
        }
    }

    /// Returns the first opaque field that a value of type `ty` holds.
    pub(super) fn held(&self, ty: TypeId) -> Option<(StructId, usize)> {
        let (structs, sums) = &self.holders;
        if structs.is_empty() {
            return None;
        }
        self.held_by(ty, structs, sums)
    }

    /// Returns the opaque field that values of the struct `s` hold.
    pub(super) fn held_by_struct(&self, s: StructId) -> Option<(StructId, usize)> {
        self.holders.0.get(s.index()).copied().flatten()
    }

    /// Returns whether the field `field` of `s` is opaque.
    pub(super) fn is_opaque(&self, s: StructId, field: usize) -> bool {
        self.types
            .structure(s)
            .fields
            .get(field)
            .is_some_and(|f| f.opaque)
    }

    /// Records a read of an opaque field in the definition being checked,
    /// unless an earlier read did.
    pub(super) fn read(
        &mut self,
        cx: &mut Cx,
        span: Span,
        field: (StructId, usize),
        kind: ReadKind,
    ) {
        if cx.unchecked.is_none() {
            cx.unchecked = Some(Rc::new(Read {
                file: cx.file,
                span,
                field,
                kind,
            }));
        }
    }

    /// Starts checking a definition.
    pub(super) fn begin_definition(&mut self, cx: &mut Cx) -> Mark {
        Mark {
            diagnostics: self.diagnostics.len(),
            reads: cx.reads.len(),
            calls: self.calls.added.len(),
            edges: match &cx.self_mode {
                SelfMode::Body(body) => Some(body.graph.edge_count()),
                _ => None,
            },
            ref_uses: self.ref_uses.len(),
            units: self.units.len(),
            parts: self.parts.len(),
            predicates: self.pending_predicates.len(),
            predicate_args: self.predicate_args.len(),
            outer: cx.unchecked.take(),
        }
    }

    /// Ends checking a definition, and returns its first read when it read
    /// an opaque field, after taking back what it added since `mark`.
    pub(super) fn end_definition(&mut self, cx: &mut Cx, mark: Mark) -> Option<Rc<Read>> {
        let read = std::mem::replace(&mut cx.unchecked, mark.outer);
        read.as_ref()?;
        cx.reads.truncate(mark.reads);
        for d in self.diagnostics.drain(mark.diagnostics..) {
            self.seen.remove(&(
                d.file,
                d.diagnostic.code,
                d.diagnostic.span,
                d.diagnostic.message,
            ));
        }
        self.calls.truncate(mark.calls);
        if let (SelfMode::Body(body), Some(edges)) = (&mut cx.self_mode, mark.edges) {
            body.graph.truncate(edges);
        }
        self.ref_uses.truncate(mark.ref_uses);
        self.units.truncate(mark.units);
        self.unit_keys.retain(|_, unit| unit.index() < mark.units);
        self.parts.truncate(mark.parts);
        self.pending_predicates.truncate(mark.predicates);
        self.predicate_args.truncate(mark.predicate_args);
        read
    }

    /// Records the use of the binding `name` at `span`, whose value reads
    /// an opaque field as `inner` says, as a read.
    pub(super) fn binding_read(&mut self, cx: &mut Cx, name: &str, span: Span, inner: Rc<Read>) {
        let field = inner.field;
        self.read(cx, span, field, ReadKind::Binding(name.to_owned(), inner));
    }
}

/// Returns the priority that the marker of a right-hand side gives, read
/// from the syntax alone: a call of the bare name `default` or `force` is
/// always a marker.
pub(super) fn written_priority(value: Expr<'_>) -> Priority {
    let Expr::Call(call) = value else {
        return Priority::Plain;
    };
    let Some(Expr::Name(name)) = call.callee() else {
        return Priority::Plain;
    };
    match name.token().map(|t| t.text()) {
        Some("default") => Priority::Default,
        Some("force") => Priority::Force,
        _ => Priority::Plain,
    }
}

/// The holders of opaque fields, by struct and by sum.
pub(super) type Holders = (
    Vec<Option<(StructId, usize)>>,
    Vec<Option<(StructId, usize)>>,
);

/// The opaque fields the root asks for, by struct, before the fields are
/// resolved.
pub(super) type Requested = BTreeMap<StructId, Vec<String>>;
