//! Hierarchical work attribution, without an extra stack or ancestor walks.
//! `attributed` is the sum of OWN work of all scopes completed so far in this
//! machine invocation. Between a scope's entry and completion, its change is
//! exactly the work of completed descendants, each counted once. Subtracting
//! that delta is equivalent to summing direct children's inclusive intervals.

#[cfg(feature = "cost-accounting")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkCost {
    pub own: u64,
    pub inclusive: u64,
}

#[cfg(feature = "cost-accounting")]
impl WorkCost {
    /// Saturated fields are lower bounds; live attribution always uses exact u64s.
    pub fn is_saturated(self) -> bool {
        self.inclusive == u32::MAX as u64 - 1
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Stored {
    #[cfg(feature = "cost-accounting")]
    own: u32,
    // Zero is unmeasured. One represents a measured zero, including test scopes.
    #[cfg(feature = "cost-accounting")]
    inclusive_plus_one: u32,
}
#[cfg(feature = "cost-accounting")]
impl Stored {
    fn new(own: u64, inclusive: u64) -> Self {
        const MAX: u64 = u32::MAX as u64 - 1;
        Self {
            own: own.min(MAX) as u32,
            inclusive_plus_one: inclusive.min(MAX) as u32 + 1,
        }
    }
    pub fn sample(self) -> Option<WorkCost> {
        (self.inclusive_plus_one != 0).then(|| WorkCost {
            own: self.own as u64,
            inclusive: (self.inclusive_plus_one - 1) as u64,
        })
    }
}

#[derive(Default)]
pub(super) struct Meter {
    #[cfg(feature = "cost-accounting")]
    attributed: u64,
}

pub(super) struct Mark {
    #[cfg(feature = "cost-accounting")]
    start: u64,
    #[cfg(feature = "cost-accounting")]
    attributed: u64,
}

impl Meter {
    #[inline]
    pub fn begin(&self, _work: u64) -> Mark {
        Mark {
            #[cfg(feature = "cost-accounting")]
            start: _work,
            #[cfg(feature = "cost-accounting")]
            attributed: self.attributed,
        }
    }
    #[inline]
    pub fn finish(&mut self, _mark: Mark, _work: u64) -> Stored {
        #[cfg(feature = "cost-accounting")]
        {
            let inclusive = _work.checked_sub(_mark.start).expect("work clock reversed");
            let children = self
                .attributed
                .checked_sub(_mark.attributed)
                .expect("attribution reversed");
            let own = inclusive
                .checked_sub(children)
                .expect("overlapping cost scopes");
            self.attributed = self
                .attributed
                .checked_add(own)
                .expect("attribution overflow");
            Stored::new(own, inclusive)
        }
        #[cfg(not(feature = "cost-accounting"))]
        {
            Stored {}
        }
    }
}

#[cfg(feature = "cost-accounting")]
impl super::Session {
    /// Historical observed non-maintenance work of successful WHNF evaluation.
    /// Cache bookkeeping still consumes the total execution budget, but cannot
    /// make a computation look more valuable to retain.
    /// Does not force anything. Hits preserve the sample; imported costs are history,
    /// not work charged to this session. Introduced values have no measured sample.
    pub fn work_cost(
        &self,
        handle: super::ThunkHandle,
    ) -> Result<Option<WorkCost>, super::Outcome> {
        self.validate_thunk(handle)?;
        let owner = self.cell_id(super::ThunkId(handle.thunk));
        Ok(match &self.thunks[owner.0].state {
            super::ThunkState::Done(Ok(done)) => done.cost.sample(),
            _ => None,
        })
    }

    /// Retained historical samples only; no event log or forcing. Summing their
    /// inclusive costs does NOT measure total execution or saved work.
    pub fn work_cost_samples(&self) -> impl Iterator<Item = WorkCost> + '_ {
        self.thunks
            .iter()
            .filter_map(|thunk| match &thunk.state {
                super::ThunkState::Done(Ok(done)) => done.cost.sample(),
                _ => None,
            })
            .chain(self.memo.costs())
    }
}

impl super::Session {
    #[inline]
    pub(super) fn cost_clock(&mut self) -> u64 {
        let work = self.work_clock();
        #[cfg(feature = "incremental")]
        return work
            .checked_sub(self.stats.maintenance_steps)
            .expect("maintenance exceeds work");
        #[cfg(not(feature = "incremental"))]
        work
    }

    /// For cache-only phases: never execute user code or cross an inspection
    /// budget reset here. Nested phases are charged exactly once, including
    /// deferred copy-on-write work and unsuccessful optional work.
    #[inline]
    pub(super) fn cache_work<T>(&mut self, operation: impl FnOnce(&mut Self) -> T) -> T {
        #[cfg(feature = "incremental")]
        let before = (self.work_clock(), self.stats.maintenance_steps);
        let result = operation(self);
        #[cfg(feature = "incremental")]
        {
            let work = self.work_clock() - before.0;
            let nested = self.stats.maintenance_steps - before.1;
            self.stats.maintenance_steps +=
                work.checked_sub(nested).expect("overlapping maintenance");
        }
        result
    }

