//! Bounded transfer of pure lazy graphs into a fresh session. Executable, lexical
//! and global dependencies must match exactly; no old heap IDs escape transfer.
use super::*;
mod captures;
#[cfg(feature = "incremental")]
mod collection;
#[cfg(feature = "incremental")]
mod retention;
#[cfg(feature = "incremental")]
pub use retention::CacheRetention;
mod graph;
#[cfg(feature = "incremental")]
pub use collection::{CollectionBudget, RetainedRoot};
#[cfg(feature = "incremental")]
mod projection;
#[cfg(feature = "incremental")]
mod source;

#[derive(Debug, Clone)]
pub struct ReuseLimits {
    pub code_nodes: usize,
    /// Independent bound on heap cells scanned for reusable expression provenance.
    pub observed_thunks: usize,
    pub globals: usize,
    pub heap_entries: usize,
    pub payload_bytes: usize,
}
impl Default for ReuseLimits {
    fn default() -> Self {
        Self {
            code_nodes: 100_000,
            observed_thunks: 10_000,
            globals: 1_024,
            heap_entries: 10_000,
            payload_bytes: 1024 * 1024,
        }
    }
}
#[derive(Debug, Default, Clone, Copy)]
pub struct ReuseStats {
    /// Pages shared by a revision fork; zero for copied transfers/rebases.
    #[cfg(feature = "incremental")]
    pub shared_heap_pages: usize,
    /// Actual ordered-read verification work during development source transfer.
    #[cfg(feature = "incremental")]
    pub validation_steps: u64,
    #[cfg(feature = "incremental")]
    pub evidence_steps: u64,
    #[cfg(feature = "incremental")]
    pub preparation_steps: u64,
    #[cfg(feature = "incremental")]
    pub graph_steps: u64,
    pub observed_thunks: usize,
    pub transferred_thunks: usize,
    pub transferred_values: usize,
    pub transferred_environments: usize,
    pub globals: usize,
    pub applications: usize,
    pub expressions: usize,
    pub captures: usize,
    pub heap_entries: usize,
    pub payload_bytes: usize,
}
impl ReuseStats {
    fn record_graph(&mut self, import: &graph::Import) {
        let (thunks, values, environments) = import.counts();
        self.transferred_thunks += thunks;
        self.transferred_values += values;
        self.transferred_environments += environments;
    }
}
impl Session {
    /// Transfer bounded pure structures, lazy fields, closures/dictionaries and
    /// compatible calls/closed or captured fragments. Old handles remain foreign. No old
    /// value is forced; missing/ambiguous code correspondence is a cache miss.
    pub fn reuse_from(&mut self, old: &Session) -> ReuseStats {
        self.reuse_with_limits(old, ReuseLimits::default())
    }
    pub fn reuse_with_limits(&mut self, old: &Session, limits: ReuseLimits) -> ReuseStats {
        self.cache_work(|session| session.reuse_impl(old, limits, false))
    }
    /// Development revision engine: permit body matches across changed global
    /// dependencies only after ordered read validation. Uses the same graph
    /// importer as static transfer; does not establish shared code/heap versions.
    /// The embedding must prove the same checked ABI/semantic context, as
    /// RevisionStore does, before calling this type-erased boundary.
    #[cfg(feature = "incremental")]
    pub fn reuse_validated(&mut self, old: &Session, limits: ReuseLimits) -> ReuseStats {
        self.cache_work(|session| session.reuse_validated_impl(old, limits))
    }
    #[cfg(feature = "incremental")]
    fn reuse_validated_impl(&mut self, old: &Session, mut limits: ReuseLimits) -> ReuseStats {
        if self.stats.steps != 0 || self.abandoned || old.abandoned {
            return ReuseStats::default();
        }
        // Reserve disjoint allowances for preparation/read validation, staged
        // graph work, and ordinary receiving-index construction. Failed optional
        // work consumes the same overall budget as successful imports.
        let total = self.limits.steps;
        let pool = total / 3;
        let bookkeeping = (old.globals.len().saturating_add(old.memo.len()) as u64)
            .saturating_mul(3)
            .saturating_add(1);
        if bookkeeping > pool {
            self.stats.steps = 1.min(total);
            return ReuseStats {
                preparation_steps: self.stats.steps,
                ..Default::default()
            };
        }
        let preparation = (pool - bookkeeping) as usize;
        limits.observed_thunks = limits.observed_thunks.min(preparation / 4);
        limits.code_nodes = limits.code_nodes.min(preparation - limits.observed_thunks);
        limits.heap_entries = limits.heap_entries.min((pool / 2) as usize);
        limits.payload_bytes = limits.payload_bytes.min((pool / 2) as usize);
        self.limits.steps = pool;
        let mut stats = self.reuse_impl(old, limits, true);
        self.limits.steps = total;
        stats.preparation_steps += bookkeeping;
        self.stats.steps += stats.preparation_steps + stats.graph_steps;
        stats
    }
    fn reuse_impl(
        &mut self,
        old: &Session,
        limits: ReuseLimits,
        _validate_reads: bool,
    ) -> ReuseStats {
        let mut stats = ReuseStats::default();
        if self.abandoned
            || old.abandoned
            || (old.validation.changed() && !_validate_reads)
            || self.stats.steps != 0
            || !self.values.is_empty()
            || self.tracking.is_some()
            || old.tracking.is_some()
            || self.limits.primitive_bytes != old.limits.primitive_bytes
            || self.limits.decimal_power != old.limits.decimal_power
            || self.limits.cancellation.load(Ordering::Relaxed)
        {
            return stats;
        }
        let roots = old
            .globals
            .iter()
            .filter(|(_, t)| matches!(old.thunks[t.0].state, ThunkState::Done(Ok(_))))
            .map(|(g, _)| g.clone())
            .take(limits.globals)
            .collect::<Vec<_>>();
        let hosts = self.hosts.compatible_functions(&old.hosts);
        let completed = old
            .memo
            .expressions()
            .filter(|(_, t)| matches!(old.thunks[t.0].state, ThunkState::Done(Ok(_))))
            .collect::<Vec<_>>();
        let mut remaining = limits.code_nodes;
        // Scanning old heap cells must not consume code-correspondence work.
        // Transfer capacity and provenance discovery are independent budgets.
        let mut observation_remaining = limits.observed_thunks;
        let observations = if self.limits.application_cache_entries > 0 {
            captures::observe(old, limits.observed_thunks, &mut observation_remaining)
        } else {
            Vec::new()
        };
        let observation_work = limits.observed_thunks - observation_remaining;
        stats.observed_thunks = observation_work;
        let expressions = completed
            .iter()
            .map(|(e, _)| *e)
            .chain(observations.iter().map(|o| o.expr))
            .collect::<Vec<_>>();
        let Some(mapping) = knot_core::equivalence::correspondence_with_observed_expressions(
            &old.program,
            &self.program,
            &roots,
            &expressions,
            &mut remaining,
            &hosts,
        ) else {
            #[cfg(feature = "incremental")]
            if _validate_reads {
                stats.preparation_steps = (limits.code_nodes - remaining + observation_work) as u64;
            }
            return stats;
        };
        #[allow(unused_mut)]
        let Some(mut code) = graph::Code::new(old, self, mapping, hosts) else {
            #[cfg(feature = "incremental")]
            if _validate_reads {
                stats.preparation_steps = (limits.code_nodes - remaining + observation_work) as u64;
            }
            return stats;
        };
        #[cfg(feature = "incremental")]
        if _validate_reads {
            let names = old
                .globals
                .keys()
                .take(limits.globals)
                .cloned()
                .collect::<Vec<_>>();
            if let Some(versions) = knot_core::equivalence::body_versions(
                &old.program,
                &self.program,
                &names,
                &mut remaining,
            ) {
                code.extend_source(old, self, &versions, remaining / 2);
            }
            remaining -= code.validation_allowance() as usize;
            if old.validation.changed() && !code.has_source_validation() {
                stats.preparation_steps = (limits.code_nodes - remaining + observation_work) as u64;
                return stats;
            }
        }
        let mut maps = graph::Maps::default();
        maps.envs.insert(0, 0);
        for global in old
            .globals
            .keys()
            .filter(|global| code.maps_global(global) && self.globals.contains_key(*global))
        {
            maps.thunks.insert(
                old.globals.get(global).unwrap().0,
                *self.globals.get(global).unwrap(),
            );
        }
        let mut budget = graph::Budget {
            entries: limits.heap_entries,
            bytes: limits.payload_bytes,
        };
        for global in roots.iter().filter(|g| code.mapping.globals.contains(*g)) {
            let ThunkState::Done(Ok(value)) = old.thunks[old.globals[global].0].state else {
                continue;
            };
            if !code.completion_valid(old, self, &value) {
                continue;
            }
            let cost = value.cost;
            let previous_proof = value.proof;
            let Some(import) = graph::transfer(
                old,
                self,
                &code,
                &maps,
                graph::Root::Value(value.value),
                &mut budget,
            ) else {
                continue;
            };
            let graph::Root::Value(value) = import.root else {
                unreachable!()
            };
            stats.heap_entries += import.entries();
            stats.payload_bytes += import.bytes;
            stats.record_graph(&import);
            import.commit_with_proofs(self, &mut maps, old, &code);
            let proof = code.transferred_proof(old, self, previous_proof);
            self.thunks[self.globals[global].0].state = ThunkState::Done(Ok(Completed {
                value,
                cost,
                expression: self.program.globals.get(global).copied(),
                proof,
                stamp: self.validation.stamp(),
                #[cfg(feature = "incremental")]
                introduced: false,
            }));
            stats.globals += 1;
        }
        // Unchanged bodies already use completed-call transfer. Restrict this
        // additional index to fragments whose enclosing definition changed.
        let mut scopes = HashMap::new();
        for o in observations.iter().filter(|o| {
            o.completed && !old.envs[o.env].empty && code.mapping.fragments.contains(&o.expr)
        }) {
            scopes
                .entry(o.env)
                .or_insert_with(|| captures::bindings(old, o.env, &mut remaining));
        }
        let inputs = scopes
            .values()
            .flatten()
            .flat_map(|xs| xs.iter().map(|(_, t)| *t))
            .collect::<std::collections::HashSet<_>>();
        let mut cells = completed;
        let mut known = cells
            .iter()
            .map(|(e, _)| *e)
            .collect::<std::collections::HashSet<_>>();
        for o in &observations {
            if old.envs[o.env].empty
                && inputs.contains(&old.cell_id(o.thunk))
                && !maps.thunks.contains_key(&o.thunk.0)
                && code.mapping.expressions.contains_key(&o.expr)
                && known.insert(o.expr)
            {
                cells.push((o.expr, old.cell_id(o.thunk)));
            }
        }
        // Transfer the actual memoized cells, including their lazy graph, rather
        // than copying expression results into a separate result index.
        for (expr, thunk) in cells {
            if self.memo.len() >= self.limits.application_cache_entries
                || self.heap_entries() >= self.limits.heap_entries
                || budget.entries == 0
            {
                break;
            }
            let Some(next) = code.mapping.expressions.get(&expr).copied() else {
                continue;
            };
            budget.entries -= 1;
            let Some(import) = graph::transfer(
                old,
                self,
                &code,
                &maps,
                graph::Root::Thunk(thunk.owner()),
                &mut budget,
            ) else {
                continue;
            };
            if import.entries() >= self.limits.heap_entries.saturating_sub(self.heap_entries()) {
                continue;
            }
            let graph::Root::Thunk(thunk) = import.root else {
                unreachable!()
            };
            stats.heap_entries += import.entries() + 1;
            stats.payload_bytes += import.bytes;
            stats.record_graph(&import);
            import.commit_with_proofs(self, &mut maps, old, &code);
            self.memo.remember_expression(
                next,
                self.cell_id(thunk),
                self.limits.application_cache_entries,
            );
            stats.expressions += 1;
        }
        // Only already-established inputs are keys. Importing one captured
        // result does not recursively admit its entire old execution history.
        let inputs = maps.thunks.clone();
        for o in observations.iter().filter(|o| {
            o.completed && !old.envs[o.env].empty && code.mapping.fragments.contains(&o.expr)
        }) {
            let Some(expr) = code.mapping.expressions.get(&o.expr).copied() else {
                continue;
            };
            let Some(Some(scope)) = scopes.get(&o.env) else {
                continue;
            };
            let Some(mut key) = scope
                .iter()
                .map(|(l, t)| {
                    Some((
                        *code.mapping.locals.get(l)?,
                        self.cell_id(*inputs.get(&t.0)?),
                    ))
                })
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            key.sort_by_key(|(l, _)| *l);
            if self.memo.captured(expr, &key).is_some() {
                continue;
            }
            let slots = key.len().saturating_add(2);
            if self.memo.len().saturating_add(slots) > self.limits.application_cache_entries {
                break;
            }
            let Some(entries) = budget.entries.checked_sub(slots) else {
                break;
            };
            budget.entries = entries;
            let bytes = key
                .len()
                .saturating_mul(std::mem::size_of::<(LocalId, CellId)>());
            let Some(left) = budget.bytes.checked_sub(bytes) else {
                break;
            };
            budget.bytes = left;
            let Some(import) = graph::transfer(
                old,
                self,
                &code,
                &maps,
                graph::Root::Thunk(o.thunk),
                &mut budget,
            ) else {
                continue;
            };
            if import.entries().saturating_add(slots)
                > self.limits.heap_entries.saturating_sub(self.heap_entries())
            {
                continue;
            }
            let graph::Root::Thunk(thunk) = import.root else {
                unreachable!()
            };
            stats.heap_entries += import.entries() + slots;
            stats.payload_bytes += import.bytes + bytes;
            stats.record_graph(&import);
            import.commit_with_proofs(self, &mut maps, old, &code);
            self.memo.remember_capture(
                expr,
                key,
                self.cell_id(thunk),
                self.limits.application_cache_entries,
            );
            stats.captures += 1;
        }
        // Arguments and execution inputs require established correspondence.
        // The callee's supporting graph may transfer even when a cheap factory
        // result was not retained; its exact input proof travels with the call.
        for (key, result) in old.memo.entries() {
            if !code.call_valid(old, self, &key) {
                continue;
            }
            let cost = old.memo.cost(&key);
            let previous_proof = old.memo.get(&key).expect("retained call").3;
            let (function, argument) = (key.function, key.argument);
            if self.memo.len() >= self.limits.application_cache_entries
                || self.heap_entries() >= self.limits.heap_entries
                || budget.entries == 0
            {
                break;
            }
            #[cfg(feature = "incremental")]
            if !maps.thunks.contains_key(&argument.0) {
                if let Some(identity) = &key.argument_identity {
                    let Some(left) = remaining.checked_sub(identity.width() + 1) else {
                        break;
                    };
                    remaining = left;
                    if code
                        .mapping
                        .expressions
                        .contains_key(&identity.expression())
                        && identity
                            .input_cells()
                            .all(|cell| maps.thunks.contains_key(&cell.0))
                    {
                        if let Some(import) = graph::transfer(
                            old,
                            self,
                            &code,
                            &maps,
                            graph::Root::Thunk(argument.owner()),
                            &mut budget,
                        ) {
                            stats.heap_entries += import.entries();
                            stats.payload_bytes += import.bytes;
                            stats.record_graph(&import);
                            import.commit_with_proofs(self, &mut maps, old, &code);
                        }
                    }
                }
            }
            #[cfg(feature = "incremental")]
            if !maps.values.contains_key(&function) && maps.thunks.contains_key(&argument.0) {
                if let Some(identity) = &key.identity {
                    let Some(left) = remaining.checked_sub(identity.width() + 1) else {
                        break;
                    };
                    remaining = left;
                    if identity
                        .input_cells()
                        .all(|cell| maps.thunks.contains_key(&cell.0))
                    {
                        if let Some(import) = graph::transfer(
                            old,
                            self,
                            &code,
                            &maps,
                            graph::Root::Value(function),
                            &mut budget,
                        ) {
                            stats.heap_entries += import.entries();
                            stats.payload_bytes += import.bytes;
                            stats.record_graph(&import);
                            import.commit_with_proofs(self, &mut maps, old, &code);
                        }
                    }
                }
            }
            let (Some(function), Some(argument)) = (
                maps.values.get(&function).copied(),
                maps.thunks.get(&argument.0).copied(),
            ) else {
                continue;
            };
            let slots = key.slots();
            let bytes = slots
                .saturating_sub(1)
                .saturating_mul(std::mem::size_of::<memo::Call>());
            if budget.entries < slots || budget.bytes < bytes {
                break;
            }
            budget.entries -= slots;
            budget.bytes -= bytes;
            let Some(import) = graph::transfer(
                old,
                self,
                &code,
                &maps,
                graph::Root::Value(result),
                &mut budget,
            ) else {
                continue;
            };
            if import.entries() >= self.limits.heap_entries.saturating_sub(self.heap_entries()) {
                continue;
            }
            let graph::Root::Value(result) = import.root else {
                unreachable!()
            };
            stats.heap_entries += import.entries() + slots;
            stats.payload_bytes += import.bytes + bytes;
            stats.record_graph(&import);
            import.commit_with_proofs(self, &mut maps, old, &code);
            let key = self.call_key(function, self.cell_id(argument));
            let proof = code.transferred_proof(old, self, previous_proof);
            if self.memo.insert_completion(
                key,
                memo::Completion {
                    #[cfg(feature = "incremental")]
                    access: Default::default(),
                    value: result,
                    cost,
                    proof,
                    stamp: self.validation.stamp(),
                    ..Default::default()
                },
                self.limits.application_cache_entries,
            ) {
                stats.applications += 1;
            }
        }
        #[cfg(feature = "incremental")]
        {
            stats.validation_steps = code.validation_work();
            stats.evidence_steps = code.evidence_work();
            self.stats.steps = self
                .stats
                .steps
                .saturating_add(stats.validation_steps + stats.evidence_steps);
            if _validate_reads {
                stats.preparation_steps = (limits.code_nodes - remaining + observation_work) as u64
                    - code.validation_allowance();
                stats.graph_steps = ((limits.heap_entries - budget.entries)
                    + (limits.payload_bytes - budget.bytes))
                    as u64;
            }
        }
        stats
    }
    /// Compatibility entry point; now uses the same graph transfer as reuse_from.
    pub fn reuse_closed_globals_from(&mut self, old: &Session) -> usize {
        self.reuse_from(old).globals
    }
}
