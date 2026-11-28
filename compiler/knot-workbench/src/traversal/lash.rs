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

/// What the application prefers when a request could be met either by
/// changing what is written or by adding a call around it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strategy {
    /// Unravel all the way back. Only if a rule snags, lash.
    #[default]
    UnravelFirst,
    /// At the first cleat reached, or at the very end of the first pipeline
    /// reached when there is none, try the lashers. If none takes the
    /// request, unravel as usual.
    LashFirst,
}

/// Stops a traversal at the first place a lasher could go.
#[derive(Clone, Default)]
pub(super) struct Halt {
    /// Stop at cleats only; otherwise at the end of a pipeline.
    pub cleats: bool,
    /// Places already tried where no lasher has the right type.
    pub skip: Vec<knot_syntax::Span>,
    pub found: SnagSlot,
}

/// Lash first looks past this many places where no lasher has the right type.
const MAX_HALTS: usize = 4;

impl Walker<'_> {
    /// A lasher can be written here: editable source of the entry file.
    fn lashable(&self, site: &Site) -> bool {
        !(site.synthetic || site.candidate_source || site.locked)
            && site.module == self.snapshot.graph.entry().program.canonical.name
            && self
                .snapshot
                .editable_source(self.snapshot.entry_path(), site.expr.span)
                .is_some()
    }

    /// Where a lasher for the value at `site` goes: right after the outermost
    /// cleat on its chain, or else at the very end of the chain.
    fn lash_point(&self, site: &Site) -> Option<Arc<Site>> {
        site.cleat
            .clone()
            .filter(|cleat| self.lashable(cleat))
            .or_else(|| Some(site.lash_root()).filter(|root| self.lashable(root)))
    }

    /// Remember the first snag of this attempt, if a lasher could be added
    /// for it.
    pub(super) fn note_snag(&mut self, site: &Site, request: &TypedValue) {
        let Some(point) = self.lash_point(site) else {
            return;
        };
        if self.snag.lock().unwrap().is_some() {
            return;
        }
        let request = self
            .probes
            .decode_live(request)
            .ok()
            .map(|data| format!("{data:?}"));
        *self.snag.lock().unwrap() = Some(LashPoint {
            span: point.expr.span,
            request,
        });
    }

    /// Remember the first list literal that is given a new element: the end
    /// of a literal is being materialized. A maker could write the element.
    pub(super) fn note_growth(&mut self, site: &Site) {
        if !matches!(&site.expr.node, CExpr::List(rest) if rest.is_empty()) {
            return;
        }
        let literal = Site {
            synthetic: false,
            ..site.clone()
        };
        if !self.lashable(&literal) {
            return;
        }
        let mut growth = self.growth.lock().unwrap();
        if growth.is_none() {
            *growth = Some(LashPoint {
                span: site.expr.span,
                request: None,
            });
        }
    }

    /// Lash first: end the traversal at a cleat (`cleat`) or at the end of
    /// the pipeline a rule is about to be asked about.
    pub(super) fn halt_at(&mut self, site: &Site, cleat: bool) -> Result<(), EditError> {
        let Some(halt) = &self.halt else {
            return Ok(());
        };
        if halt.cleats != cleat {
            return Ok(());
        }
        let point = if cleat {
            Some(Arc::new(site.clone())).filter(|site| self.lashable(site))
        } else {
            Some(site.lash_root()).filter(|root| self.lashable(root))
        };
        let Some(point) = point else {
            return Ok(());
        };
        if halt.skip.contains(&point.expr.span) {
            return Ok(());
        }
        // A pipeline with a cleat in it is lashed at the cleat or not at all:
        // what is written outside the cleat is edited as usual.
        if !cleat
            && self
                .snapshot
                .editable_source(self.snapshot.entry_path(), point.expr.span)
                .is_some_and(|text| text.contains("@cleat"))
        {
            return Ok(());
        }
        let mut found = halt.found.lock().unwrap();
        if found.is_none() {
            *found = Some(LashPoint {
                span: point.expr.span,
                request: None,
            });
        }
        Err(EditError::RuleFailed)
    }
}

struct Lasher {
    reference: String,
    /// Arguments before the wrapped value; every argument for a maker.
    leading: usize,
    /// Passes a value of its result type on as its last argument, so it can
    /// be added around one. Otherwise it makes a value from its arguments.
    wraps: bool,
}

