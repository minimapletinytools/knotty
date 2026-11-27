//! Lashing: when a request snags, add a call around the expression that
//! produces the value and unravel the request through that call instead.
//!
//! `e` becomes `e |> set_stroke default`, and the ordinary rule for
//! `set_stroke` then routes the request: the stroke to the new argument, the
//! rest to `e`. Nothing here knows about any particular function. Candidates
//! are the functions marked `@lasher`, tried in the order they are declared.
use super::*;
use crate::unravel::LiteralPatch;
use std::sync::Mutex;

/// Where a lasher could go: the outermost expression of the pipeline whose
/// rule snagged, in entry-file coordinates.
#[derive(Clone, Debug)]
pub(super) struct LashPoint {
    pub span: knot_syntax::Span,
    /// The request that snagged, printed. A lasher that leaves the very same
    /// request to snag again took nothing and is not the one to add.
    pub request: Option<String>,
}
pub(super) type SnagSlot = Arc<Mutex<Option<LashPoint>>>;

/// A lashing nests: adding one setter may reveal that another is needed.
const MAX_LASHINGS: usize = 4;

impl Walker<'_> {
    /// Remember the first snag of this attempt, if a lasher could be added
    /// for it: the pipeline has to start in editable entry-file source.
    pub(super) fn note_snag(&mut self, site: &Site, request: &TypedValue) {
        let root = site.lash_root();
        if root.synthetic || root.candidate_source || root.locked {
            return;
        }
        let entry = self.snapshot.graph.entry();
        if root.module != entry.program.canonical.name
            || self
                .snapshot
                .editable_source(self.snapshot.entry_path(), root.expr.span)
                .is_none()
        {
            return;
        }
        if self.snag.lock().unwrap().is_some() {
            return;
        }
        let request = self
            .probes
            .decode_live(request)
            .ok()
            .map(|data| format!("{data:?}"));
        *self.snag.lock().unwrap() = Some(LashPoint {
            span: root.expr.span,
            request,
        });
    }
}

struct Lasher {
    reference: String,
    /// Arguments before the wrapped value.
    leading: usize,
}

fn arity(ty: &knot_canonical::ast::CType) -> usize {
    match ty {
        knot_canonical::ast::CType::Fn(_, rest) => 1 + arity(rest),
        _ => 0,
    }
}

/// Every `@lasher` function the entry module can name, with the spelling to
/// use there.
fn lashers(snapshot: &ProjectSnapshot) -> Vec<Lasher> {
    use knot_syntax::ast::decl::{ExposedItem, Exposing};
    let entry = &snapshot.graph.entry().program.canonical;
    let mut found = Vec::new();
    for module in &snapshot.graph.modules {
        let canonical = &module.program.canonical;
        let prefix = if canonical.name == entry.name {
            Some(String::new())
        } else {
            entry
                .imports
                .iter()
                .find(|import| import.module == canonical.name)
                .map(|import| {
                    import
                        .alias
                        .clone()
                        .unwrap_or_else(|| canonical.name.join("."))
                        + "."
                })
        };
        // Not imported by the entry module: its names cannot be written there.
        let Some(prefix) = prefix else { continue };
        let exposed = |name: &str| {
            canonical.name == entry.name
                || entry
                    .imports
                    .iter()
                    .filter(|import| import.module == canonical.name)
                    .any(|import| match &import.exposing {
                        Some(Exposing::All) => true,
                        Some(Exposing::Some(items)) => items
                            .iter()
                            .any(|item| matches!(item, ExposedItem::Value(v) if v == name)),
                        None => false,
                    })
        };
        for decl in &canonical.decls {
            let CDecl::Fn(f) = &decl.node else { continue };
            if !f.annotations.iter().any(|a| a.key == "lasher") {
                continue;
            }
            let Some(signature) = &f.signature else { continue };
            let arity = arity(&signature.node.ty);
            if arity == 0 {
                continue;
            }
            found.push(Lasher {
                reference: if exposed(&f.name) {
                    f.name.clone()
                } else {
                    format!("{prefix}{}", f.name)
                },
                leading: arity - 1,
            });
        }
    }
    found
}

