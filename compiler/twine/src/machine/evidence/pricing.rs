//! Bounded retention estimates over the production read DAG. This is a benefit
//! heuristic, never a validity proof or a charge for work skipped by cache hits.
use super::*;

#[derive(Default)]
pub(in crate::machine) struct RetainedCosts(std::collections::HashSet<storage::Id>);
impl RetainedCosts {
    pub fn contains(&self, proof: Proof) -> bool {
        proof.id.is_none_or(|id| self.0.contains(&id))
    }
    pub fn insert(&mut self, proof: Proof) {
        if let Some(id) = proof.id {
            self.0.insert(id);
        }
    }
}
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::machine) struct CostEstimate {
    pub own_work: u64,
    /// All available dependency records were visited or covered by a retained
    /// result. Stored saturation still makes the estimate a lower bound.
    pub complete: bool,
    pub saturated: bool,
}
impl Store {
    pub(in crate::machine) fn estimate_cost(
        &self,
        root: Proof,
        retained: &RetainedCosts,
        visits: usize,
        remaining: &mut u64,
    ) -> CostEstimate {
        let mut estimate = CostEstimate {
            complete: true,
            ..Default::default()
        };
        let mut pending = vec![root];
        let mut seen = std::collections::HashSet::new();
        while let Some(proof) = pending.pop() {
            if *remaining == 0 || seen.len() >= visits {
                estimate.complete = false;
                break;
            }
            *remaining -= 1;
            let Some(id) = proof.id else {
                estimate.complete = false;
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            let Some(record) = self.records.get(id) else {
                estimate.complete = false;
                continue;
            };
            // One summary can describe different runtime computations. Retaining
            // one result with it does not pay for all other results sharing it.
            if matches!(record.recipe, Recipe::Summary(_)) {
                estimate.complete = false;
                continue;
            }
            if retained.0.contains(&id) {
                continue;
            }
            if let Some(cost) = record.cost.sample() {
                estimate.own_work = estimate.own_work.saturating_add(cost.own);
                estimate.saturated |= cost.is_saturated();
            } else {
                // Unmeasured does not certify zero useful work. The selector
                // can fall back to the candidate's observed inclusive sample.
                estimate.complete = false;
            }
            // Bound scratch before copying edges, including repeated reads.
            // Wide/deep/incomplete history is an unknown estimate, not zero cost.
            if record.reads.len() > visits.saturating_sub(pending.len())
                || record.reads.len() as u64 > *remaining
            {
                estimate.complete = false;
                break;
            }
            *remaining -= record.reads.len() as u64;
            pending.extend(record.reads.iter().copied());
        }
        estimate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cost(own: u64, inclusive: u64) -> cost::Stored {
        let mut meter = cost::Meter::default();
        let parent = meter.begin(0);
        let child = meter.begin(0);
        meter.finish(child, inclusive - own);
        meter.finish(parent, inclusive)
    }
    fn record(store: &mut Store, own: u64, inclusive: u64, reads: Vec<Proof>) -> Proof {
        let bytes = BASE_BYTES + READ_BYTES * reads.len();
        store.insert_record(
            Recipe::Historical(None),
            None,
            reads,
            cost(own, inclusive),
            bytes,
            &Limits::default(),
        )
    }
    #[test]
    fn shared_read_dags_sum_own_work_once_and_stop_at_retained_results() {
        let mut store = Store::default();
        let shared = record(&mut store, 100, 100, vec![]);
        let left = record(&mut store, 3, 103, vec![shared]);
        // A hit performed no expensive work during this historical interval.
        let right = record(&mut store, 5, 5, vec![shared]);
        let root = record(&mut store, 2, 110, vec![left, right]);
        let mut retained = RetainedCosts::default();
        let estimate = store.estimate_cost(root, &retained, 128, &mut 1000);
        assert_eq!(
            estimate,
            CostEstimate {
                own_work: 110,
                complete: true,
                saturated: false
            }
        );
        retained.insert(shared);
        assert_eq!(
            store
                .estimate_cost(root, &retained, 128, &mut 1000)
                .own_work,
            10
        );
        retained.insert(left);
        assert_eq!(
            store
                .estimate_cost(root, &retained, 128, &mut 1000)
                .own_work,
            7
        );
        retained.insert(root);
        assert_eq!(
            store
                .estimate_cost(root, &retained, 128, &mut 1000)
                .own_work,
            0
        );
    }
    #[test]
    fn missing_or_over_budget_history_is_not_a_zero_cost_fact() {
        let mut store = Store::default();
        let root = record(&mut store, 3, 1000, vec![Proof::default()]);
        let retained = RetainedCosts::default();
        let estimate = store.estimate_cost(root, &retained, 128, &mut 1000);
        assert!(!estimate.complete);
        assert_eq!(estimate.own_work, 3);
        for visits in [0, 1] {
            let mut remaining = 1;
            assert!(
                !store
                    .estimate_cost(root, &retained, visits, &mut remaining)
                    .complete
            );
        }
        let unmeasured = store.insert_record(
            Recipe::Historical(None),
            None,
            vec![],
            cost::Stored::default(),
            BASE_BYTES,
            &Limits::default(),
        );
        assert!(
            !store
                .estimate_cost(unmeasured, &retained, 128, &mut 1000)
                .complete
        );
    }
}
