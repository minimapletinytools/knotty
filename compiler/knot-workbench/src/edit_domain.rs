//! Prospective source-edit permissions and exact accepted transitions. These
//! establish source confinement, not executable equivalence or runtime validity.
use crate::{
    project::ProjectSnapshot,
    unravel::{EditError, LiteralPatch},
};
use knot_canonical::ast::{CDecl, CDoStmt, CExpr, CModule};
use knot_syntax::{Span, Spanned};
use std::{collections::BTreeSet, sync::Arc};

#[derive(Debug)]
struct Domain {
    snapshot: String,
    regions: Vec<Span>,
    expressions: BTreeSet<(u32, u32)>,
    source_bytes: usize,
}

/// Permissions in original source coordinates for one immutable snapshot.
/// A read-only definition's writable reference site is a separate expression.
#[derive(Debug, Clone)]
pub struct EditDomain(Arc<Domain>);

/// Private construction proves that these exact snapshots differ only by checked
/// expression replacements inside the declared domain. This is not authority to
/// skip runtime input, host, scope, type or executable-code validation.
#[derive(Debug, Clone)]
pub struct CheckedSourceChange {
    domain: EditDomain,
    candidate: String,
    changed: Vec<Span>,
}
impl CheckedSourceChange {
    pub fn original_snapshot(&self) -> &str {
        self.domain.snapshot()
    }
    pub fn candidate_snapshot(&self) -> &str {
        &self.candidate
    }
    pub fn domain(&self) -> &EditDomain {
        &self.domain
    }
    pub fn changed_expressions(&self) -> &[Span] {
        &self.changed
    }
    pub fn certifies(&self, original: &ProjectSnapshot, candidate: &ProjectSnapshot) -> bool {
        self.original_snapshot() == original.id() && self.candidate == candidate.id()
    }
}

fn violation(message: &str) -> EditError {
    EditError::EditDomainViolation(message.into())
}
fn contains(region: Span, expression: Span) -> bool {
    region.start <= expression.start && expression.end <= region.end
}
impl EditDomain {
    /// Initial broad policy: original entry-file function/instance-method bodies.
    /// Headers, annotations on declarations, imports and generated helpers are
    /// protected. Unravel annotations do not implicitly freeze function bodies.
    pub fn entry(snapshot: &ProjectSnapshot, source_bytes: usize) -> Result<Self, EditError> {
        let source = &snapshot.sources()[snapshot.entry_path()];
        if source.len() > source_bytes {
            return Err(EditError::SourceTooLarge);
        }
        let module = &snapshot.graph.entry().program.canonical;
        let original = |span: Span| {
            span.start < span.end && source.get(span.start as usize..span.end as usize).is_some()
        };
        let regions = bodies(module)
            .into_iter()
            .map(|e| e.span)
            .filter(|s| original(*s))
            .collect();
        let expressions = expressions(module)
            .map(|e| e.span)
            .filter(|s| original(*s))
            .map(|s| (s.start, s.end))
            .collect();
        Ok(Self(Arc::new(Domain {
            snapshot: snapshot.id().into(),
            regions,
            expressions,
            source_bytes,
        })))
    }
    pub fn snapshot(&self) -> &str {
        &self.0.snapshot
    }
    pub fn regions(&self) -> &[Span] {
        &self.0.regions
    }