/// Whether `source[span]` can be extended with `|> f x` in place without
/// parentheses: it must already stand alone between delimiters.
fn stands_alone(source: &str, span: knot_syntax::Span) -> bool {
    let before = source[..span.start as usize].trim_end();
    let after = source[span.end as usize..].trim_start_matches([' ', '\t']);
    let opens = before.ends_with(['[', ',', '(', '=']) && !before.ends_with("==");
    let closes = after.is_empty()
        || after.starts_with([',', ']', ')', '\n'])
        || after.starts_with("|>");
    opens && closes
}

/// Express an edit made to `lashed` (the source with a lasher inserted over
/// `region`) as patches to the original source.
fn rebase(
    original: &str,
    region: knot_syntax::Span,
    inserted: &str,
    patches: &[LiteralPatch],
) -> Option<Vec<LiteralPatch>> {
    let start = region.start as usize;
    let end = region.end as usize;
    let inserted_end = start + inserted.len();
    let shift = inserted.len() as i64 - (end - start) as i64;
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut within = inserted.to_owned();
    let mut within_shift = 0i64;
    for patch in patches {
        let (a, b) = (patch.span.start as usize, patch.span.end as usize);
        if b <= start {
            before.push(patch.clone());
        } else if a >= inserted_end {
            after.push(LiteralPatch {
                span: knot_syntax::Span::new(
                    (a as i64 - shift) as u32,
                    (b as i64 - shift) as u32,
                ),
                ..patch.clone()
            });
        } else if a >= start && b <= inserted_end {
            // Inside the lashed expression: fold it into the one replacement.
            let from = (a as i64 - start as i64 + within_shift) as usize;
            let to = (b as i64 - start as i64 + within_shift) as usize;
            within.replace_range(from..to, &patch.replacement);
            within_shift += patch.replacement.len() as i64 - (b - a) as i64;
        } else if a <= start && b >= inserted_end {
            // The whole lashed expression was replaced anyway.
            let original_end = (b as i64 - shift) as usize;
            return Some(
                before
                    .into_iter()
                    .chain([LiteralPatch {
                        span: knot_syntax::Span::new(a as u32, original_end as u32),
                        before: original[a..original_end].to_owned(),
                        replacement: patch.replacement.clone(),
                    }])
                    .collect(),
            );
        } else {
            return None;
        }
    }
    before.push(LiteralPatch {
        span: region,
        before: original[start..end].to_owned(),
        replacement: within,
    });
    before.extend(after);
    Some(before)
}