    /// Recorder operations cannot mutate runtime heaps, so they need no COW
    /// flush on the hot path. Both feature-disabled helpers inline away.
    #[inline]
    pub(super) fn record_evidence<T>(
        &mut self,
        operation: impl FnOnce(&mut super::evidence::Store, &super::Limits, &mut u64) -> T,
    ) -> T {
        #[cfg(feature = "incremental")]
        let before = self.stats.steps;
        let result = operation(&mut self.evidence, &self.limits, &mut self.stats.steps);
        #[cfg(feature = "incremental")]
        {
            self.stats.maintenance_steps += self.stats.steps - before;
        }
        result
    }
}

#[cfg(all(test, feature = "cost-accounting"))]
mod tests {
    use super::*;

    #[test]
    fn nested_calls_partition_work_without_double_counting() {
        let mut meter = Meter::default();
        let parent = meter.begin(0);
        let call = meter.begin(2);
        let leaf = meter.begin(5);
        assert_eq!(
            meter.finish(leaf, 12).sample(),
            Some(WorkCost {
                own: 7,
                inclusive: 7
            })
        );
        assert_eq!(
            meter.finish(call, 16).sample(),
            Some(WorkCost {
                own: 7,
                inclusive: 14
            })
        );
        let sibling = meter.begin(17);
        assert_eq!(
            meter.finish(sibling, 20).sample(),
            Some(WorkCost {
                own: 3,
                inclusive: 3
            })
        );
        assert_eq!(
            meter.finish(parent, 25).sample(),
            Some(WorkCost {
                own: 8,
                inclusive: 25
            })
        );
        assert_eq!(meter.attributed, 25);
    }

    #[test]
    fn cheap_children_contribute_before_any_admission_threshold() {
        let mut meter = Meter::default();
        let parent = meter.begin(0);
        for n in 0..1_000 {
            let child = meter.begin(n * 10);
            let cost = meter.finish(child, (n + 1) * 10).sample().unwrap();
            assert!(cost.inclusive < 100);
        }
        assert_eq!(
            meter.finish(parent, 10_020).sample(),
            Some(WorkCost {
                own: 20,
                inclusive: 10_020
            })
        );
    }

    #[test]
    fn root_boundaries_and_hits_do_not_import_historical_costs() {
        let mut meter = Meter::default();
        let first = meter.begin(10);
        assert_eq!(meter.finish(first, 110).sample().unwrap().inclusive, 100);
        let parent = meter.begin(120);
        // The completed first computation is read for two units, without a new scope.
        assert_eq!(
            meter.finish(parent, 122).sample(),
            Some(WorkCost {
                own: 2,
                inclusive: 2
            })
        );
    }

    #[test]
    fn stored_saturation_does_not_corrupt_parent_attribution() {
        assert_eq!(Stored::default().sample(), None);
        assert_eq!(
            Stored::new(0, 0).sample(),
            Some(WorkCost {
                own: 0,
                inclusive: 0
            })
        );
        let mut meter = Meter::default();
        let parent = meter.begin(0);
        let child = meter.begin(2);
        let end = u32::MAX as u64 + 100;
        assert!(meter.finish(child, end).sample().unwrap().is_saturated());
        let parent = meter.finish(parent, end + 3).sample().unwrap();
        assert_eq!(parent.own, 5);
        assert!(parent.is_saturated());
    }
}

#[cfg(all(test, feature = "incremental"))]
mod maintenance_tests {
    use super::super::*;
    #[test]
    fn nested_failed_metadata_work_counts_once_and_exhausts_the_real_budget() {
        let mut session = Session::new(
            Arc::new(Program::default()),
            Limits {
                steps: 20,
                ..Default::default()
            },
        );
        let start = session.cost_clock();
        let result: Result<(), Outcome> = session.cache_work(|session| {
            session.stats.steps += 3;
            session.cache_work(|session| {
                session.stats.steps += 7;
            });
            session.stats.steps += 2;
            Err(Outcome::Cancelled)
        });
        assert_eq!(result, Err(Outcome::Cancelled));
        assert_eq!(session.stats.steps, 12);
        assert_eq!(session.stats.maintenance_steps, 12);
        assert_eq!(session.cost_clock(), start);
        for _ in 0..8 {
            session.tick().unwrap();
        }
        assert_eq!(session.tick(), Err(Outcome::BudgetExhausted));
        assert_eq!(session.cost_clock(), 8);
    }
}
