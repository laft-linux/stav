//! What stops an evaluation that needs an opaque field: `E0619`, with the
//! place that needed it and the selected attribute it was needed for.

use std::collections::BTreeMap;

use super::body::{Kind, Prio};
use super::merge::NodeId;
use super::{Eval, Stop};
use crate::check::model::{Read, ReadKind};
use crate::check::types::StructId;
use crate::diagnostic::{Diagnostic, FileDiagnostic};
use crate::span::{SourceSpan, Span};
use crate::value::AttrPath;

/// What the evaluation is doing, for the diagnostics of opaque fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Evaluating the statements of the root and the top-level values.
    Statements,
    /// Evaluating the declared defaults that decide the shape of the root.
    Shapes,
    /// Computing the attributes that the selected attributes need.
    Needs,
}

/// The selected attributes, and why each needed attribute is needed.
#[derive(Debug, Default)]
pub(crate) struct Selection {
    /// The paths of the selected attributes.
    pub(crate) paths: Vec<AttrPath>,
    /// For each needed attribute, the index of the selected attribute that
    /// needs it, and the contribution whose `self` read needed it last on
    /// the way, if any.
    pub(crate) reasons: BTreeMap<NodeId, (usize, Option<SourceSpan>)>,
}

/// How a needed attribute meets an opaque field.
pub(crate) enum Need {
    /// An unchecked contribution that wins at the attribute or may supply
    /// it.
    Contribution(u32),
    /// The attribute is an instance of an opaque field.
    Field,
    /// Whether the attribute, an `Option`, is present depends on an
    /// unchecked contribution: the deciding `none`, if one decides it.
    Presence { none: Option<u32>, unchecked: u32 },
}

