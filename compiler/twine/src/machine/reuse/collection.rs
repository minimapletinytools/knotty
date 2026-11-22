//! Compact a named root graph into a new exact-code/world Session. No exposed
//! arena slot is recycled; inherited handles use explicit old-to-new maps.
use super::*;
use std::cell::{Cell, RefCell};

#[derive(Debug, Clone, Copy)]
pub enum RetainedRoot {
    Thunk(ThunkHandle),
    Value(ValueHandle),
}
#[derive(Debug, Clone, Copy)]
pub struct CollectionBudget {
    /// Additional staged runtime entries, independent of mandatory code globals.
    pub entries: usize,
    /// Staged/retained graph, inheritance maps, payload and recovery metadata.
    /// Code/global-table setup and mandatory-root proof storage have separate
    /// bounds. Optional retention also bills its proof and identity growth here.
    pub bytes: usize,
    /// Optional cache roots, selected after mandatory live roots have survived.
    pub retention: Option<CacheRetention>,
}
impl Default for CollectionBudget {
    fn default() -> Self {
        Self {
            entries: 100_000,
            bytes: 16 * 1024 * 1024,
            retention: None,
        }
    }
}

pub(super) struct History {
    source: u64,
    destination: u64,
    limits: Limits,
    work: Cell<u64>,
    work_limit: Cell<u64>,
    imported: RefCell<HashMap<storage::Id, evidence::Proof>>,
}
impl History {
    pub fn new(old: &Session, new: &Session, allowance: u64) -> Self {
        let mut limits = new.limits.clone();
        limits.steps = allowance;
        Self {
            source: old.id,
            destination: new.id,
            limits,
            work: Cell::new(0),
            work_limit: Cell::new(allowance),
            imported: RefCell::new(HashMap::new()),
        }
    }
    pub fn proof_storage(
        &self,
        old: &Session,
        new: &Session,
        roots: impl IntoIterator<Item = evidence::Proof>,
        remaining: &mut u64,
        scratch: &mut evidence::StorageTraversal,
    ) -> (usize, bool) {
        assert_eq!((old.id, new.id), (self.source, self.destination));
        let estimate = old.evidence.estimate_storage(
            &new.evidence,
            roots,
            &self.imported.borrow(),
            (128, remaining, &new.limits.cancellation),
            scratch,
        );
        (estimate.bytes, estimate.complete)
    }
    pub fn set_work_limit(&self, limit: u64) -> u64 {
        self.work_limit.replace(limit.min(self.limits.steps))
    }
    pub fn work(&self) -> u64 {
        self.work.get()
    }
    pub fn import(
        &self,
        old: &Session,
        new: &mut Session,
        proof: evidence::Proof,
    ) -> evidence::Proof {
        assert_eq!((old.id, new.id), (self.source, self.destination));
        let mut work = self.work.get();
        let mut limits = self.limits.clone();
        limits.steps = limits.steps.min(self.work_limit.get());
        limits.dependency_bytes = limits.dependency_bytes.min(new.limits.dependency_bytes);
        let proof = new.evidence.import_proof(
            &old.evidence,
            proof,
            &mut self.imported.borrow_mut(),
            (&limits, &mut work),
            |recipe| match recipe {
                evidence::Recipe::Thunk(_, Pending::Expr(expression, _))
                | evidence::Recipe::Historical(Some(expression)) => {
                    Some(evidence::SummaryCode::Expression(*expression))
                }
                evidence::Recipe::Summary(code) => Some(*code),
                evidence::Recipe::Call(function, _) => match old.values.get(*function)? {
                    Value::Closure(_, body, _) => Some(evidence::SummaryCode::Expression(*body)),
                    _ => None,
                },
                evidence::Recipe::Thunk(..)
                | evidence::Recipe::Introduced(_)
                | evidence::Recipe::Host(..)
                | evidence::Recipe::Historical(None) => Some(evidence::SummaryCode::Immutable),
            },
        );
        self.work.set(work);
        proof
    }
}

