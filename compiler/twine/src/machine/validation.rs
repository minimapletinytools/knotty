//! Same-code input worlds. A stamp is authority only within one immutable world;
//! old completions require ordered dependency validation before returning a value.
use super::*;
#[cfg(feature = "incremental")]
mod observation;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stamp {
    #[cfg(feature = "incremental")]
    world: u64,
}
#[derive(Default, Clone)]
pub(super) struct State {
    #[cfg(feature = "incremental")]
    world: u64,
    #[cfg(feature = "incremental")]
    verdicts: Verdicts,
    // Non-expression recipes must survive optional evidence collection.
    #[cfg(feature = "incremental")]
    recoveries: HashMap<CellId, Arc<Pending>>,
}
impl State {
    pub fn stamp(&self) -> Stamp {
        Stamp {
            #[cfg(feature = "incremental")]
            world: self.world,
        }
    }
    pub fn changed(&self) -> bool {
        #[cfg(feature = "incremental")]
        return self.world != 0;
        #[cfg(not(feature = "incremental"))]
        false
    }
    pub fn current(&self, _stamp: Stamp) -> bool {
        #[cfg(feature = "incremental")]
        return self.world == _stamp.world;
        #[cfg(not(feature = "incremental"))]
        true
    }
    #[cfg(feature = "incremental")]
    pub fn clone_work(&self) -> usize {
        self.verdicts
            .entries
            .len()
            .saturating_add(self.recoveries.len())
    }
}

#[cfg(feature = "incremental")]
pub struct InputForkAttempt {
    /// Preparation work, included in the child if sharing succeeded.
    pub work: u64,
    /// Same Program with new immutable host inputs; old handles remain foreign.
    pub session: Option<Session>,
}

#[cfg(feature = "incremental")]
impl Session {
    /// Share an exact Program across versioned host-input changes. This does not
    /// grant inheritance of old request handles or establish source correspondence.
    /// Missing recovery/host promises conservatively decline sharing.
    pub fn try_fork_inputs(
        &self,
        hosts: Arc<HostRegistry>,
        limits: Limits,
    ) -> Result<InputForkAttempt, Outcome> {
        if self.abandoned {
            return Err(Outcome::Abandoned);
        }
        let cancellation = limits.cancellation.clone();
        if cancellation.load(Ordering::Relaxed) {
            return Err(Outcome::Cancelled);
        }
        let mut work = 0;
        let Some(mut next) = self.snapshot(limits, &mut work) else {
            return if cancellation.load(Ordering::Relaxed) {
                Err(Outcome::Cancelled)
            } else {
                Ok(InputForkAttempt {
                    work,
                    session: None,
                })
            };
        };
        // Reserve all preflight work before allocating recovery metadata. Logical
        // entries are bounded by the existing evidence allowance; no forcing.
        let extra = self
            .program
            .expressions
            .len()
            .saturating_add(self.values.len())
            .saturating_add(self.thunks.len())
            .saturating_add(self.memo.len());
        if extra as u64 > next.limits.steps.saturating_sub(work) {
            return Ok(InputForkAttempt {
                work,
                session: None,
            });
        }
        let mut recoveries = HashMap::new();
        let mut recovery_bytes = 0usize;
        let accepted = (|| {
            for expression in &self.program.expressions {
                work += 1;
                if cancellation.load(Ordering::Relaxed) {
                    return None;
                }
                if let Kind::Host(name, signature) = &expression.kind {
                    let function = hosts.function(name)?;
                    if &function.signature != signature || function.identity.is_none() {
                        return None;
                    }
                }
            }
            for value in self.values.iter() {
                work += 1;
                if cancellation.load(Ordering::Relaxed) {
                    return None;
                }
                if let Value::Native(native) = value {
                    if !hosts.accepts_snapshot(native) {
                        return None;
                    }
                }
            }
            for (index, thunk) in self.thunks.iter().enumerate() {
                work += 1;
                if cancellation.load(Ordering::Relaxed) {
                    return None;
                }
                match &thunk.state {
                    ThunkState::Done(Ok(done)) if !done.introduced && done.expression.is_none() => {
                        let cell = CellId(index);
                        let existing = self.validation.recoveries.get(&cell);
                        let recipe = existing
                            .map(Arc::as_ref)
                            .or_else(|| self.evidence.recovery(done.proof, cell))?;
                        // TextList owns shared immutable strings, DecimalParts
                        // may own digits; count payload before retaining a recipe.
                        recovery_bytes = recovery_bytes.saturating_add(recipe.recovery_bytes());
                        if recoveries.len() >= next.limits.dependency_records
                            || recovery_bytes > next.limits.dependency_bytes
                        {
                            return None;
                        }
                        let bytes = recipe.recovery_bytes() as u64;
                        if bytes
                            > next
                                .limits
                                .steps
                                .saturating_sub(work)
                                .saturating_sub(extra as u64)
                        {
                            return None;
                        }
                        work += bytes;
                        recoveries.insert(
                            cell,
                            existing
                                .cloned()
                                .unwrap_or_else(|| Arc::new(recipe.clone())),
                        );
                    }
                    ThunkState::Done(Err(_)) | ThunkState::Evaluating(_) => return None,
                    _ => {}
                }
            }
            Some(())
        })()
        .is_some();
        if cancellation.load(Ordering::Relaxed) {
            return Err(Outcome::Cancelled);
        }
        if !accepted {
            return Ok(InputForkAttempt {
                work,
                session: None,
            });
        }
        next.hosts = hosts;
        next.validation = State {
            world: next.id,
            verdicts: Default::default(),
            recoveries,
        };
        next.stats.steps = work;
        next.stats.maintenance_steps = work;
        Ok(InputForkAttempt {
            work,
            session: Some(next),
        })
    }