impl SourceEditor {
    /// Unravel; if a rule snags where a lasher could be added, try each
    /// lasher there and unravel again through it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn prepare_lashing(
        &self,
        snapshot: &ProjectSnapshot,
        context: &PreparedContext,
        requests: &[OutputRequest<'_>],
        argument: Option<&Request>,
        roots: &[std::path::PathBuf],
        limits: EditLimits,
        depth: usize,
        // The request that snagged before the lasher being tried was added.
        previous: Option<&str>,
    ) -> Result<SourceEdit, EditError> {
        let snag = SnagSlot::default();
        let failure = match self.prepare_requests_once(
            snapshot,
            context,
            requests,
            argument,
            roots,
            limits.clone(),
            &snag,
        ) {
            Err(EditError::RuleFailed) => EditError::RuleFailed,
            result => return result,
        };
        let Some(point) = snag.lock().unwrap().clone() else {
            return Err(failure);
        };
        // The lasher just added changed nothing about the snag: stop here, so
        // the caller moves on to the next one.
        if depth >= MAX_LASHINGS || (depth > 0 && point.request.as_deref() == previous) {
            return Err(failure);
        }
        // Without a readable request there is no telling progress from none.
        if point.request.is_none() && depth > 0 {
            return Err(failure);
        }
        let original = &snapshot.sources()[snapshot.entry_path()];
        let expression = &original[point.span.start as usize..point.span.end as usize];
        let bare = stands_alone(original, point.span);
        for lasher in lashers(snapshot) {
            if limits
                .runtime
                .cancellation
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(twine::Outcome::Cancelled.into());
            }
            let call = format!(
                "{expression} |> {}{}",
                lasher.reference,
                " default".repeat(lasher.leading)
            );
            let inserted = if bare { call } else { format!("({call})") };
            let mut lashed = String::with_capacity(original.len() + inserted.len());
            lashed.push_str(&original[..point.span.start as usize]);
            lashed.push_str(&inserted);
            lashed.push_str(&original[point.span.end as usize..]);
            if lashed.len() > limits.source_bytes {
                continue;
            }
            // A lasher of the wrong type, or one whose arguments have no
            // `Default`, does not check and is skipped.
            let Ok(candidate) = ProjectSnapshot::load_reusing(
                snapshot.entry_path(),
                roots,
                &Default::default(),
                Some(lashed),
                snapshot.entry_extension(),
                Some(snapshot),
            ) else {
                continue;
            };
            let Ok(lashed_context) = self.context(&candidate) else {
                continue;
            };
            let edit = match self.prepare_lashing(
                &candidate,
                &lashed_context,
                requests,
                argument,
                roots,
                limits.clone(),
                depth + 1,
                point.request.as_deref(),
            ) {
                Ok(edit) => edit,
                Err(
                    EditError::RuleFailed
                    | EditError::ValidationFailed
                    | EditError::ConflictingRequests
                    | EditError::Unsupported(_)
                    | EditError::Check(_),
                ) => continue,
                Err(error) => return Err(error),
            };
            // Report the lashing and the unravel through it as one edit to
            // the original source.
            let Some(patches) = rebase(original, point.span, &inserted, &edit.patches) else {
                continue;
            };
            let Ok(source_change) = context
                .edit_domain
                .certify(snapshot, &edit.candidate, &patches)
            else {
                continue;
            };
            return Ok(SourceEdit {
                snapshot: snapshot.id().into(),
                source_change,
                patches,
                ..edit
            });
        }
        Err(failure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn span(source: &str, text: &str) -> knot_syntax::Span {
        let start = source.find(text).unwrap();
        knot_syntax::Span::new(start as u32, (start + text.len()) as u32)
    }
    #[test]
    fn an_expression_between_delimiters_needs_no_parentheses() {
        for source in ["x = [a, b c]", "x = f (b c)", "x = b c\n", "x = b c |> g"] {
            assert!(stands_alone(source, span(source, "b c")), "{source}");
        }
        for source in ["x = f b c", "x = b c + 1", "x = 1 + b c", "x = a == b c"] {
            assert!(!stands_alone(source, span(source, "b c")), "{source}");
        }
    }
    #[test]
    fn edits_inside_the_lashed_expression_fold_into_one_patch() {
        let original = "x = [a, b]";
        let region = span(original, "b");
        let inserted = "b |> f default";
        // lashed: "x = [a, b |> f default]"; the unravel then spliced `default`.
        let lashed = "x = [a, b |> f default]";
        let patch = LiteralPatch {
            span: span(lashed, "default"),
            before: "default".into(),
            replacement: "(7)".into(),
        };
        let patches = rebase(original, region, inserted, &[patch]).unwrap();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].span, region);
        assert_eq!(patches[0].before, "b");
        assert_eq!(patches[0].replacement, "b |> f (7)");
    }
    #[test]
    fn edits_outside_keep_their_place_in_the_original() {
        let original = "y = 1\nx = [b]\nz = 2";
        let region = span(original, "b");
        let lashed = "y = 1\nx = [b |> f default]\nz = 2";
        let patches = rebase(
            original,
            region,
            "b |> f default",
            &[
                LiteralPatch {
                    span: span(lashed, "1"),
                    before: "1".into(),
                    replacement: "(9)".into(),
                },
                LiteralPatch {
                    span: span(lashed, "2"),
                    before: "2".into(),
                    replacement: "(8)".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(patches.len(), 3);
        assert_eq!(patches[0].span, span(original, "1"));
        assert_eq!(patches[2].span, span(original, "2"));
    }
}