impl Session {
    /// Collect at a new revision boundary. Only named roots and their strong
    /// graph edges survive, with optional cost-selected roots when requested. Old
    /// handles remain valid in the old Session. The returned inheritance proves
    /// only retained handles, including reachable children, never later allocations.
    /// A miss reports preparation work and publishes no partial Session.
    pub fn collect_snapshot(
        &self,
        roots: &[RetainedRoot],
        limits: Limits,
        budget: CollectionBudget,
    ) -> Result<ForkAttempt, Outcome> {
        if self.abandoned {
            return Err(Outcome::Abandoned);
        }
        if limits.cancellation.load(Ordering::Relaxed) {
            return Err(Outcome::Cancelled);
        }
        let mut work = 0;
        let mut attempt = || -> Option<SessionFork> {
            if limits.steps == 0 {
                return None;
            }
            work = 1;
            if self.inspection.is_some()
                || self.tracking.is_some()
                || limits.primitive_bytes != self.limits.primitive_bytes
                || limits.decimal_power != self.limits.decimal_power
                || limits.heap_entries <= self.globals.len()
            {
                return None;
            }
            // Bound all preparation, root enumeration and receiving global/ref
            // construction before creating the candidate. No old heap-wide scan.
            let preparation = self
                .program
                .expressions
                .len()
                .checked_mul(2)?
                .checked_add(self.globals.len().checked_mul(3)?)?
                .checked_add(roots.len())?
                .checked_add(1)? as u64;
            if preparation > limits.steps {
                return None;
            }
            for root in roots {
                work += 1;
                if limits.cancellation.load(Ordering::Relaxed) {
                    return None;
                }
                match root {
                    RetainedRoot::Thunk(handle) => self.validate_thunk(*handle).ok()?,
                    RetainedRoot::Value(handle) => {
                        self.view(*handle).ok()?;
                    }
                }
            }
            for expression in &self.program.expressions {
                work += 1;
                if limits.cancellation.load(Ordering::Relaxed) {
                    return None;
                }
                if let Kind::Host(name, _) = &expression.kind {
                    self.hosts.function(name)?.identity.as_ref()?;
                }
            }
            work = preparation;
            let mut next = Self::empty(self.program.clone(), limits.clone(), self.hosts.clone());
            // Reserve receiving globals before staged graph allocations.
            next.limits.heap_entries -= self.globals.len();
            next.validation = self.validation.empty_in_same_world();
            next.source_code = self.source_code.clone();
            let mut maps = graph::Maps::default();
            maps.envs.insert(0, 0);
            // Separate graph and evidence allowances. Evidence exhaustion only
            // loses future reuse; root-graph exhaustion cannot drop live values.
            let available = limits.steps - preparation;
            let entries = budget.entries.min((available / 4) as usize);
            let available = available - entries as u64;
            let bytes = budget.bytes.min((available / 2) as usize);
            let available = available - bytes as u64;
            let allowance = available / 2;
            let index_allowance = available - allowance;
            let mut remaining = graph::Budget { entries, bytes };
            let initial = (remaining.entries, remaining.bytes);
            let code = graph::Code::collection(self, &next, allowance)?;
            let mut complete = true;
            for root in roots {
                let root = match root {
                    RetainedRoot::Thunk(handle) => graph::Root::Thunk(ThunkId(handle.thunk)),
                    RetainedRoot::Value(handle) => graph::Root::Value(handle.value),
                };
                let copied = graph::transfer(self, &next, &code, &maps, root, &mut remaining);
                let Some(copied) = copied else {
                    complete = false;
                    break;
                };
                copied.commit_with_proofs(&mut next, &mut maps, self, &code);
            }
            if complete {
                if let Some(policy) = budget.retention {
                    next.limits.steps = index_allowance;
                    retention::retain(self, &mut next, &code, &mut maps, &mut remaining, policy);
                    next.limits.steps = limits.steps;
                }
            }
            work += next.stats.steps;
            work += (initial.0 - remaining.entries + initial.1 - remaining.bytes) as u64
                + code.collection.as_ref()?.work();
            if !complete || limits.cancellation.load(Ordering::Relaxed) {
                return None;
            }
            next.limits.heap_entries = limits.heap_entries;
            for (name, previous) in &self.globals {
                let thunk = match maps.thunks.get(&previous.0) {
                    Some(thunk) => *thunk,
                    None => {
                        let expression = *self.program.globals.get(name)?;
                        next.delay_expression(expression, 0)
                    }
                };
                next.globals.insert(name.clone(), thunk);
            }
            next.global_refs
                .extend(
                    next.program
                        .expressions
                        .iter()
                        .map(|expression| match &expression.kind {
                            Kind::Global(name) => next.globals.get(name).copied(),
                            _ => None,
                        }),
                );
            if next.heap_entries() > next.limits.heap_entries {
                return None;
            }
            if self.source_scopes.is_some() {
                let mut scopes = vec![true; next.envs.len()];
                for (old, new) in &maps.envs {
                    let (visible, cost) = self.source_scope_visibility(*old);
                    if cost > next.limits.steps.saturating_sub(work) {
                        return None;
                    }
                    work += cost;
                    scopes[*new] = visible;
                }
                next.source_scopes = Some(scopes.into());
            }
            next.stats.steps = work;
            next.stats.maintenance_steps = work;
            let inheritance = SnapshotInheritance::collected(
                self.id,
                next.id,
                maps.thunks.into_iter().map(|(a, b)| (a, b.0)).collect(),
                maps.values.into_iter().collect(),
            );
            Some(SessionFork {
                session: next,
                inheritance,
            })
        };
        let fork = attempt();
        if limits.cancellation.load(Ordering::Relaxed) {
            return Err(Outcome::Cancelled);
        }
        Ok(ForkAttempt { work, fork })
    }
}
