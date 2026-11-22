//! Bounded node descriptions reuse the real collector's graph/source-scope rules.
//! Estimates rank candidates; only the subsequent checked transfer grants reuse.
use super::*;
pub(super) const SCRATCH: usize =
    graph::footprint::CACHE_BYTES + graph::footprint::SCRATCH + 128 * 96;

pub(super) fn estimate(
    old: &Session,
    new: &mut Session,
    code: &graph::Code,
    descriptions: &mut graph::footprint::Cache<'_>,
    proofs: &mut evidence::StorageTraversal,
    candidate: (Candidate<'_>, evidence::Proof, usize),
    allowance: u64,
) -> (usize, bool) {
    let (candidate, proof, slots) = candidate;
    let available = allowance.min(new.limits.steps.saturating_sub(new.stats.steps));
    if available < 4 || new.limits.cancellation.load(Ordering::Relaxed) {
        return (0, false);
    }
    // Reserve disjoint graph/proof work. Cached descriptions skip cloning and
    // rediscovering nodes, but never mark a merely priced graph as retained.
    let graph_allowance = available / 2;
    let mut graph_remaining = graph_allowance;
    let graph = descriptions.probe(new, &roots(candidate), &mut graph_remaining);
    let graph_work = graph_allowance - graph_remaining;
    let mut remaining = available.saturating_sub(graph_work).min(1024);
    let before = remaining;
    // Even a graph miss can reveal useful root-proof storage. Only a complete
    // graph description supplies all its nested completion proofs.
    let (proof_bytes, proof_complete) = code.collection.as_ref().unwrap().proof_storage(
        old,
        new,
        graph.proofs.into_iter().chain(std::iter::once(proof)),
        &mut remaining,
        proofs,
    );
    let complete = graph.complete && proof_complete;
    let proof_work = before - remaining;
    let work = graph_work + proof_work;
    new.stats.retention_graph_storage_steps += graph_work;
    new.stats.retention_proof_storage_steps += proof_work;
    new.stats.retention_unknown_graph_storage += u64::from(!graph.complete);
    new.stats.retention_unknown_proof_storage += u64::from(!proof_complete);
    new.stats.steps += work;
    new.stats.retention_storage_steps += work;
    let index = if slots == 0 {
        0
    } else {
        256usize.saturating_add(slots.saturating_mul(64))
    };
    let summary_index = if proof_bytes > 0
        && new.limits.compact_evidence
        && new.evidence_stats().summary_index_bytes == 0
    {
        8192
    } else {
        0
    };
    (
        graph
            .bytes
            .saturating_add(proof_bytes)
            .saturating_add(index)
            .saturating_add(summary_index),
        complete,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn name(n: &str) -> GlobalName {
        GlobalName {
            module: vec!["Test".into()],
            name: n.into(),
        }
    }
    #[test]
    fn descriptions_price_full_closure_scope_without_forcing_or_installing_it() {
        let parsed = knot_syntax::parse("module Test exposing (..)\nmake :: Int -> Int -> Int -> Int\nmake ignored kept later = kept\nbox = make (error \"unused\") 7\nscalar :: Int\nscalar = 7\n").unwrap();
        let canonical = knot_canonical::canonicalize_module(&parsed, None).unwrap();
        let code = knot_core::lower(&[
            knot_checker::check::check_library_program("Prelude").unwrap(),
            knot_checker::check::check_program(&canonical).unwrap(),
        ]);
        let limits = Limits {
            joint_retention: true,
            compact_evidence: true,
            region_evidence: true,
            ..Default::default()
        };
        let mut old = Session::new(Arc::new(code), limits.clone());
        old.evaluate(&name("box")).unwrap();
        old.evaluate(&name("scalar")).unwrap();
        let mut new = Session::empty(old.program.clone(), limits, old.hosts.clone());
        let code = graph::Code::collection(&old, &new, 1_000_000).unwrap();
        let mut maps = graph::Maps::default();
        maps.envs.insert(0, 0);
        let old_work = old.stats.steps;
        let old_heap = old.heap_entries();
        let new_heap = new.heap_entries();
        let mut estimates = Vec::new();
        let mut proofs = evidence::StorageTraversal::default();
        let mut descriptions = graph::footprint::Cache::new(&old, &new, &code, &maps);
        for binding in ["scalar", "box"] {
            let cell = old.globals[&Global::Value(name(binding))];
            let candidate = Candidate::Global(CellId(cell.0));
            let (_, proof, _) = completion(&old, candidate).unwrap_or_else(|| {
                let ThunkState::Done(Ok(done)) = old.thunks[cell.0].state else {
                    panic!("done")
                };
                (done.cost.sample().unwrap(), done.proof, done.value)
            });
            estimates.push(estimate(
                &old,
                &mut new,
                &code,
                &mut descriptions,
                &mut proofs,
                (candidate, proof, 0),
                100_000,
            ));
        }
        assert!(estimates[0].1 && estimates[1].1, "{estimates:?}");
        assert!(
            estimates[1].0 > estimates[0].0,
            "full lexical scope has a storage cost: {estimates:?}"
        );
        assert_eq!(old.stats.steps, old_work);
        assert_eq!(old.heap_entries(), old_heap);
        assert_eq!(new.heap_entries(), new_heap);
        assert_eq!(new.evidence_stats().certificates, 0);
        assert_eq!(code.collection.as_ref().unwrap().work(), 0);
        assert!(new.stats.retention_storage_steps > 0);
    }
}
