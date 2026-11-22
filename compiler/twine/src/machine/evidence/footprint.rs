//! Marginal proof storage relative to an exact collection's imported records.
//! This estimate never certifies validity or computation-cost coverage.
use super::*;
#[derive(Debug, PartialEq, Eq)]
pub(in crate::machine) struct StorageEstimate {
    pub bytes: usize,
    pub complete: bool,
}
/// One bounded workspace per collection's candidate-discovery pass. Records
/// already hold immutable byte/edge descriptions; no second copy is needed.
/// Only allocations survive between queries, never visited/covered membership.
#[derive(Default)]
pub(in crate::machine) struct StorageTraversal {
    pending: Vec<Proof>,
    seen: std::collections::HashSet<storage::Id>,
}
impl Store {
    pub(in crate::machine) fn estimate_storage(
        &self,
        destination: &Store,
        roots: impl IntoIterator<Item = Proof>,
        imported: &HashMap<storage::Id, Proof>,
        budget: (usize, &mut u64, &AtomicBool),
        scratch: &mut StorageTraversal,
    ) -> StorageEstimate {
        let (visits, remaining, cancellation) = budget;
        let visits = visits.min(128);
        let mut result = StorageEstimate {
            bytes: 0,
            complete: true,
        };
        let StorageTraversal { pending, seen } = scratch;
        pending.clear();
        seen.clear();
        for root in roots {
            if *remaining == 0 || pending.len() >= visits || cancellation.load(Ordering::Relaxed) {
                result.complete = false;
                return result;
            }
            *remaining -= 1;
            pending.push(root);
        }
        while let Some(proof) = pending.pop() {
            if *remaining == 0 || seen.len() >= visits || cancellation.load(Ordering::Relaxed) {
                result.complete = false;
                break;
            }
            *remaining -= 1;
            let Some(id) = proof.id else {
                result.complete = false;
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            // Only an extant imported proof covers this storage. An earlier
            // incomplete import is not a zero-byte success.
            if let Some(mapped) = imported.get(&id) {
                if destination.get(*mapped).is_some() {
                    continue;
                }
                result.complete = false;
            }
            let Some(record) = self.get(proof) else {
                result.complete = false;
                continue;
            };
            result.bytes = result.bytes.saturating_add(record.bytes).saturating_add(64);
            if record.reads.len() > visits.saturating_sub(pending.len())
                || record.reads.len() as u64 > *remaining
            {
                result.complete = false;
                break;
            }
            *remaining -= record.reads.len() as u64;
            pending.extend(record.reads.iter().copied());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(store: &mut Store, reads: Vec<Proof>) -> Proof {
        store.insert_record(
            Recipe::Historical(None),
            None,
            reads.clone(),
            Default::default(),
            BASE_BYTES + reads.len() * READ_BYTES,
            &Limits::default(),
        )
    }
    #[test]
    fn diamonds_count_shared_storage_once_and_stop_at_extant_imports() {
        let mut old = Store::default();
        let leaf = record(&mut old, vec![]);
        let a = record(&mut old, vec![leaf]);
        let b = record(&mut old, vec![leaf]);
        let root = record(&mut old, vec![a, b]);
        let mut new = Store::default();
        let mut imported = HashMap::new();
        let cancel = AtomicBool::new(false);
        let estimate = old.estimate_storage(
            &new,
            [root],
            &imported,
            (128, &mut 1000, &cancel),
            &mut StorageTraversal::default(),
        );
        assert!(estimate.complete);
        assert_eq!(estimate.bytes, 4 * (BASE_BYTES + 64) + 4 * READ_BYTES);
        let mapped = record(&mut new, vec![]);
        imported.insert(leaf.id.unwrap(), mapped);
        let estimate = old.estimate_storage(
            &new,
            [root, root],
            &imported,
            (128, &mut 1000, &cancel),
            &mut StorageTraversal::default(),
        );
        assert!(estimate.complete);
        assert_eq!(estimate.bytes, 3 * (BASE_BYTES + 64) + 4 * READ_BYTES);
        imported.insert(root.id.unwrap(), mapped);
        let estimate = old.estimate_storage(
            &new,
            [root],
            &imported,
            (128, &mut 1000, &cancel),
            &mut StorageTraversal::default(),
        );
        assert!(estimate.complete);
        assert_eq!(estimate.bytes, 0);
        imported.insert(root.id.unwrap(), Proof::default());
        assert!(
            !old.estimate_storage(
                &new,
                [root],
                &imported,
                (128, &mut 1000, &cancel),
                &mut StorageTraversal::default()
            )
            .complete
        );
    }
    #[test]
    fn missing_wide_and_interrupted_history_never_claim_complete_storage() {
        let mut old = Store::default();
        let missing = record(&mut old, vec![Proof::default()]);
        let leaf = record(&mut old, vec![]);
        let wide = record(&mut old, vec![leaf; 129]);
        let new = Store::default();
        let imported = HashMap::new();
        let cancel = AtomicBool::new(false);
        for root in [missing, wide] {
            let mut work = 1000;
            assert!(
                !old.estimate_storage(
                    &new,
                    [root],
                    &imported,
                    (128, &mut work, &cancel),
                    &mut StorageTraversal::default()
                )
                .complete
            );
            assert!(work < 1000);
        }
        assert!(
            !old.estimate_storage(
                &new,
                [leaf],
                &imported,
                (128, &mut 0, &cancel),
                &mut StorageTraversal::default()
            )
            .complete
        );
        cancel.store(true, Ordering::Relaxed);
        let mut work = 1000;
        assert!(
            !old.estimate_storage(
                &new,
                [leaf],
                &imported,
                (128, &mut work, &cancel),
                &mut StorageTraversal::default()
            )
            .complete
        );
        assert_eq!(work, 1000);
    }
    #[test]
    fn reused_workspace_clears_coverage_after_truncation_cancellation_and_changed_imports() {
        let mut old = Store::default();
        let leaf = record(&mut old, vec![]);
        let mut root = leaf;
        for _ in 0..40 {
            root = record(&mut old, vec![root, leaf]);
        }
        let wide = record(&mut old, vec![root; 129]);
        let missing = record(&mut old, vec![Proof::default()]);
        let mut new = Store::default();
        let mut imported = HashMap::new();
        let mut reused = StorageTraversal::default();
        let cancel = AtomicBool::new(false);
        for phase in 0..4 {
            match phase {
                1 => {
                    new.records.checkpoint();
                    let mapped = record(&mut new, vec![]);
                    imported.insert(root.id.unwrap(), mapped);
                }
                2 => new.records.rollback(), // Same import map, now a stale generation.
                3 => {
                    imported.clear();
                }
                _ => {}
            }
            for cancelled in [true, false] {
                cancel.store(cancelled, Ordering::Relaxed);
                for fuel in [0, 1, 4, 32, 1024] {
                    for roots in [[root, root], [wide, leaf], [missing, root], [leaf, root]] {
                        let mut a = fuel;
                        let mut b = fuel;
                        let expected = old.estimate_storage(
                            &new,
                            roots,
                            &imported,
                            (128, &mut a, &cancel),
                            &mut StorageTraversal::default(),
                        );
                        let actual = old.estimate_storage(
                            &new,
                            roots,
                            &imported,
                            (128, &mut b, &cancel),
                            &mut reused,
                        );
                        assert_eq!((actual, b), (expected, a));
                        assert!(reused.pending.len() <= 128);
                        assert!(reused.seen.len() <= 128);
                        // Standard Vec/HashSet growth fits the existing 128 * 96 scratch allowance.
                        assert!(reused.pending.capacity() <= 128);
                        assert!(reused.seen.capacity() <= 256);
                    }
                }
            }
        }
        let mut fuel = 1024;
        let estimate = old.estimate_storage(
            &new,
            [root],
            &imported,
            (128, &mut fuel, &cancel),
            &mut reused,
        );
        assert!(estimate.complete);
        assert!(
            estimate.bytes > BASE_BYTES + 64,
            "previously priced is not retained"
        );
    }

    #[test]
    #[ignore = "release-only traversal benchmark; run serially with --nocapture"]
    fn storage_traversal_benchmark() {
        use std::{hint::black_box, time::Instant};
        let mut old = Store::default();
        let leaf = record(&mut old, vec![]);
        let mut common = leaf;
        for _ in 0..48 {
            common = record(&mut old, vec![common, leaf]);
        }
        let roots: Vec<_> = (0..512).map(|_| record(&mut old, vec![common])).collect();
        let new = Store::default();
        let imported = HashMap::new();
        let cancel = AtomicBool::new(false);
        let mut oracle = None;
        for reuse in [false, true, true, false, false, true] {
            let mut scratch = StorageTraversal::default();
            let start = Instant::now();
            let mut total = (0usize, 0u64);
            for _ in 0..64 {
                for root in &roots {
                    let mut fuel = 1024;
                    let mut fresh = StorageTraversal::default();
                    let estimate = old.estimate_storage(
                        &new,
                        [*root],
                        &imported,
                        (128, &mut fuel, &cancel),
                        if reuse { &mut scratch } else { &mut fresh },
                    );
                    assert!(estimate.complete);
                    total.0 += black_box(estimate.bytes);
                    total.1 += 1024 - fuel;
                }
            }
            assert_eq!(*oracle.get_or_insert(total), total);
            println!(
                "proof_scratch reuse={reuse} ns={} bytes={} work={}",
                start.elapsed().as_nanos(),
                total.0,
                total.1
            );
        }
    }
}
