//! Solving one request in passes, for branches that converge.
//!
//! One request can branch (a rule with several inputs asks something of each)
//! and converge again on a shared source value, with each branch asking that
//! value for something different. A single traversal cannot reconcile them:
//! both demands were worked out from the original values.
//!
//! So each pass decides one source value. It unravels from the output with the
//! current values and stops at the first value it changes. That value is
//! frozen, the program is evaluated again, and the next pass starts from the
//! output. An input whose source values are all frozen is pinned, so a rule
//! with several inputs can send its request the other way. A pass that
//! changes nothing has satisfied the request.
use super::*;
use crate::unravel::LiteralPatch;

/// More passes than a real edit decides values.
const MAX_PASSES: usize = 64;

type Solved = (Arc<ProjectSnapshot>, Vec<LiteralPatch>);

fn retryable(error: &EditError) -> bool {
    matches!(
        error,
        EditError::RuleFailed | EditError::ValidationFailed | EditError::ConflictingRequests
    )
}

impl Walker<'_> {
    /// Try deciding values in traversal order, then once in the other order.
    pub(super) fn solve_in_passes(
        &mut self,
        output: &(Site, TypedValue, Ty),
        roots: &[std::path::PathBuf],
        libraries: &knot_core::Program,
    ) -> Result<Solved, EditError> {
        let mut failure = EditError::ValidationFailed;
        for reversed in [false, true] {
            self.restart_attempt(libraries)?;
            self.stepping = true;
            self.reversed = reversed;
            let result = self.passes(output, roots, libraries);
            self.stepping = false;
            self.reversed = false;
            self.step_proposals.clear();
            match result {
                Ok(solved) => return Ok(solved),
                Err(error) if retryable(&error) => failure = error,
                Err(error) => return Err(error),
            }
        }
        Err(failure)
    }

    fn passes(
        &mut self,
        (site, request, ty): &(Site, TypedValue, Ty),
        roots: &[std::path::PathBuf],
        libraries: &knot_core::Program,
    ) -> Result<Solved, EditError> {
        // What frozen values must still hold at the end.
        let mut frozen: Vec<(Site, TypedValue, Ty)> = Vec::new();
        for _ in 0..MAX_PASSES {
            // Requests retained by the last pass came from stale values.
            self.obligations = frozen.clone();
            // Branch guards are kept: which branch the original program took
            // must still hold at the end, whatever later passes walk through.
            self.pending.clear();
            self.proposals.clear();
            self.step_proposals.clear();
            self.stepped = false;
            self.release_held();
            self.walk(site.clone(), request.clone(), ty.clone())?;
            if !self.stepped {
                // Nothing left to change: every request of this pass was
                // worked out from current values, so check them all.
                let (candidate, patches, _, _) = self.install_candidate(roots, libraries, true)?;
                return Ok((candidate, patches));
            }
            self.install_candidate(roots, libraries, false)?;
            // Freeze what this pass settled: the value it changed, and each
            // expression on the way whose request now fits.
            for (site, request, ty) in std::mem::take(&mut self.step_proposals) {
                if self.fits(&site, &request, &ty)? {
                    self.proposals.push(site);
                }
            }
            self.obligations.clear();
            self.freeze_solutions()?;
            frozen.append(&mut self.obligations);
        }
        Err(EditError::ValidationFailed)
    }

    /// Hold every source value this expression depends on for the rest of
    /// the pass: its request already fits.
    pub(super) fn hold_values(&mut self, s: &Site, depth: usize) {
        if depth > 128 {
            return;
        }
        let entry = self.snapshot.graph.entry().program.canonical.name.clone();
        let children: Vec<Site> = match &s.expr.node {
            CExpr::IntLit(_) | CExpr::FloatLit(_) | CExpr::StringLit(_) => {
                self.hold_node(s);
                return;
            }
            CExpr::Var(Ref::Local(name)) => {
                self.scopes[s.scope].get(name).cloned().into_iter().collect()
            }
            CExpr::Var(reference) => {
                let name = syntax::reference(reference, &s.module);
                match self.definition(&name) {
                    Some(f) if f.params.is_empty() && name.module == entry => vec![Site {
                        expr: f.body,
                        module: name.module,
                        scope: 0,
                        ..s.clone()
                    }],
                    // A use of a library value is itself what would be replaced.
                    Some(f) if f.params.is_empty() => {
                        self.hold_node(s);
                        return;
                    }
                    _ => return,
                }
            }
            CExpr::App(a, b) | CExpr::BinOp(_, a, b) => vec![s.child(a), s.child(b)],
            CExpr::Annotated(_, e) | CExpr::Negate(e) | CExpr::FieldAccess(e, _) => {
                vec![s.child(e)]
            }
            CExpr::List(es) | CExpr::Tuple(es) => es.iter().map(|e| s.child(e)).collect(),
            CExpr::Record(fields) => fields.iter().map(|(_, e)| s.child(e)).collect(),
            CExpr::If(c, t, f) => vec![s.child(c), s.child(t), s.child(f)],
            _ => return,
        };
        for child in children {
            self.hold_values(&child, depth + 1);
        }
    }

    /// Whether every source value this expression depends on is decided, so
    /// that nothing in this edit can change its value any more.
    pub(super) fn all_decided(&self, s: &Site, depth: usize) -> bool {
        if depth > 128 {
            return false;
        }
        if s.fixed || self.solver_pinned(s) || self.frozen_node(s) {
            return true;
        }
        let entry = &self.snapshot.graph.entry().program.canonical.name;
        let all = |parts: &mut dyn Iterator<Item = &Spanned<CExpr>>| {
            for e in parts {
                if !self.all_decided(&s.child(e), depth + 1) {
                    return false;
                }
            }
            true
        };
        match &s.expr.node {
            // A literal that has not been frozen can still be replaced.
            CExpr::IntLit(_) | CExpr::FloatLit(_) | CExpr::StringLit(_) => false,
            CExpr::Unit | CExpr::Ctor(_) => true,
            CExpr::Var(Ref::Local(name)) => self.scopes[s.scope]
                .get(name)
                .is_none_or(|bound| self.all_decided(bound, depth + 1)),
            CExpr::Var(reference) => {
                let name = syntax::reference(reference, &s.module);
                match self.definition(&name) {
                    // A function is code, and code stays as it is.
                    Some(f) if !f.params.is_empty() => true,
                    Some(f) if &name.module == entry => self.all_decided(
                        &Site {
                            expr: f.body,
                            module: name.module,
                            scope: 0,
                            ..s.clone()
                        },
                        depth + 1,
                    ),
                    // A use of a library value can be replaced where it is used.
                    Some(_) => false,
                    None => true,
                }
            }
            CExpr::App(a, b) | CExpr::BinOp(_, a, b) => all(&mut [&**a, &**b].into_iter()),
            CExpr::Annotated(_, e) | CExpr::Negate(e) | CExpr::FieldAccess(e, _) => {
                all(&mut std::iter::once(&**e))
            }
            CExpr::List(es) | CExpr::Tuple(es) => all(&mut es.iter()),
            CExpr::Record(fields) => all(&mut fields.iter().map(|(_, e)| e)),
            CExpr::If(c, t, f) => all(&mut [&**c, &**t, &**f].into_iter()),
            // Anything else may still have a free value inside.
            _ => false,
        }
    }
}
