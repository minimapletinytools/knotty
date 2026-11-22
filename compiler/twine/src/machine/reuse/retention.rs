//! Cost-guided optional roots for the same collector as explicit live roots.
//! Selection is a bounded heuristic; identity and world stamps establish validity.
use super::*;
use memo::Candidate;
mod footprint;

#[derive(Debug, Clone, Copy)]
pub struct CacheRetention {
    /// Historical useful work; excludes cache-maintenance charges.
    pub minimum_work: u64,
    /// Maximum scored candidates. Discovery and pricing also spend `steps`.
    pub candidates: usize,
    /// Combined call/expression/capture index slots in the collected Session.
    pub slots: usize,
    /// Conservative allocation allowance for optional graph, keys and scratch.
    /// This is not an allocator/RSS measurement; code and mandatory roots remain
    /// independently bounded by the enclosing collection and execution limits.
    pub bytes: usize,
    /// Additional work allowance within the enclosing collection's budget.
    pub steps: u64,
}
impl Default for CacheRetention {
    fn default() -> Self {
        Self {
            minimum_work: 64,
            candidates: 512,
            slots: 16_384,
            bytes: 16 * 1024 * 1024,
            steps: 50_000_000,
        }
    }
}
#[derive(Clone, Copy)]
struct Scored<'a> {
    candidate: Candidate<'a>,
    cost: cost::WorkCost,
    proof: evidence::Proof,
    slots: usize,
    rank: u128,
    order: usize,
}
#[derive(Clone, Copy)]
struct Start {
    entries: usize,
    bytes: usize,
    steps: u64,
    history: u64,
}
impl Start {
    fn new(new: &Session, code: &graph::Code, budget: &graph::Budget) -> Self {
        Self {
            entries: budget.entries,
            bytes: budget.bytes,
            steps: new.stats.steps,
            history: code.collection.as_ref().unwrap().work(),
        }
    }
    fn work(self, new: &Session, code: &graph::Code, budget: &graph::Budget) -> u64 {
        (self.entries - budget.entries) as u64
            + (self.bytes - budget.bytes) as u64
            + new.stats.steps
            - self.steps
            + code.collection.as_ref().unwrap().work()
            - self.history
    }
    fn bytes(self, budget: &graph::Budget) -> usize {
        self.bytes - budget.bytes
    }
}
fn completion(
    old: &Session,
    candidate: Candidate<'_>,
) -> Option<(cost::WorkCost, evidence::Proof, ValueId)> {
    if let Candidate::Global(cell) = candidate {
        if let Some((expression, _)) = old.argument_recipe(old.cell_id(cell.owner())) {
            if matches!(
                old.program.expressions[expression.0].kind,
                Kind::Lit(_)
                    | Kind::Rational(_)
                    | Kind::Local(_)
                    | Kind::Global(_)
                    | Kind::Prim(_)
                    | Kind::Ctor(..)
                    | Kind::Unsupported(_)
            ) {
                return None;
            }
        }
    }
    let (cost, proof, value, stamp) = match candidate {
        Candidate::Call(_, done) => (done.cost, done.proof, done.value, done.stamp),
        Candidate::Expression(_, cell)
        | Candidate::Capture(_, _, cell)
        | Candidate::Global(cell) => {
            let ThunkState::Done(Ok(done)) = old.cell_state(cell.owner()) else {
                return None;
            };
            (done.cost, done.proof, done.value, done.stamp)
        }
    };
    if matches!(old.values[value], Value::Io(..)) {
        return None;
    }
    old.validation
        .current(stamp)
        .then_some((cost.sample()?, proof, value))
}
fn slots(old: &Session, candidate: Candidate<'_>) -> usize {
    match candidate {
        Candidate::Call(key, _) => old
            .call_key_slots_bound(key.function, key.argument)
            .max(key.slots()),
        Candidate::Expression(..) => 1,
        Candidate::Capture(_, key, _) => key.len() + 2,
        Candidate::Global(_) => 0,
    }
}
fn roots(candidate: Candidate<'_>) -> Vec<graph::Root> {
    match candidate {
        Candidate::Call(key, done) => {
            let mut roots = vec![
                graph::Root::Value(key.function),
                graph::Root::Thunk(key.argument.owner()),
                graph::Root::Value(done.value),
            ];
            if let Some(scope) = done.scope.body() {
                roots.push(graph::Root::Env(scope));
            }
            roots
        }
        Candidate::Expression(_, cell)
        | Candidate::Capture(_, _, cell)
        | Candidate::Global(cell) => vec![graph::Root::Thunk(cell.owner())],
    }
}
fn charge(new: &mut Session, work: u64) -> bool {
    if work > new.limits.steps.saturating_sub(new.stats.steps) {
        return false;
    }
    new.stats.steps += work;
    true
}