    pub(super) fn validate_completion(&mut self, cell: CellId) -> Result<(), Outcome> {
        let ThunkState::Done(Ok(done)) = self.thunks[cell.0].state else {
            return Ok(());
        };
        if self.validation.current(done.stamp) {
            return Ok(());
        }
        self.cache_work(|session| session.validate_stale_completion(cell, done))
    }

    fn validate_stale_completion(
        &mut self,
        cell: CellId,
        mut done: Completed,
    ) -> Result<(), Outcome> {
        let valid = done.introduced || self.valid_in_world(done.stamp, done.proof)?;
        // Journal before both stamping and invalidation. Interrupted inspection
        // restores the old completion, including its original world authority.
        if let Some(checkpoint) = &mut self.inspection {
            if cell.0 < checkpoint.thunks {
                checkpoint
                    .undo
                    .entry(cell.0)
                    .or_insert_with(|| self.thunks[cell.0].state.clone());
            }
        }
        if valid {
            done.stamp = self.validation.stamp();
            self.thunks[cell.0].state = ThunkState::Done(Ok(done));
        } else {
            let recipe = match done.expression {
                Some(expression) => {
                    Pending::Expr(expression, self.thunks[cell.0].activation.0 as usize)
                }
                None => {
                    let recipe = self
                        .validation
                        .recoveries
                        .get(&cell)
                        .expect("input-world preflight retains every non-expression recipe");
                    let bytes = recipe.recovery_bytes() as u64;
                    if bytes > self.limits.steps.saturating_sub(self.stats.steps) {
                        self.abandoned = true;
                        return Err(Outcome::BudgetExhausted);
                    }
                    self.stats.steps += bytes;
                    recipe.as_ref().clone()
                }
            };
            self.thunks[cell.0].state = ThunkState::Pending(recipe);
        }
        Ok(())
    }
}
#[cfg(feature = "incremental")]
impl Pending {
    pub(super) fn recovery_bytes(&self) -> usize {
        let payload = match self {
            Self::TextList {
                source, separator, ..
            } => source
                .len()
                .saturating_add(separator.as_ref().map_or(0, |s| s.len())),
            Self::DecimalParts(decimal, _) => super::decimal_storage_bytes(decimal),
            _ => 0,
        };
        std::mem::size_of::<Self>()
            .saturating_add(64)
            .saturating_add(payload)
    }
}

