//! Optional result proofs compete for retained evidence space. The storage arena
//! validates whole dependency closures while marking, so selection does not need
//! a dry graph copy or a second proof walk before the sweep.
use super::*;
use storage::selection::{SelectionBudget, Usage};

pub(super) fn collect(
    session: &mut Session,
    mandatory: std::collections::HashSet<storage::Id>,
    policy: EvidencePressure,
    allowance: u64,
) -> EvidenceCollection {
    let mut left = allowance;
    let cancellation = session.limits.cancellation.clone();
    let mut candidates = std::collections::BinaryHeap::new();
    let mut order = 0u64;
    const CANDIDATES: usize = 512;
    // At most two arena-sized ID vectors during marking, one during sweep,
    // plus root discovery and the bounded candidate heap. No runtime graph copy.
    let scan = session.evidence.records.slot_count();
    let scratch = scan
        .saturating_mul(64)
        .saturating_add(16_384 * 64)
        .saturating_add(CANDIDATES * 64);
    let failed = |status| EvidenceCollection {
        status,
        work: allowance - left,
        reclaimed: 0,
        remaining: session.evidence.records.len(),
    };
    if scratch > policy.scratch_bytes {
        return failed(EvidenceCollectionStatus::Truncated);
    }
    let pruning = session.memo.eviction_work_bound(usize::MAX);
    if pruning > left {
        return failed(EvidenceCollectionStatus::Truncated);
    }
    // Reserve index pruning before any sweep. Rejected plans do not mutate indexes.
    let mut discover = |proof: Proof, cost: cost::Stored| -> Result<(), EvidenceCollectionStatus> {
        if cancellation.load(Ordering::Relaxed) {
            return Err(EvidenceCollectionStatus::Cancelled);
        }
        if left < 20 {
            return Err(EvidenceCollectionStatus::Truncated);
        }
        left -= 20; // Root lookup plus bounded heap comparisons (512 entries).
        let Some(record) = session.evidence.get(proof) else {
            return Ok(());
        };
        // Inclusive measured benefit is a heuristic, never deduplicated by proof ID.
        // Immediate proof bytes rank candidates; actual full storage is checked below.
        let benefit = cost.sample().map_or(1, |cost| cost.inclusive.max(1));
        let rank = benefit as u128 * 1024 / record.bytes.saturating_add(64).max(1) as u128;
        let key = (rank, std::cmp::Reverse(order), proof.id.unwrap());
        order += 1;
        if candidates.len() < CANDIDATES {
            candidates.push(std::cmp::Reverse(key));
        } else if candidates.peek().is_some_and(|smallest| key > smallest.0) {
            *candidates.peek_mut().unwrap() = std::cmp::Reverse(key);
        }
        Ok(())
    };
    let discovered = (|| {
        for cell in session.globals.values() {
            if let ThunkState::Done(Ok(done)) = session.cell_state(*cell) {
                discover(done.proof, done.cost)?;
            } else {
                // Unfinished globals still cost discovery work; do not force them.
                discover(Proof::default(), Default::default())?;
            }
        }
        for candidate in session.memo.candidates() {
            let (proof, cost) = completion(&session.thunks, candidate);
            discover(proof, cost)?;
        }
        Ok(())
    })();
    if let Err(status) = discovered {
        return EvidenceCollection {
            status,
            work: allowance - left,
            reclaimed: 0,
            remaining: session.evidence.records.len(),
        };
    }
    let sort = (candidates.len() * 20) as u64;
    if sort.saturating_add(pruning) > left {
        return EvidenceCollection {
            status: EvidenceCollectionStatus::Truncated,
            work: allowance - left,
            reclaimed: 0,
            remaining: session.evidence.records.len(),
        };
    }
    left -= sort;
    // Reverse-wrapped min-heap sorts into highest benefit first. Discovery order
    // breaks ties; numeric proof IDs never decide between different discoveries.
    let optional: Vec<_> = candidates
        .into_sorted_vec()
        .into_iter()
        .map(|candidate| candidate.0 .2)
        .collect();
    let target =
        |limit: usize, reserved: usize| limit.saturating_sub(limit / 4).saturating_sub(reserved);
    let evidence = &mut session.evidence;
    let metadata = evidence
        .stats
        .summary_index_bytes
        .saturating_add(evidence.stats.region_bytes);
    let quota = Usage {
        nodes: target(session.limits.dependency_records, evidence.active_records),
        edges: target(session.limits.dependency_reads, evidence.active_reads),
        bytes: target(
            session.limits.dependency_bytes,
            evidence.active_bytes.saturating_add(metadata),
        ),
        sweep: u64::MAX,
    };
    let stats = &mut evidence.stats;
    let selected = evidence.records.select(
        mandatory,
        &optional,
        SelectionBudget {
            work: left - pruning,
            cancellation: &cancellation,
            quota,
            sweep: (stats.certificates as u64).saturating_add(stats.reads as u64),
            nodes: session.limits.dependency_records.max(stats.certificates),
        },
        |record| record.bytes,
        |record| {
            stats.reads -= record.reads.len();
            stats.bytes -= record.bytes;
            stats.certificates -= 1;
            stats.summaries -= usize::from(matches!(record.recipe, Recipe::Summary(_)));
        },
    );
    let mut report = selected.collection;
    report.work += allowance - left;
    session.stats.evidence_pressure_mandatory_records = session
        .stats
        .evidence_pressure_mandatory_records
        .max(selected.mandatory.nodes as u64);
    if report.status == EvidenceCollectionStatus::Collected {
        session.stats.evidence_pressure_selected_roots += selected.optional_roots as u64;
        session.stats.evidence_pressure_selected_records = session
            .stats
            .evidence_pressure_selected_records
            .max(selected.retained.nodes as u64);
        // The complete sweep and bounded index pass are one commit. No user code
        // or fallible/budgeted work occurs between them. Ordinary thunks stay memoized.
        let thunks = &session.thunks;
        let evidence = &session.evidence;
        let removed = session.memo.retain_candidates(|candidate| {
            let (proof, _) = completion(thunks, candidate);
            evidence.get(proof).is_some()
        });
        report.work += pruning;
        session.stats.evidence_pressure_evicted_indexes += removed as u64;
        session.stats.cache_evictions += removed as u64;
    }
    report
}

fn completion(thunks: &Heap<Thunk>, candidate: memo::Candidate<'_>) -> (Proof, cost::Stored) {
    match candidate {
        memo::Candidate::Call(_, done) => (done.proof, done.cost),
        memo::Candidate::Expression(_, cell)
        | memo::Candidate::Capture(_, _, cell)
        | memo::Candidate::Global(cell) => match &thunks[cell.0].state {
            ThunkState::Done(Ok(done)) => (done.proof, done.cost),
            _ => (Proof::default(), Default::default()),
        },
    }
}
