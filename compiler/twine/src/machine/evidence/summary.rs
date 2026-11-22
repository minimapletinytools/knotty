//! Exact sharing of code/read validity, independent of computation identity.
//! Summaries retain ordered child proofs and executable IDs. A weak bounded
//! index only finds candidates: collisions and reclaimed generations must match
//! the complete description before reuse. No runtime value or cost is shared.
use super::*;
use std::hash::{Hash, Hasher};

/// Static validity guard only. Demanded input dependencies remain in ordered reads;
/// their actual computation identities remain on the runtime cells/call keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::machine) enum SummaryCode {
    Expression(ExprId),
    Call(LocalId, ExprId),
    /// Complete syntactic lambda version; inputs are still separate reads.
    Region(ExprId),
    Immutable,
}
impl SummaryCode {
    pub(in crate::machine) fn expression(self) -> Option<ExprId> {
        match self {
            Self::Expression(expr) | Self::Call(_, expr) | Self::Region(expr) => Some(expr),
            Self::Immutable => None,
        }
    }
}

const MAX_SLOTS: usize = 1024;

impl Store {
    pub(super) fn insert_summary(
        &mut self,
        code: SummaryCode,
        mut reads: Vec<Proof>,
        limits: &Limits,
        work: &mut u64,
    ) -> Proof {
        self.normalize_region_reads(code, &mut reads, limits, work);
        let lookup_work = 1u64.saturating_add((reads.len() as u64).saturating_mul(2));
        if limits.cancellation.load(Ordering::Relaxed)
            || lookup_work > limits.steps.saturating_sub(*work)
        {
            self.stats.unrecorded = self.stats.unrecorded.saturating_add(1);
            return Proof::default();
        }
        *work += lookup_work;
        let bytes = BASE_BYTES.saturating_add(reads.len().saturating_mul(READ_BYTES));
        if limits.compact_evidence
            && self.summary_index.is_empty()
            && self.room(reads.len(), bytes, limits)
        {
            let available = limits
                .dependency_bytes
                .saturating_sub(self.stats.bytes)
                .saturating_sub(self.active_bytes)
                .saturating_sub(bytes); // the optional index must leave room for its record
            let slots = MAX_SLOTS
                .min(limits.dependency_records)
                .min(available / std::mem::size_of::<Proof>());
            if slots > 0 && slots as u64 <= limits.steps.saturating_sub(*work) {
                *work += slots as u64;
                self.summary_index = vec![Proof::default(); slots];
                let bytes = self.summary_index.capacity() * std::mem::size_of::<Proof>();
                self.stats.bytes += bytes;
                self.stats.summary_index_bytes = bytes;
            }
        }
        let slot = if limits.compact_evidence && !self.summary_index.is_empty() {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            code.hash(&mut hash);
            for read in &reads {
                read.id.hash(&mut hash);
            }
            let slot = hash.finish() as usize % self.summary_index.len();
            let candidate = self.summary_index[slot];
            if self.get(candidate).is_some_and(|record| {
                matches!(record.recipe, Recipe::Summary(stored) if stored == code)
                    && record.reads.as_ref() == reads.as_slice()
            }) {
                self.stats.summary_hits = self.stats.summary_hits.saturating_add(1);
                return candidate;
            }
            Some(slot)
        } else {
            None
        };
        let proof = self.insert_record(
            Recipe::Summary(code),
            None,
            reads,
            Default::default(),
            bytes,
            limits,
        );
        if let Some(slot) = slot {
            self.summary_index[slot] = proof;
        }
        proof
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            compact_evidence: true,
            ..Default::default()
        }
    }

    #[test]
    fn summaries_share_validity_without_sharing_results_costs_or_read_order() {
        let limits = limits();
        let mut store = Store::default();
        let mut work = 0;
        let a = store.insert_summary(
            SummaryCode::Expression(ExprId(1)),
            vec![],
            &limits,
            &mut work,
        );
        let b = store.insert_summary(
            SummaryCode::Expression(ExprId(2)),
            vec![],
            &limits,
            &mut work,
        );
        let first = store.insert_summary(
            SummaryCode::Expression(ExprId(3)),
            vec![a, b],
            &limits,
            &mut work,
        );
        assert_eq!(
            first,
            store.insert_summary(
                SummaryCode::Expression(ExprId(3)),
                vec![a, b],
                &limits,
                &mut work
            )
        );
        assert_ne!(
            first,
            store.insert_summary(
                SummaryCode::Expression(ExprId(3)),
                vec![b, a],
                &limits,
                &mut work
            )
        );
        let record = store.get(first).unwrap();
        assert!(record.result.is_none());
        assert!(record.cost.sample().is_none());
        assert_eq!(store.stats.summary_hits, 1);
        let mut retained = RetainedCosts::default();
        retained.insert(first);
        let estimate = store.estimate_cost(first, &retained, 128, &mut 1000);
        assert!(
            !estimate.complete,
            "same validity does not mean the computation is retained"
        );
    }

    #[test]
    fn collisions_and_reclaimed_generations_do_not_alias_summaries() {
        let limits = limits();
        // Force every index candidate to collide.
        let mut store = Store {
            summary_index: vec![Proof::default()],
            stats: EvidenceStats {
                bytes: std::mem::size_of::<Proof>(),
                summary_index_bytes: std::mem::size_of::<Proof>(),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut work = 0;
        let a = store.insert_summary(
            SummaryCode::Expression(ExprId(1)),
            vec![],
            &limits,
            &mut work,
        );
        let b = store.insert_summary(
            SummaryCode::Expression(ExprId(2)),
            vec![],
            &limits,
            &mut work,
        );
        assert_ne!(a, b);
        let stats = &mut store.stats;
        store
            .records
            .collect([], 1000, &limits.cancellation, |record| {
                stats.bytes -= record.bytes;
                stats.reads -= record.reads.len();
                stats.certificates -= 1;
                stats.summaries -= 1;
            });
        assert!(store.get(a).is_none());
        assert!(store.get(b).is_none());
        let c = store.insert_summary(
            SummaryCode::Expression(ExprId(2)),
            vec![],
            &limits,
            &mut work,
        );
        assert_ne!(b, c);
        assert_eq!(store.stats.summary_hits, 0);
    }

    #[test]
    fn rollback_and_bounds_include_the_weak_index() {
        let limits = limits();
        let mut store = Store::default();
        let mut work = 0;
        let empty = store.snapshot();
        let discarded = store.insert_summary(
            SummaryCode::Expression(ExprId(1)),
            vec![],
            &limits,
            &mut work,
        );
        assert!(discarded.id.is_some());
        store.rollback(empty);
        assert!(store.summary_index.is_empty());
        assert_eq!(store.stats.bytes, 0);
        assert!(store.get(discarded).is_none());
        let kept = store.insert_summary(
            SummaryCode::Expression(ExprId(2)),
            vec![],
            &limits,
            &mut work,
        );
        let before = store.stats.bytes;
        let checkpoint = store.snapshot();
        store.insert_summary(
            SummaryCode::Expression(ExprId(3)),
            vec![],
            &limits,
            &mut work,
        );
        store.rollback(checkpoint);
        assert!(store.get(kept).is_some());
        assert_eq!(store.stats.bytes, before);
        let tiny = Limits {
            dependency_bytes: 0,
            ..limits
        };
        let mut empty = Store::default();
        assert!(empty
            .insert_summary(SummaryCode::Expression(ExprId(1)), vec![], &tiny, &mut work)
            .id
            .is_none());
        assert_eq!(empty.stats.bytes, 0);
        assert!(empty.summary_index.is_empty());
    }

    #[test]
    fn optional_index_does_not_displace_a_record_that_fits_the_byte_budget() {
        let limits = Limits {
            dependency_bytes: BASE_BYTES,
            ..limits()
        };
        let mut store = Store::default();
        let proof =
            store.insert_summary(SummaryCode::Expression(ExprId(1)), vec![], &limits, &mut 0);
        assert!(proof.id.is_some());
        assert!(store.summary_index.is_empty());
        assert_eq!(store.stats.bytes, BASE_BYTES);
    }

    #[test]
    fn immutable_receipts_do_not_retain_or_identify_their_values() {
        let limits = limits();
        let mut store = Store::default();
        let mut work = 0;
        let first = store.introduced(CellId(1), 10, &limits, &mut work);
        let second = store.introduced(CellId(2), 20, &limits, &mut work);
        assert_eq!(first, second);
        let record = store.get(first).unwrap();
        assert!(record.result.is_none());
        assert!(record.reads.is_empty());
        assert!(matches!(
            record.recipe,
            Recipe::Summary(SummaryCode::Immutable)
        ));
    }

    #[test]
    fn call_guards_remap_parameters_and_bodies_without_becoming_expression_guards() {
        let limits = limits();
        let mut source = Store::default();
        let mut work = 0;
        let call = source.insert_summary(
            SummaryCode::Call(LocalId(1), ExprId(2)),
            vec![],
            &limits,
            &mut work,
        );
        let expr = source.insert_summary(
            SummaryCode::Expression(ExprId(2)),
            vec![],
            &limits,
            &mut work,
        );
        assert_ne!(call, expr);
        let mut target = Store::default();
        let imported = target.import_proof(
            &source,
            call,
            &mut HashMap::new(),
            (&limits, &mut work),
            |recipe| match recipe {
                Recipe::Summary(SummaryCode::Call(LocalId(1), ExprId(2))) => {
                    Some(SummaryCode::Call(LocalId(11), ExprId(22)))
                }
                _ => None,
            },
        );
        assert!(matches!(
            target.get(imported).unwrap().recipe,
            Recipe::Summary(SummaryCode::Call(LocalId(11), ExprId(22)))
        ));
        let missing = target.import_proof(
            &source,
            call,
            &mut HashMap::new(),
            (&limits, &mut work),
            |_| None,
        );
        assert!(missing.id.is_none());
    }

    #[test]
    fn unversioned_host_reads_and_cancelled_recording_remain_unknown() {
        let limits = limits();
        let name = GlobalName {
            module: vec!["Input".into()],
            name: "read".into(),
        };
        let mut registry = HostRegistry::new();
        registry
            .register_function(
                name.clone(),
                knot_core::HostSignature {
                    parameters: vec![],
                    result: knot_core::HostType::Int,
                },
                |_, _| Ok(crate::HostValue::Int(1)),
            )
            .unwrap();
        let mut store = Store::default();
        let mut recorder = Recorder::default();
        let mut work = 0;
        recorder.begin_call(
            1,
            CellId(2),
            (LocalId(3), ExprId(4)),
            &mut store,
            &limits,
            &mut work,
        );
        recorder.host(
            &name,
            registry.function(&name).unwrap(),
            5,
            &mut store,
            &limits,
            &mut work,
        );
        assert!(recorder
            .finish(6, Default::default(), &mut store, &limits, &mut work)
            .id
            .is_none());
        limits.cancellation.store(true, Ordering::Relaxed);
        let before = store.stats.bytes;
        assert!(store
            .insert_summary(SummaryCode::Immutable, vec![], &limits, &mut work)
            .id
            .is_none());
        assert_eq!(store.stats.bytes, before);
    }

    #[test]
    fn incomplete_reads_never_become_empty_summaries() {
        let limits = limits();
        let mut store = Store::default();
        let mut recorder = Recorder::default();
        let mut work = 0;
        recorder.begin_thunk(
            CellId(0),
            &Pending::Expr(ExprId(0), 0),
            &mut store,
            &limits,
            &mut work,
        );
        recorder.hit(Proof::default(), &mut store, &limits, &mut work);
        assert!(recorder
            .finish(0, Default::default(), &mut store, &limits, &mut work)
            .id
            .is_none());
        assert_eq!(store.stats.summaries, 0);
    }
}
