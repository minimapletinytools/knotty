//! Depth-first source work with explicit continuations. Each node's children are
//! scheduled in source/rule order, then processed before its remaining siblings.
//! Field envelopes are opened only when their turn is reached.
use super::*;
// Keep visits inline as before, without a separate allocation for every source
// hop. Completion entries share the same bounded, depth-first work stack.
#[allow(clippy::large_enum_variant)]
pub(super) enum Work {
    Visit {
        site: Site,
        request: TypedValue,
        ty: Ty,
    },
    ListRemainder(Box<list_route::Remainder>),
    Complete(Vec<usize>),
}
impl Walker<'_> {
    pub(super) fn walk(
        &mut self,
        site: Site,
        request: TypedValue,
        ty: Ty,
    ) -> Result<(), EditError> {
        self.unravel_cache.clear_wave();
        self.schedule(site, request, ty)?;
        while let Some(work) = self.pending.pop() {
            // A pass ends at the first value it changes: everything still
            // pending was worked out from values that are now stale.
            if self.stepping && self.stepped {
                self.pending.clear();
                break;
            }
            if self
                .probes
                .limits
                .runtime
                .cancellation
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                return Err(twine::Outcome::Cancelled.into());
            }
            let siblings = self.pending.len();
            match work {
                Work::Visit { site, request, ty } => self.run_field(site, request, ty)?,
                Work::ListRemainder(rest) => self.resume_list_route(*rest)?,
                Work::Complete(entries) => self.unravel_cache.complete(entries),
            }
            if !(self.stepping && self.reversed) {
                self.pending[siblings..].reverse();
            }
        }
        Ok(())
    }
    pub(super) fn schedule(
        &mut self,
        site: Site,
        request: TypedValue,
        ty: Ty,
    ) -> Result<(), EditError> {
        self.push_work(Work::Visit { site, request, ty })
    }

    pub(super) fn push_work(&mut self, item: Work) -> Result<(), EditError> {
        if self.pending.len() >= self.probes.limits.traversal.source_sites {
            return unsupported("source traversal exceeded its pending-work budget");
        }
        self.pending.push(item);
        Ok(())
    }
}