    /// Narrow permissions to existing complete expressions. Cannot enlarge a
    /// domain, switch its snapshot, or infer authority from arbitrary byte spans.
    pub fn restrict(&self, regions: &[Span]) -> Result<Self, EditError> {
        if regions.len() > self.0.expressions.len() {
            return Err(violation("too many edit regions"));
        }
        let mut regions = regions.to_vec();
        regions.sort_by_key(|s| (s.start, s.end));
        for (i, span) in regions.iter().enumerate() {
            if !self.permits(*span) || i > 0 && regions[i - 1].end > span.start {
                return Err(violation(
                    "edit regions must be disjoint permitted expressions",
                ));
            }
        }
        Ok(Self(Arc::new(Domain {
            snapshot: self.0.snapshot.clone(),
            regions,
            expressions: self.0.expressions.clone(),
            source_bytes: self.0.source_bytes,
        })))
    }
    fn permits(&self, span: Span) -> bool {
        self.0.expressions.contains(&(span.start, span.end))
            && self.0.regions.iter().any(|region| contains(*region, span))
    }
    fn check_patches(
        &self,
        snapshot: &ProjectSnapshot,
        patches: &[LiteralPatch],
    ) -> Result<usize, EditError> {
        if self.snapshot() != snapshot.id() {
            return Err(EditError::StaleRevision);
        }
        let source = &snapshot.sources()[snapshot.entry_path()];
        let mut length = source.len();
        let mut end = 0;
        for patch in patches {
            if !self.permits(patch.span) || patch.span.start < end {
                return Err(violation(
                    "patches must be ordered disjoint permitted expressions",
                ));
            }
            if source.get(patch.span.start as usize..patch.span.end as usize)
                != Some(patch.before.as_str())
            {
                return Err(EditError::StaleRevision);
            }
            length = length
                .checked_sub(patch.before.len())
                .and_then(|n| n.checked_add(patch.replacement.len()))
                .ok_or(EditError::SourceTooLarge)?;
            if length > self.0.source_bytes {
                return Err(EditError::SourceTooLarge);
            }
            end = patch.span.end;
        }
        Ok(length)
    }
    pub(crate) fn apply(
        &self,
        snapshot: &ProjectSnapshot,
        patches: &[LiteralPatch],
    ) -> Result<String, EditError> {
        let length = self.check_patches(snapshot, patches)?;
        let source = &snapshot.sources()[snapshot.entry_path()];
        let mut result = String::with_capacity(length);
        let mut end = 0;
        for patch in patches {
            result.push_str(&source[end..patch.span.start as usize]);
            result.push_str(&patch.replacement);
            end = patch.span.end as usize;
        }
        result.push_str(&source[end..]);
        Ok(result)
    }
    pub(crate) fn certify(
        &self,
        original: &ProjectSnapshot,
        candidate: &ProjectSnapshot,
        patches: &[LiteralPatch],
    ) -> Result<CheckedSourceChange, EditError> {
        let length = self.check_patches(original, patches)?;
        if original.entry_path() != candidate.entry_path()
            || original.entry_extension() != candidate.entry_extension()
            || !original
                .sources()
                .iter()
                .filter(|(p, _)| *p != original.entry_path())
                .eq(candidate
                    .sources()
                    .iter()
                    .filter(|(p, _)| *p != candidate.entry_path()))
        {
            return Err(EditError::StaleRevision);
        }
        let old = &original.sources()[original.entry_path()];
        let new = &candidate.sources()[candidate.entry_path()];
        if new.len() != length {
            return Err(violation("candidate does not match patches"));
        }
        let mut old_end = 0;
        let mut new_end = 0;
        let mut wanted = BTreeSet::new();
        for patch in patches {
            let prefix = &old[old_end..patch.span.start as usize];
            let start = new_end + prefix.len();
            let end = start + patch.replacement.len();
            if new.get(new_end..start) != Some(prefix)
                || new.get(start..end) != Some(patch.replacement.as_str())
            {
                return Err(violation("candidate does not match patches"));
            }
            wanted.insert((
                u32::try_from(start).map_err(|_| EditError::SourceTooLarge)?,
                u32::try_from(end).map_err(|_| EditError::SourceTooLarge)?,
            ));
            old_end = patch.span.end as usize;
            new_end = end;
        }
        if new.get(new_end..) != Some(&old[old_end..]) {
            return Err(violation("candidate changed protected source"));
        }
        let before = &original.graph.entry().program.canonical;
        let after = &candidate.graph.entry().program.canonical;
        if before.decls.len() != after.decls.len() || before.name != after.name {
            return Err(violation("replacement changed declaration structure"));
        }
        // Byte confinement alone permits text that closes an expression and
        // injects declarations. Require the entire replacement to be one checked
        // expression in its actual receiving context, not parsed in isolation.
        for expression in expressions(after) {
            let (start, end) = (expression.span.start, expression.span.end);
            wanted.remove(&(start, end));
            // A complete expression in its own parentheses is one too.
            if start > 0
                && new.as_bytes().get(start as usize - 1) == Some(&b'(')
                && new.as_bytes().get(end as usize) == Some(&b')')
            {
                wanted.remove(&(start - 1, end + 1));
            }
        }
        if !wanted.is_empty() {
            return Err(violation(
                "replacement is not a complete checked expression",
            ));
        }
        Ok(CheckedSourceChange {
            domain: self.clone(),
            candidate: candidate.id().into(),
            changed: patches.iter().map(|p| p.span).collect(),
        })
    }
}

fn bodies(module: &CModule) -> Vec<&Spanned<CExpr>> {
    module
        .decls
        .iter()
        .flat_map(|d| match &d.node {
            CDecl::Fn(f) => vec![&f.body],
            CDecl::Instance(i) => i.methods.iter().map(|f| &f.body).collect(),
            _ => vec![],
        })
        .collect()
}
pub(crate) fn expressions(module: &CModule) -> impl Iterator<Item = &Spanned<CExpr>> {
    let mut pending = bodies(module);
    std::iter::from_fn(move || {
        let expression = pending.pop()?;
        match &expression.node {
            CExpr::App(a, b) | CExpr::BinOp(_, a, b) => pending.extend([a.as_ref(), b.as_ref()]),
            CExpr::Negate(e)
            | CExpr::FieldAccess(e, _)
            | CExpr::Annotated(_, e)
            | CExpr::Lambda(_, e) => pending.push(e),
            CExpr::Tuple(es) | CExpr::List(es) => pending.extend(es),
            CExpr::Record(fs) => pending.extend(fs.iter().map(|(_, e)| e)),
            CExpr::RecordUpdate(base, fs) => {
                pending.push(base);
                pending.extend(fs.iter().map(|(_, e)| e));
            }
            CExpr::If(c, a, b) => pending.extend([c.as_ref(), a.as_ref(), b.as_ref()]),
            CExpr::Let(bs, body) => {
                pending.push(body);
                pending.extend(bs.iter().map(|(_, e)| e));
            }
            CExpr::Case(value, arms) => {
                pending.push(value);
                pending.extend(arms.iter().map(|(_, e)| e));
            }
            CExpr::Do(statements, body) => {
                pending.push(body);
                for statement in statements {
                    match statement {
                        CDoStmt::Let(bs) => pending.extend(bs.iter().map(|(_, e)| e)),
                        CDoStmt::Bind(_, e) | CDoStmt::Expr(e) => pending.push(e),
                    }
                }
            }
            _ => {}
        }
        Some(expression)
    })
}

#[cfg(test)]
mod tests;