// Coverage is only a pricing hint. If its bounded scratch is full, omit the
// hint rather than discard a live result or pretend its validity is unknown.
fn cover(
    retained: &mut evidence::RetainedCosts,
    proof: evidence::Proof,
    budget: &mut graph::Budget,
    bytes_left: usize,
    work_left: u64,
) {
    const ENTRY_BYTES: usize = 64;
    if !retained.contains(proof)
        && bytes_left.min(budget.bytes) >= ENTRY_BYTES
        && work_left >= ENTRY_BYTES as u64
    {
        budget.bytes -= ENTRY_BYTES;
        retained.insert(proof);
    }
}

pub(super) fn retain(
    old: &Session,
    new: &mut Session,
    code: &graph::Code,
    maps: &mut graph::Maps,
    budget: &mut graph::Budget,
    policy: CacheRetention,
) {
    let start = Start::new(new, code, budget);
    retain_impl(old, new, code, maps, budget, policy);
    new.stats.retention_steps = start.work(new, code, budget);
}
fn retain_impl(
    old: &Session,
    new: &mut Session,
    code: &graph::Code,
    maps: &mut graph::Maps,
    budget: &mut graph::Budget,
    policy: CacheRetention,
) {
    let start = Start::new(new, code, budget);
    // Reserve peak scratch for one probe at a time, before allocation,
    // including the existing cost-pricing scratch.
    let pricing_bytes = 128 * 96
        + if new.limits.joint_retention {
            footprint::SCRATCH
        } else {
            0
        };
    let available_work = policy
        .steps
        .min(new.limits.steps.saturating_sub(new.stats.steps));
    let cap = policy
        .candidates
        .min(policy.bytes.saturating_sub(pricing_bytes) / std::mem::size_of::<Scored>())
        .min(budget.bytes.saturating_sub(pricing_bytes) / std::mem::size_of::<Scored>())
        .min(
            (available_work.saturating_sub(pricing_bytes as u64)
                / std::mem::size_of::<Scored>() as u64) as usize,
        );
    if cap == 0 {
        return;
    }
    let scratch = cap * std::mem::size_of::<Scored>() + pricing_bytes;
    budget.bytes -= scratch;
    let mut candidates = Vec::with_capacity(cap);
    let mut retained = evidence::RetainedCosts::default();
    // A covered live cell keeps ordinary memoization. Its history need not be
    // charged again as benefit when pricing an optional parent.
    for cell in maps.thunks.keys() {
        if start.work(new, code, budget) >= policy.steps
            || new.limits.cancellation.load(Ordering::Relaxed)
            || !charge(new, 1)
        {
            return;
        }
        if let ThunkState::Done(Ok(done)) = old.thunks[*cell].state {
            if old.validation.current(done.stamp) {
                let bytes_left = policy.bytes.saturating_sub(start.bytes(budget));
                let work_left = policy.steps.saturating_sub(start.work(new, code, budget));
                cover(&mut retained, done.proof, budget, bytes_left, work_left);
            }
        }
    }
    // Pricing must leave fuel for installing candidates, even when every probe
    // is incomplete. This shares the collector's disjoint-allowance convention.
    let storage_allowance = available_work / 4;
    let storage_start = new.stats.retention_storage_steps;
    let mut proof_scratch = evidence::StorageTraversal::default();
    let mut descriptions = new
        .limits
        .joint_retention
        .then(|| graph::footprint::Cache::new(old, new, code, maps));
    let globals = old
        .globals
        .values()
        .map(|thunk| Candidate::Global(CellId(thunk.0)));
    for candidate in globals.chain(old.memo.candidates()) {
        if start.work(new, code, budget) >= policy.steps || !charge(new, 1) {
            break;
        }
        let Some((cost, proof, value)) = completion(old, candidate) else {
            continue;
        };
        if cost.inclusive < policy.minimum_work {
            continue;
        }
        let slots = slots(old, candidate);
        if slots as u64 > policy.steps.saturating_sub(start.work(new, code, budget))
            || !charge(new, slots as u64)
        {
            break;
        }
        let payload = match &old.values[value] {
            Value::Lit(literal) => literal_bytes(literal),
            Value::Native(native) => native.retained_bytes(),
            _ => 0,
        };
        let mut bytes = slots
            .saturating_mul(64)
            .saturating_add(payload)
            .saturating_add(256);
        if new.limits.joint_retention {
            let left = storage_allowance
                .saturating_sub(new.stats.retention_storage_steps - storage_start)
                .min(policy.steps.saturating_sub(start.work(new, code, budget)));
            let (estimate, complete) = footprint::estimate(
                old,
                new,
                code,
                descriptions.as_mut().unwrap(),
                &mut proof_scratch,
                (candidate, proof, slots),
                left,
            );
            // An incomplete traversal supplies only a partial estimate, never a claim
            // that the rest is free. Keep the existing heuristic as its floor.
            bytes = if complete {
                estimate.max(1)
            } else {
                bytes.max(estimate)
            };
            new.stats.retention_storage_queries += 1;
            if !complete {
                new.stats.retention_unknown_storage += 1;
            }
        }
        candidates.push(Scored {
            candidate,
            cost,
            proof,
            slots,
            rank: cost.inclusive as u128 * 1024 / bytes as u128,
            order: candidates.len(),
        });
        if candidates.len() == cap {
            break;
        }
    }
    if let Some(descriptions) = descriptions {
        new.stats.retention_graph_description_hits = descriptions.hits;
        new.stats.retention_graph_description_misses = descriptions.misses;
        new.stats.retention_graph_description_evictions = descriptions.evictions;
        new.stats.retention_graph_description_bytes = descriptions.bytes;
    }
    drop(proof_scratch);
    let sorting = candidates
        .len()
        .saturating_mul((candidates.len().max(1).ilog2() + 1) as usize) as u64;
    if sorting > policy.steps.saturating_sub(start.work(new, code, budget)) || !charge(new, sorting)
    {
        return;
    }
    // Discovery order is a unique tie-breaker; in-place sorting preserves that
    // order without an additional candidate-sized allocation.
    candidates.sort_unstable_by(|a, b| b.rank.cmp(&a.rank).then(a.order.cmp(&b.order)));
    let capacity = policy.slots.min(new.limits.application_cache_entries);
    for scored in candidates {
        let work_left = policy.steps.saturating_sub(start.work(new, code, budget));
        let bytes_left = policy
            .bytes
            .saturating_sub(start.bytes(budget))
            .min(budget.bytes);
        if work_left == 0 || bytes_left == 0 || new.limits.cancellation.load(Ordering::Relaxed) {
            break;
        }
        new.stats.retention_candidates += 1;
        if new.memo.len().saturating_add(scored.slots) > capacity {
            continue;
        }
        let mut pricing = work_left
            .min(new.limits.steps.saturating_sub(new.stats.steps))
            .min(1024);
        let before = pricing;
        let estimate = old
            .evidence
            .estimate_cost(scored.proof, &retained, 128, &mut pricing);
        new.stats.steps += before - pricing;
        let benefit = if estimate.complete {
            estimate.own_work
        } else {
            new.stats.retention_unknown_costs += 1;
            scored.cost.inclusive
        };
        if benefit < policy.minimum_work {
            continue;
        }
        let index_bytes = if scored.slots == 0 {
            0
        } else {
            256 + scored.slots * 64
        };
        let work_left = policy.steps.saturating_sub(start.work(new, code, budget));
        if index_bytes as u64 >= work_left / 4 || index_bytes >= bytes_left {
            continue;
        }
        let mut attempt = graph::Budget {
            entries: budget.entries.min((work_left / 4) as usize),
            bytes: bytes_left
                .saturating_sub(index_bytes)
                .min((work_left / 4) as usize),
        };
        let initial = (attempt.entries, attempt.bytes);
        let heap_limit = new.limits.heap_entries;
        new.limits.heap_entries = heap_limit.saturating_sub(scored.slots);
        let copied =
            graph::transfer_roots(old, new, code, maps, &roots(scored.candidate), &mut attempt);
        new.limits.heap_entries = heap_limit;
        budget.entries -= initial.0 - attempt.entries;
        budget.bytes -= initial.1 - attempt.bytes;
        let Some(copied) = copied else {
            continue;
        };
        // Proofs and supporting identity allocations share the optional byte
        // allowance. Incomplete proof remains unknown; a current result is valid.
        let bytes_left = policy
            .bytes
            .saturating_sub(start.bytes(budget))
            .min(budget.bytes)
            .saturating_sub(index_bytes);
        let work_left = policy.steps.saturating_sub(start.work(new, code, budget));
        let proof_bytes = bytes_left.min((work_left / 4) as usize);
        let old_limit = new.limits.dependency_bytes;
        let before_proofs = new.evidence_stats().bytes;
        new.limits.dependency_bytes = old_limit.min(before_proofs.saturating_add(proof_bytes));
        let history = code.collection.as_ref().unwrap();
        let old_history_limit =
            history.set_work_limit(history.work().saturating_add(work_left / 4));
        copied.commit_with_proofs(new, maps, old, code);
        let call_proof = match scored.candidate {
            Candidate::Call(_, done) => code.transferred_proof(old, new, done.proof),
            _ => Default::default(),
        };
        history.set_work_limit(old_history_limit);
        new.limits.dependency_bytes = old_limit;
        budget.bytes -= new.evidence_stats().bytes - before_proofs;
        budget.bytes -= index_bytes;
        let support_before = new.identity_support.bytes();
        let work_left = policy.steps.saturating_sub(start.work(new, code, budget));
        // Both constructing a key and retaining its new support consume work.
        // Separate allowances prevent each from spending the entire remainder.
        let available = policy
            .bytes
            .saturating_sub(start.bytes(budget))
            .min(budget.bytes)
            .min((work_left / 2) as usize);
        let support_limit = new
            .identity_support
            .set_local_ceiling(support_before.saturating_add(available));
        let step_limit = new.limits.steps;
        new.limits.steps = step_limit.min(new.stats.steps.saturating_add(work_left / 2));
        let admitted = install(
            new,
            maps,
            scored.candidate,
            capacity,
            scored.slots,
            call_proof,
        );
        new.limits.steps = step_limit;
        new.identity_support.set_local_ceiling(support_limit);
        budget.bytes -= new.identity_support.bytes().saturating_sub(support_before);
        if admitted {
            new.stats.retention_admitted += 1;
            let bytes_left = policy.bytes.saturating_sub(start.bytes(budget));
            let work_left = policy.steps.saturating_sub(start.work(new, code, budget));
            cover(&mut retained, scored.proof, budget, bytes_left, work_left);
        } else {
            // Bounded floating garbage is reclaimed by the next collection; a
            // late identity/index miss cannot grant reuse or exceed the budget.
            new.stats.retention_index_rejections += 1;
        }
    }
}
fn install(
    new: &mut Session,
    maps: &graph::Maps,
    candidate: Candidate<'_>,
    capacity: usize,
    bound: usize,
    proof: evidence::Proof,
) -> bool {
    match candidate {
        Candidate::Global(_) => true,
        Candidate::Expression(expression, cell) => {
            new.memo
                .remember_expression(expression, new.cell_id(maps.thunks[&cell.0]), capacity);
            true
        }
        Candidate::Capture(expression, key, cell) => {
            let key = key
                .iter()
                .map(|(local, cell)| Some((*local, new.cell_id(*maps.thunks.get(&cell.0)?))))
                .collect::<Option<Vec<_>>>();
            key.is_some_and(|key| {
                new.memo.remember_capture(
                    expression,
                    key,
                    new.cell_id(maps.thunks[&cell.0]),
                    capacity,
                )
            })
        }
        Candidate::Call(key, done) => {
            let function = maps.values[&key.function];
            let argument = new.cell_id(maps.thunks[&key.argument.0]);
            let key = new.call_key(function, argument);
            assert!(
                key.slots() <= bound,
                "prepared layout must bound a receiving key"
            );
            new.memo.insert_completion(
                key,
                memo::Completion {
                    #[cfg(feature = "incremental")]
                    access: Default::default(),
                    value: maps.values[&done.value],
                    cost: done.cost,
                    proof,
                    stamp: done.stamp,
                    scope: done
                        .scope
                        .body()
                        .map(|scope| memo::Scope::new(maps.envs[&scope]))
                        .unwrap_or_default(),
                },
                capacity,
            )
        }
    }
}
