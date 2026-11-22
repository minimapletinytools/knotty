//! Select complete optional closures while marking, then sweep once. Trial marks
//! are never accepted marks; a rejected/partial candidate contributes no coverage.
use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::machine) struct Usage {
    pub nodes: usize,
    pub edges: usize,
    pub bytes: usize,
    pub sweep: u64,
}

impl Usage {
    fn add(self, other: Self) -> Self {
        Self {
            nodes: self.nodes.saturating_add(other.nodes),
            edges: self.edges.saturating_add(other.edges),
            bytes: self.bytes.saturating_add(other.bytes),
            sweep: self.sweep.saturating_add(other.sweep),
        }
    }
    fn fits(self, limit: Self) -> bool {
        self.nodes <= limit.nodes && self.edges <= limit.edges && self.bytes <= limit.bytes
    }
}
pub(in crate::machine) struct Selected {
    pub collection: Collection,
    pub mandatory: Usage,
    pub retained: Usage,
    pub optional_roots: usize,
}
pub(in crate::machine) struct SelectionBudget<'a> {
    pub work: u64,
    pub cancellation: &'a AtomicBool,
    pub quota: Usage,
    /// Conservative bound on all records' sweep work, used only to reserve fuel.
    pub sweep: u64,
    /// Maximum trial node IDs/worklist entries, reserved by the caller.
    pub nodes: usize,
}
impl<T: Trace> Arena<T> {
    pub fn select(
        &mut self,
        mandatory: impl IntoIterator<Item = Id>,
        optional: &[Id],
        limits: SelectionBudget<'_>,
        bytes: impl Fn(&T) -> usize,
        mut removed: impl FnMut(&T),
    ) -> Selected {
        let mut budget = Budget {
            left: limits.work,
            cancellation: limits.cancellation,
        };
        let mut retained = Usage::default();
        let mut required = Usage::default();
        let mut selected = 0;
        let result = (|| {
            if self.journal.is_some() {
                return Err(CollectionStatus::CheckpointActive);
            }
            // Reserve unique marks for the accepted set and each optional trial.
            // No generation or record changes before the final sweep commits.
            let epochs = optional.len() as u64 + 2;
            if self.epoch.checked_add(epochs).is_none() {
                for slot in &mut self.slots {
                    budget.tick(1)?;
                    slot.marked = 0;
                }
                self.epoch = 0;
            }
            let accepted = self.epoch + 1;
            self.epoch += epochs;
            let mut pending = Vec::new();
            for root in mandatory {
                budget.tick(1)?;
                self.select_enqueue(
                    root,
                    accepted,
                    accepted,
                    &mut pending,
                    &mut retained,
                    Usage {
                        nodes: limits.nodes,
                        edges: usize::MAX,
                        bytes: usize::MAX,
                        sweep: u64::MAX,
                    },
                    &bytes,
                )?;
            }
            while let Some(id) = pending.pop() {
                budget.tick(1)?;
                let edges = self.get(id).unwrap().edge_count();
                for edge in 0..edges {
                    budget.tick(1)?;
                    let child = self.get(id).unwrap().edge(edge);
                    self.select_enqueue(
                        child,
                        accepted,
                        accepted,
                        &mut pending,
                        &mut retained,
                        Usage {
                            nodes: limits.nodes,
                            edges: usize::MAX,
                            bytes: usize::MAX,
                            sweep: u64::MAX,
                        },
                        &bytes,
                    )?;
                }
            }
            required = retained;
            let mut trial = Vec::new();
            for (index, root) in optional.iter().copied().enumerate() {
                // Already-covered proofs require no extra storage. Sharing a
                // validity ID never equates the independent cached results.
                budget.tick(1)?;
                if self.get(root).is_none() {
                    continue;
                }
                if self.slots[root.slot()].marked == accepted {
                    selected += 1;
                    continue;
                }
                let reserve = (self.slots.len() as u64)
                    .saturating_add(limits.sweep.saturating_sub(retained.sweep));
                let allowance = budget.left.saturating_sub(reserve).min(4096);
                if allowance == 0 {
                    break;
                }
                let mut local = Budget {
                    left: allowance,
                    cancellation: limits.cancellation,
                };
                let mark = accepted + 1 + index as u64;
                let mut usage = retained;
                pending.clear();
                trial.clear();
                let cap = Usage {
                    nodes: limits.quota.nodes.min(limits.nodes),
                    ..limits.quota
                };
                let attempt = (|| {
                    self.select_enqueue(
                        root,
                        accepted,
                        mark,
                        &mut pending,
                        &mut usage,
                        cap,
                        &bytes,
                    )?;
                    while let Some(id) = pending.pop() {
                        local.tick(1)?;
                        trial.push(id);
                        let edges = self.get(id).unwrap().edge_count();
                        for edge in 0..edges {
                            local.tick(1)?;
                            let child = self.get(id).unwrap().edge(edge);
                            self.select_enqueue(
                                child,
                                accepted,
                                mark,
                                &mut pending,
                                &mut usage,
                                cap,
                                &bytes,
                            )?;
                        }
                    }
                    local.tick(trial.len() as u64)?;
                    Ok(())
                })();
                budget.left -= allowance - local.left;
                match attempt {
                    Ok(()) => {
                        for id in &trial {
                            self.slots[id.slot()].marked = accepted;
                        }
                        retained = usage;
                        selected += 1;
                    }
                    Err(CollectionStatus::Cancelled) => return Err(CollectionStatus::Cancelled),
                    Err(_) => {} // Partial trial marks are ignored by every later trial and sweep.
                }
            }
            drop(pending);
            drop(trial);
            let mut garbage = Vec::new();
            let mut sweep = 0u64;
            for (index, slot) in self.slots.iter().enumerate() {
                budget.tick(1)?;
                if let Some(value) = &slot.value {
                    if slot.marked != accepted {
                        sweep = sweep
                            .checked_add(value.sweep_work())
                            .ok_or(CollectionStatus::Truncated)?;
                        garbage.push(Id(NonZeroU64::new(
                            (u64::from(slot.generation) << 32) | (index as u64 + 1),
                        )
                        .unwrap()));
                    }
                }
            }
            budget.tick(sweep)?;
            let count = garbage.len();
            for id in garbage {
                let value = self.remove(id).unwrap();
                removed(&value);
            }
            Ok(count)
        })();
        let (status, reclaimed) = match result {
            Ok(n) => (CollectionStatus::Collected, n),
            Err(status) => (status, 0),
        };
        Selected {
            collection: Collection {
                status,
                reclaimed,
                remaining: self.len(),
                work: limits.work - budget.left,
            },
            mandatory: required,
            retained,
            optional_roots: selected,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn select_enqueue(
        &mut self,
        id: Id,
        accepted: u64,
        trial: u64,
        pending: &mut Vec<Id>,
        usage: &mut Usage,
        quota: Usage,
        bytes: &impl Fn(&T) -> usize,
    ) -> Result<(), CollectionStatus> {
        let Some(value) = self.get(id) else {
            return Err(CollectionStatus::InvalidEdge);
        };
        if self.slots[id.slot()].marked == accepted || self.slots[id.slot()].marked == trial {
            return Ok(());
        }
        let next = usage.add(Usage {
            nodes: 1,
            edges: value.edge_count(),
            bytes: bytes(value),
            sweep: value.sweep_work(),
        });
        if !next.fits(quota) {
            return Err(CollectionStatus::Truncated);
        }
        *usage = next;
        self.slots[id.slot()].marked = trial;
        pending.push(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Node {
        edges: Vec<Id>,
        bytes: usize,
    }
    impl Trace for Node {
        fn edge_count(&self) -> usize {
            self.edges.len()
        }
        fn edge(&self, index: usize) -> Id {
            self.edges[index]
        }
    }
    fn node(arena: &mut Arena<Node>, edges: Vec<Id>, bytes: usize) -> Id {
        arena.insert(Node { edges, bytes }, 100_000).ok().unwrap()
    }
    fn run(
        arena: &mut Arena<Node>,
        roots: &[Id],
        candidates: &[Id],
        quota: Usage,
        work: u64,
    ) -> Selected {
        let cancellation = AtomicBool::new(false);
        let sweep = arena.entries().map(|(_, node)| node.sweep_work()).sum();
        arena.select(
            roots.iter().copied(),
            candidates,
            SelectionBudget {
                work,
                cancellation: &cancellation,
                quota,
                sweep,
                nodes: 100_000,
            },
            |node| node.bytes,
            |_| {},
        )
    }
    fn quota(nodes: usize, bytes: usize) -> Usage {
        Usage {
            nodes,
            edges: usize::MAX,
            bytes,
            sweep: u64::MAX,
        }
    }
    #[test]
    fn rejected_trials_never_cover_later_candidates_and_diamonds_count_once() {
        let mut a = Arena::default();
        let shared = node(&mut a, vec![], 10);
        let live = node(&mut a, vec![shared], 10);
        let small = node(&mut a, vec![shared], 10);
        let large = node(&mut a, vec![small], 100);
        let result = run(
            &mut a,
            &[live],
            &[large, small, small],
            quota(3, 30),
            10_000,
        );
        assert_eq!(result.collection.status, CollectionStatus::Collected);
        assert_eq!(result.mandatory.nodes, 2);
        assert_eq!(result.retained.nodes, 3);
        assert_eq!(result.retained.bytes, 30);
        assert_eq!(result.optional_roots, 2);
        assert!(a.get(shared).is_some() && a.get(live).is_some() && a.get(small).is_some());
        assert!(a.get(large).is_none());
        // Rejection after descendants were staged, not just an oversized root.
        let child = node(&mut a, vec![], 10);
        let huge = node(&mut a, vec![child, small], 10);
        let result = run(&mut a, &[live], &[huge, small], quota(3, 30), 10_000);
        assert_eq!(result.collection.status, CollectionStatus::Collected);
        assert_eq!(result.retained.nodes, 3);
        assert!(a.get(huge).is_none() && a.get(child).is_none() && a.get(small).is_some());
    }
    #[test]
    fn mandatory_over_quota_cycles_and_epoch_wrap_remain_safe() {
        let mut a = Arena::default();
        let live = node(&mut a, vec![], 100);
        a.get_mut(live).unwrap().edges.push(live);
        let garbage = node(&mut a, vec![], 1);
        a.epoch = u64::MAX;
        let result = run(&mut a, &[live], &[garbage], quota(0, 0), 1000);
        assert_eq!(result.collection.status, CollectionStatus::Collected);
        assert_eq!(result.mandatory.nodes, 1);
        assert_eq!(result.retained.bytes, 100);
        assert!(a.get(live).is_some() && a.get(garbage).is_none());
        let cycle = node(&mut a, vec![live], 1);
        a.get_mut(cycle).unwrap().edges.push(cycle);
        assert_eq!(
            run(&mut a, &[live], &[cycle], quota(2, 101), 1000)
                .retained
                .nodes,
            2
        );
    }
    #[test]
    fn every_incomplete_mark_or_sweep_leaves_records_and_generations_intact() {
        for work in 0..20 {
            let mut a = Arena::default();
            let leaf = node(&mut a, vec![], 1);
            let root = node(&mut a, vec![leaf], 1);
            let garbage = node(&mut a, vec![], 1);
            let result = run(&mut a, &[root], &[], quota(2, 2), work);
            if result.collection.status != CollectionStatus::Collected {
                assert_eq!(a.len(), 3);
                assert!(a.get(leaf).is_some() && a.get(root).is_some() && a.get(garbage).is_some());
            }
        }
        let mut a = Arena::default();
        let stale = node(&mut a, vec![], 1);
        run(&mut a, &[], &[], quota(0, 0), 1000);
        let replacement = node(&mut a, vec![], 2);
        let result = run(&mut a, &[stale], &[], quota(2, 2), 1000);
        assert_eq!(result.collection.status, CollectionStatus::InvalidEdge);
        assert!(a.get(replacement).is_some());
        let cancel = AtomicBool::new(true);
        let result = a.select(
            [replacement],
            &[],
            SelectionBudget {
                work: 1000,
                cancellation: &cancel,
                quota: quota(2, 2),
                sweep: 100,
                nodes: 100,
            },
            |node| node.bytes,
            |_| {},
        );
        assert_eq!(result.collection.status, CollectionStatus::Cancelled);
        assert!(a.get(replacement).is_some());
    }
    #[test]
    fn deep_mandatory_graphs_and_wide_optional_trials_use_bounded_worklists() {
        std::thread::Builder::new()
            .stack_size(96 * 1024)
            .spawn(|| {
                let mut a = Arena::default();
                let mut root = node(&mut a, vec![], 1);
                for _ in 0..20_000 {
                    root = node(&mut a, vec![root], 1);
                }
                let wide = node(&mut a, vec![root; 5000], 1);
                let result = run(&mut a, &[root], &[wide], quota(30_000, 30_000), 100_000);
                assert_eq!(result.collection.status, CollectionStatus::Collected);
                assert_eq!(result.retained.nodes, 20_001);
                assert!(
                    a.get(wide).is_none(),
                    "a trial over its work limit is not partially accepted"
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