fn result(ty: &knot_canonical::ast::CType) -> &knot_canonical::ast::CType {
    match ty {
        knot_canonical::ast::CType::Fn(_, rest) => result(rest),
        other => other,
    }
}
fn last_argument(ty: &knot_canonical::ast::CType) -> Option<&knot_canonical::ast::CType> {
    match ty {
        knot_canonical::ast::CType::Fn(argument, rest) => {
            last_argument(rest).or(Some(argument.as_ref()))
        }
        _ => None,
    }
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
            let wraps = last_argument(&signature.node.ty) == Some(result(&signature.node.ty));
            found.push(Lasher {
                reference: if exposed(&f.name) {
                    f.name.clone()
                } else {
                    format!("{prefix}{}", f.name)
                },
                leading: arity - usize::from(wraps),
                wraps,
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

/// A span that is one parenthesised expression, without its parentheses: the
/// lasher is written inside them.
fn unwrapped(source: &str, span: knot_syntax::Span) -> knot_syntax::Span {
    let text = &source[span.start as usize..span.end as usize];
    if !text.starts_with('(') || !text.ends_with(')') {
        return span;
    }
    let mut depth = 0usize;
    for (i, c) in text.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 && i + 1 != text.len() {
                    return span;
                }
            }
            '"' => return span,
            _ => {}
        }
    }
    knot_syntax::Span::new(span.start + 1, span.end - 1)
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

/// What trying the lashers at one place came to.
struct Tried {
    edit: Option<SourceEdit>,
    /// Some lasher had the right type to go there.
    checked: bool,
}

impl SourceEditor {
    /// Unravel; if a rule snags where a lasher could be added, try each
    /// lasher there and unravel again through it. With `Strategy::LashFirst`
    /// the lashers are tried before anything is unravelled.
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
        if depth == 0 && self.strategy == Strategy::LashFirst {
            if let Some(edit) =
                self.lash_first(snapshot, context, requests, argument, roots, &limits)?
            {
                return Ok(edit);
            }
        }
        let snag = SnagSlot::default();
        let growth = SnagSlot::default();
        let result = self.prepare_requests_once(
            snapshot,
            context,
            requests,
            argument,
            roots,
            limits.clone(),
            &snag,
            &growth,
            None,
        );
        // A list was given a new element: have a maker write it, if one can.
        let grown = growth.lock().unwrap().clone();
        if let Some(point) = grown.filter(|_| depth < MAX_LASHINGS) {
            // Only where the element could be added anyway: a maker changes
            // how it is written, not whether the list may grow.
            if result.is_ok() {
                if let Some(edit) = self.try_makers(
                    snapshot, context, requests, argument, roots, &limits, &point,
                )? {
                    return Ok(edit);
                }
            }
        }
        let failure = match result {
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
        self.try_lashers(
            snapshot, context, requests, argument, roots, &limits, depth, &point, false,
        )?
        .edit
        .ok_or(failure)
    }

    /// Find the first place a lasher could go without unravelling anything,
    /// and try the lashers there. `None` when no lasher takes the request.
    fn lash_first(
        &self,
        snapshot: &ProjectSnapshot,
        context: &PreparedContext,
        requests: &[OutputRequest<'_>],
        argument: Option<&Request>,
        roots: &[std::path::PathBuf],
        limits: &EditLimits,
    ) -> Result<Option<SourceEdit>, EditError> {
        // Cleats come first; the end of a pipeline only when none is reached.
        let mut halt = Halt {
            cleats: true,
            ..Halt::default()
        };
        for _ in 0..=MAX_HALTS {
            halt.found = SnagSlot::default();
            let result = self.prepare_requests_once(
                snapshot,
                context,
                requests,
                argument,
                roots,
                limits.clone(),
                &SnagSlot::default(),
                &SnagSlot::default(),
                Some(&halt),
            );
            let point = halt.found.lock().unwrap().clone();
            let Some(point) = point else {
                match result {
                    Err(
                        EditError::RuleFailed
                        | EditError::ValidationFailed
                        | EditError::ConflictingRequests
                        | EditError::Unsupported(_),
                    )
                    | Ok(_) => {}
                    Err(error) => return Err(error),
                }
                if halt.cleats {
                    halt.cleats = false;
                    continue;
                }
                return Ok(None);
            };
            let tried = self.try_lashers(
                snapshot, context, requests, argument, roots, limits, 0, &point, true,
            )?;
            if tried.edit.is_some() || tried.checked || halt.cleats {
                return Ok(tried.edit);
            }
            // Nothing can be lashed onto a value of this type: look further in.
            halt.skip.push(point.span);
        }
        Ok(None)
    }

    /// A new element for the list literal at `point`, written as a call to a
    /// maker: `circle default default default`, with the request then
    /// unravelled through it. The first maker the request goes through wins.
    #[allow(clippy::too_many_arguments)]
    fn try_makers(
        &self,
        snapshot: &ProjectSnapshot,
        context: &PreparedContext,
        requests: &[OutputRequest<'_>],
        argument: Option<&Request>,
        roots: &[std::path::PathBuf],
        limits: &EditLimits,
        point: &LashPoint,
    ) -> Result<Option<SourceEdit>, EditError> {
        let original = &snapshot.sources()[snapshot.entry_path()];
        let literal = &original[point.span.start as usize..point.span.end as usize];
        let Some(body) = literal.strip_suffix(']') else {
            return Ok(None);
        };
        let elements = body.trim_end();
        let closing = &body[elements.len()..];
        let separator = if elements == "[" { "" } else { ", " };
        for maker in lashers(snapshot).into_iter().filter(|l| !l.wraps) {
            if limits
                .runtime
                .cancellation
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(twine::Outcome::Cancelled.into());
            }
            let inserted = format!(
                "{elements}{separator}{}{}{closing}]",
                maker.reference,
                " default".repeat(maker.leading)
            );
            let mut made = String::with_capacity(original.len() + inserted.len());
            made.push_str(&original[..point.span.start as usize]);
            made.push_str(&inserted);
            made.push_str(&original[point.span.end as usize..]);
            if made.len() > limits.source_bytes {
                continue;
            }
            // A maker of another type, or one whose arguments have no
            // `Default`, does not check and is skipped.
            let Ok(candidate) = ProjectSnapshot::load_reusing(
                snapshot.entry_path(),
                roots,
                &Default::default(),
                Some(made),
                snapshot.entry_extension(),
                Some(snapshot),
            ) else {
                continue;
            };
            let Ok(made_context) = self.context(&candidate) else {
                continue;
            };
            let edit = match self.prepare_lashing(
                &candidate,
                &made_context,
                requests,
                argument,
                roots,
                limits.clone(),
                // One new element, written by one call and nothing more.
                MAX_LASHINGS,
                None,
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
            let Some(patches) = rebase(original, point.span, &inserted, &edit.patches) else {
                continue;
            };
            let Ok(source_change) = context
                .edit_domain
                .certify(snapshot, &edit.candidate, &patches)
            else {
                continue;
            };
            return Ok(Some(SourceEdit {
                snapshot: snapshot.id().into(),
                source_change,
                patches,
                ..edit
            }));
        }
        Ok(None)
    }

    /// Try each lasher at `point`. With `first`, a lasher is kept only when
    /// it takes the request by itself: its arguments are the only source that
    /// changes.
    #[allow(clippy::too_many_arguments)]
    fn try_lashers(
        &self,
        snapshot: &ProjectSnapshot,
        context: &PreparedContext,
        requests: &[OutputRequest<'_>],
        argument: Option<&Request>,
        roots: &[std::path::PathBuf],
        limits: &EditLimits,
        depth: usize,
        point: &LashPoint,
        first: bool,
    ) -> Result<Tried, EditError> {
        let mut tried = Tried {
            edit: None,
            checked: false,
        };
        let original = &snapshot.sources()[snapshot.entry_path()];
        // A parenthesised expression keeps its parentheses, with the lasher
        // written inside them.
        let inner = unwrapped(original, point.span);
        let expression = &original[inner.start as usize..inner.end as usize];
        let bare = inner == point.span && stands_alone(original, point.span);
        for lasher in lashers(snapshot).into_iter().filter(|l| l.wraps) {
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
            tried.checked = true;
            let edit = match self.prepare_lashing(
                &candidate,
                &lashed_context,
                requests,
                argument,
                roots,
                limits.clone(),
                // Lash first adds one call and nothing more.
                if first { MAX_LASHINGS } else { depth + 1 },
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
            if first {
                // The arguments start after the expression and its ` |> name`.
                let arguments = point.span.start as usize
                    + inserted.len()
                    - usize::from(!bare)
                    - " default".len() * lasher.leading;
                if edit.patches.len() != lasher.leading
                    || edit
                        .patches
                        .iter()
                        .any(|patch| {
                            (patch.span.start as usize) < arguments
                                || patch.span.end as usize > point.span.start as usize + inserted.len()
                        })
                {
                    continue;
                }
            }
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
            tried.edit = Some(SourceEdit {
                snapshot: snapshot.id().into(),
                source_change,
                patches,
                ..edit
            });
            break;
        }
        Ok(tried)
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