impl Eval<'_> {
    /// Returns `E0619` for an unchecked definition that evaluation entered.
    pub(crate) fn entered(&self, read: &Read) -> Stop {
        let at = read_span(read);
        let field = self.field_name(read.field);
        let diagnostic = match self.phase {
            Phase::Needs => {
                let n = self.finalizing.last().copied().unwrap_or(NodeId::ROOT);
                self.needs(n, &field, at.span, false)
            }
            Phase::Shapes => Diagnostic::error(
                "E0619",
                at.span,
                format!("a declared default reads the opaque field `{field}`"),
            )
            .with_note(self.every_evaluation(
                "runs the declared defaults that read no `self` and may fill a field",
            ))
            .with_help(format!(
                "compute the default without `{field}`, which this evaluation leaves opaque"
            )),
            Phase::Statements => Diagnostic::error(
                "E0619",
                at.span,
                format!("a statement reads the opaque field `{field}`"),
            )
            .with_note(self.every_evaluation("runs every statement of the root"))
            .with_help(format!(
                "compute the statement without `{field}`, which this evaluation leaves opaque"
            )),
        };
        self.stop(at, self.read_labels(diagnostic, read))
    }

    /// Returns `E0619` for the attribute `n`, which the evaluation needs.
    pub(crate) fn opaque_need(&self, n: NodeId, need: Need) -> Stop {
        let (_, read_by) = self.reason(n);
        let path = self.display_path(n);
        let (read, field) = match need {
            Need::Contribution(id) | Need::Presence { unchecked: id, .. } => {
                let read = match &self.merge.contrib(id).kind {
                    Kind::Unchecked(u) => Some(u.read.as_ref()),
                    _ => None,
                };
                (read, read.map(|r| r.field))
            }
            Need::Field => (self.first_opaque_contribution(n), self.node_field(n)),
        };
        let field = field.map(|f| self.field_name(f)).unwrap_or_default();
        if let Need::Presence { none, unchecked } = need {
            let contrib = self.merge.contrib(unchecked).at();
            let Some(none) = none else {
                let at = read.map_or(contrib, read_span);
                let diagnostic = self
                    .needs(n, &field, at.span, true)
                    .with_note(format!("the unchecked value of `{path}` may be `none`"));
                let diagnostic = match read {
                    Some(read) => self.read_labels(diagnostic, read),
                    None => diagnostic,
                };
                return self.stop(at, diagnostic);
            };
            let at = self.merge.contrib(none).at();
            let diagnostic = self
                .needs(n, &field, at.span, true)
                .with_label(
                    Some(at.file),
                    at.span,
                    format!("this `none` competes with a contribution to `{field}`"),
                )
                .with_label(
                    Some(contrib.file),
                    contrib.span,
                    format!("this contributes to `{field}`, so its priority is not known"),
                );
            return self.stop(at, diagnostic);
        }
        let at = match (read_by, read) {
            (Some(by), _) => by,
            (None, Some(read)) => read_span(read),
            (None, None) => {
                let (file, span) = self.opaque_declaration(n);
                SourceSpan { file, span }
            }
        };
        let mut diagnostic = self.needs(n, &field, at.span, false);
        if read_by.is_some() {
            diagnostic = diagnostic.with_label(
                Some(at.file),
                at.span,
                format!("this needs `{path}` through `self`"),
            );
        }
        diagnostic = match read {
            Some(read) => self.read_labels(diagnostic, read),
            None => diagnostic.with_label(
                Some(at.file),
                at.span,
                format!("`{field}` is declared here"),
            ),
        };
        self.stop(at, diagnostic)
    }

    /// Returns the start of `E0619` for an opaque field that the attribute
    /// `n` needs, or that its presence needs, with the selected attribute
    /// it is needed for.
    fn needs(&self, n: NodeId, field: &str, span: Span, presence: bool) -> Diagnostic {
        let (selected, _) = self.reason(n);
        let selected = self
            .selection
            .paths
            .get(selected)
            .cloned()
            .unwrap_or_else(AttrPath::root);
        let root = selected.keys().is_empty();
        let target = if root {
            "the root".to_owned()
        } else {
            format!("`{selected}`")
        };
        let mut diagnostic = Diagnostic::error(
            "E0619",
            span,
            format!("{target} needs the opaque field `{field}`"),
        );
        if presence || self.attr_path(n) != selected {
            let path = self.display_path(n);
            let what = if presence {
                format!("whether `{path}` is present")
            } else {
                format!("`{path}`")
            };
            let target = if root {
                "the root".to_owned()
            } else {
                format!("the selected attribute `{selected}`")
            };
            diagnostic = diagnostic.with_note(format!("{what} is needed for {target}"));
        }
        let help = if root {
            format!(
                "select attributes that do not need `{field}`, or evaluate the root without opaque fields"
            )
        } else {
            format!("compute `{selected}` without `{field}`, which this evaluation leaves opaque")
        };
        diagnostic.with_help(help)
    }

    /// Returns a note that says that every evaluation does what `what`
    /// says, whichever attributes it selects.
    fn every_evaluation(&self, what: &str) -> String {
        let paths: Vec<String> = self
            .selection
            .paths
            .iter()
            .filter(|p| !p.keys().is_empty())
            .map(|p| format!("`{p}`"))
            .collect();
        if paths.is_empty() || paths.len() < self.selection.paths.len() {
            return format!("every evaluation {what}");
        }
        format!(
            "every evaluation {what}, also one that selects only {}",
            paths.join(", ")
        )
    }

    /// Adds the label of a read, and the labels of the reads of the
    /// bindings it goes through, to `diagnostic`.
    fn read_labels(&self, mut diagnostic: Diagnostic, read: &Read) -> Diagnostic {
        let mut current = Some(read);
        let mut depth = 0;
        while let Some(read) = current {
            let field = self.field_name(read.field);
            let (message, next) = match &read.kind {
                ReadKind::Field => (format!("`{field}` is read here"), None),
                ReadKind::Construction(ty) => (
                    format!("this builds a value of `{ty}`, which holds `{field}`"),
                    None,
                ),
                ReadKind::Binding(name, inner) => {
                    (format!("`{name}` reads `{field}`"), Some(inner.as_ref()))
                }
                ReadKind::HostType(ty) => (format!("the type `{ty}` holds `{field}`"), None),
                ReadKind::Contribution => (format!("this contributes to `{field}`"), None),
            };
            diagnostic = diagnostic.with_label(Some(read.file), read.span, message);
            depth += 1;
            current = next.filter(|_| depth < 8);
        }
        diagnostic
    }

    /// Returns the selected attribute that needs `n`, and the contribution
    /// whose `self` read made it needed.
    fn reason(&self, n: NodeId) -> (usize, Option<SourceSpan>) {
        let mut current = Some(n);
        while let Some(c) = current {
            if let Some(&reason) = self.selection.reasons.get(&c) {
                return reason;
            }
            current = self.merge.parent(c);
        }
        (0, None)
    }

    /// Returns the opaque field as `Struct.field`.
    pub(crate) fn field_name(&self, (s, field): (StructId, usize)) -> String {
        let def = self.c.types.structure(s);
        let name = def.fields.get(field).map_or("", |f| f.name.as_str());
        format!("{}.{name}", def.name)
    }

    /// Returns what the first contribution to an opaque field other than
    /// its declared default writes.
    fn first_opaque_contribution(&self, n: NodeId) -> Option<&Read> {
        self.merge
            .contribs_at(n)
            .iter()
            .map(|&id| self.merge.contrib(id))
            .filter_map(|c| match &c.kind {
                Kind::Unchecked(u) if c.prio != Prio::Declared => Some((c.seq, u.read.as_ref())),
                _ => None,
            })
            .min_by_key(|(seq, _)| *seq)
            .map(|(_, read)| read)
    }
}

/// Returns the place of a read.
fn read_span(read: &Read) -> SourceSpan {
    SourceSpan {
        file: read.file,
        span: read.span,
    }
}

/// Returns `E0805` for a selected path that names no field.
pub(crate) fn no_field(message: String, help: Option<String>) -> Box<FileDiagnostic> {
    Box::new(FileDiagnostic {
        file: None,
        diagnostic: Diagnostic::error("E0805", Span { start: 0, end: 0 }, message)
            .with_optional_help(help),
    })
}