impl Session {
    pub(super) fn valid_in_world(
        &mut self,
        stamp: Stamp,
        _proof: evidence::Proof,
    ) -> Result<bool, Outcome> {
        if self.validation.current(stamp) {
            return Ok(true);
        }
        #[cfg(feature = "incremental")]
        return self.cache_work(|session| {
            let validation = if let Some(view) = &session.source_code {
                session.evidence.validate_with_code(
                    _proof,
                    &session.hosts,
                    &mut session.validation.verdicts,
                    &session.limits,
                    &mut session.stats.steps,
                    |recipe| {
                        if view.accepts(recipe, &session.values) {
                            evidence::CodeValidity::CheckReads
                        } else {
                            evidence::CodeValidity::Changed
                        }
                    },
                )
            } else {
                session.evidence.validate(
                    _proof,
                    &session.hosts,
                    &mut session.validation.verdicts,
                    &session.limits,
                    &mut session.stats.steps,
                )
            };
            validation.inspect_err(|_| {
                session.abandoned = true;
            })
        });
        #[cfg(not(feature = "incremental"))]
        Ok(true)
    }
}

/// Diagnostic work counters for dependency validation, not elapsed time. Counters
/// include interrupted inspection work; a fork starts new counters.
#[cfg(feature = "incremental")]
#[derive(Debug, Default, Clone, Copy)]
pub struct ValidationStats {
    pub certificates_visited: u64,
    pub cached_decisions: u64,
    pub rejected_paths: u64,
    pub entries: usize,
}
#[cfg(feature = "incremental")]
#[derive(Default, Clone)]
pub(super) struct Verdicts {
    entries: HashMap<storage::Id, bool>,
    pub stats: ValidationStats,
}
#[cfg(feature = "incremental")]
impl Verdicts {
    pub fn lookup(&mut self, id: storage::Id) -> Option<bool> {
        self.stats.certificates_visited += 1;
        let verdict = self.entries.get(&id).copied();
        if verdict.is_some() {
            self.stats.cached_decisions += 1;
        }
        verdict
    }
    pub fn remember(&mut self, id: storage::Id, verdict: bool, limits: &Limits) {
        if self.entries.len() < limits.dependency_records.min(limits.dependency_bytes / 64) {
            self.entries.insert(id, verdict);
        }
    }
}
#[cfg(feature = "incremental")]
impl State {
    pub fn fork(&self) -> Self {
        let mut state = self.clone();
        state.verdicts.stats = Default::default();
        state
    }
}
#[cfg(feature = "incremental")]
impl Session {
    pub fn validation_stats(&self) -> ValidationStats {
        ValidationStats {
            entries: self.validation.verdicts.entries.len(),
            ..self.validation.verdicts.stats
        }
    }
}

#[cfg(feature = "incremental")]
impl Session {
    pub(super) fn recovery_recipe(&self, cell: CellId, proof: evidence::Proof) -> Option<&Pending> {
        self.validation
            .recoveries
            .get(&cell)
            .map(Arc::as_ref)
            .or_else(|| self.evidence.recovery(proof, cell))
    }
}

#[cfg(feature = "incremental")]
impl State {
    pub(super) fn empty_in_same_world(&self) -> Self {
        Self {
            world: self.world,
            ..Default::default()
        }
    }
    /// A detached recipe already preserves reevaluation independently of evidence.
    pub(super) fn owns_recovery(&self, cell: CellId) -> bool {
        self.recoveries.contains_key(&cell)
    }

    pub(super) fn retain_recovery(&mut self, cell: CellId, recipe: Pending) {
        self.recoveries.insert(cell, Arc::new(recipe));
    }
}
