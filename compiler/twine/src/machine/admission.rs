//! Online admission into the existing bounded index. Cost selects optional
//! candidates only; every successful thunk still memoizes and records reads.
use super::*;
impl Session {
    pub(super) fn admit_completed_cost(&self, cost: cost::Stored, value: ValueId) -> bool {
        !matches!(self.values[value], Value::Io(..))
            && (self.limits.cache_minimum_work == 0
                || cost
                    .sample()
                    .is_some_and(|c| c.inclusive >= self.limits.cache_minimum_work))
    }
    pub(super) fn admit_expression_completion(
        &mut self,
        cell: CellId,
        expression: Option<ExprId>,
        cost: cost::Stored,
        value: ValueId,
    ) {
        if self.limits.cache_minimum_work == 0
            || self.inspection.is_some()
            || self.tracking.is_some()
            || !self.admit_completed_cost(cost, value)
        {
            return;
        }
        let Some(expr) = expression else {
            return;
        };
        let scope = self.thunks[cell.0].activation.0 as usize;
        if !self.envs[scope].empty
            || matches!(
                self.program.expressions[expr.0].kind,
                Kind::Local(_)
                    | Kind::Global(_)
                    | Kind::Lit(_)
                    | Kind::Rational(_)
                    | Kind::Ctor(..)
                    | Kind::Prim(_)
                    | Kind::Unsupported(_)
            )
        {
            return;
        }
        let slots = usize::from(self.memo.expression(expr).is_none());
        if self.cache_work(|session| session.optional_cache_room(slots, cost)) {
            self.memo.remember_completed_expression(
                expr,
                cell,
                self.limits.application_cache_entries,
            );
        }
    }
}

// Online pressure concerns index slots. Physical graph/proof/native byte pricing
// remains the collector's job: removing an index never frees a reachable thunk.
fn rank(work: u64, slots: usize, access: memo::Access, epoch: u64, capacity: usize) -> u128 {
    let age = epoch.saturating_sub(access.epoch);
    let window = capacity.max(1) as u64;
    if age >= window.saturating_mul(8) {
        return 0; // an expensive result cannot pin an unused index indefinitely
    }
    let bytes = 32u128 + slots.max(1) as u128 * 64;
    work.min(1 << 20) as u128 * (1 + access.hits.min(3) as u128) * 1024
        / (bytes * (1 + age / window) as u128)
}
impl Session {
    /// Policy only: no forcing, Eq, code matching or validity certification.
    /// Caller accounts this as maintenance, outside the computation's cost.
    pub(super) fn optional_cache_room(&mut self, slots: usize, cost: cost::Stored) -> bool {
        let capacity = self.limits.application_cache_entries;
        if slots > capacity {
            return false;
        }
        if self.limits.cache_eviction_samples == 0 || slots == 0 {
            return true;
        }
        if self.stats.steps >= self.limits.steps || self.limits.cancellation.load(Ordering::Relaxed)
        {
            return false;
        }
        let epoch = self.memo.advance();
        self.stats.steps += 1;
        self.stats.cache_eviction_work += 1;
        let incoming = rank(
            cost.sample().map_or(0, |c| c.inclusive),
            slots,
            memo::Access { epoch, hits: 0 },
            epoch,
            capacity,
        );
        while slots > capacity.saturating_sub(self.memo.len()) {
            let samples = self.limits.cache_eviction_samples.min(16);
            // Price the bounded sample before any key lookup, ranking or removal.
            // The preflight itself only reads immediate key widths.
            let preflight = samples as u64;
            if preflight > self.limits.steps.saturating_sub(self.stats.steps) {
                self.stats.cache_admission_rejections += 1;
                return false;
            }
            self.stats.steps += preflight;
            self.stats.cache_eviction_work += preflight;
            let work = self.memo.eviction_work_bound(samples);
            if work > self.limits.steps.saturating_sub(self.stats.steps)
                || self.limits.cancellation.load(Ordering::Relaxed)
            {
                self.stats.cache_admission_rejections += 1;
                return false;
            }
            self.stats.steps += work;
            self.stats.cache_eviction_work += work;
            let mut victim = None;
            for (index, candidate, access, candidate_slots) in self
                .memo
                .eviction_window(self.limits.cache_eviction_samples.min(16))
            {
                let stored = match candidate {
                    memo::Candidate::Call(_, done) => Some(done.cost),
                    memo::Candidate::Expression(_, cell)
                    | memo::Candidate::Capture(_, _, cell)
                    | memo::Candidate::Global(cell) => match self.cell_state(cell.owner()) {
                        ThunkState::Done(Ok(done)) => Some(done.cost),
                        _ => None,
                    },
                };
                // Historical useful work is a ranking hint, not a validity
                // claim. Cached proofs may have validated in this world without
                // rewriting the original completion's world stamp.
                let benefit = stored
                    .and_then(|cost| cost.sample())
                    .map_or(0, |c| c.inclusive);
                let score = rank(benefit, candidate_slots, access, epoch, capacity);
                if victim.is_none_or(|(_, lowest)| score < lowest) {
                    victim = Some((index, score));
                }
            }
            let Some((index, score)) = victim else {
                return false;
            };
            if incoming < score {
                self.stats.cache_admission_rejections += 1;
                return false;
            }
            self.memo.evict_at(index);
            self.stats.cache_evictions += 1;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cost(work: u64) -> cost::Stored {
        let mut meter = cost::Meter::default();
        meter.finish(meter.begin(0), work)
    }
    fn admit(s: &mut Session, id: usize, work: u64) -> bool {
        if !s.optional_cache_room(1, cost(work)) {
            return false;
        }
        s.memo.insert_completion(
            memo::Call::allocated(id, CellId(id)),
            memo::Completion {
                cost: cost(work),
                stamp: s.validation.stamp(),
                ..Default::default()
            },
            s.limits.application_cache_entries,
        )
    }
    fn session() -> Session {
        Session::new(
            Arc::new(Program::default()),
            Limits {
                application_cache_entries: 4,
                cache_eviction_samples: 8,
                ..Default::default()
            },
        )
    }
    #[test]
    fn stale_expensive_entries_age_out_while_hits_refresh_bounded_history() {
        let mut s = session();
        for id in 0..4 {
            assert!(admit(&mut s, id, 10_000));
        }
        let hot = memo::Call::allocated(0, CellId(0));
        for id in 4..80 {
            s.memo.note_call_hit(&hot);
            admit(&mut s, id, 10);
            assert!(s.memo.len() <= 4);
        }
        assert!(s.memo.get(&hot).is_some());
        for id in 1..4 {
            assert!(s.memo.get(&memo::Call::allocated(id, CellId(id))).is_none());
        }
        assert!(s.stats.cache_evictions > 0 && s.stats.cache_admission_rejections > 0);
    }
    #[test]
    fn optional_selection_respects_remaining_work_and_cancellation() {
        let mut s = session();
        for id in 0..4 {
            assert!(admit(&mut s, id, 10));
        }
        s.limits.steps = s.stats.steps + 2;
        assert!(!admit(&mut s, 4, 100));
        assert!(s.stats.steps <= s.limits.steps);
        assert_eq!(s.stats.cache_evictions, 0);
        let before = s.stats.steps;
        s.limits.cancellation.store(true, Ordering::Relaxed);
        assert!(!admit(&mut s, 5, 100));
        assert_eq!(s.stats.steps, before);
        assert_eq!(s.memo.len(), 4);
    }
}
