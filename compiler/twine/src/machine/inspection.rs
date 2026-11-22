//! Bounded, transactional forcing. The journal records only existing thunks
//! touched by a query; it never copies the session's entire heap.
use super::*;
mod fields;
pub use fields::{IndexedFields, RecordFields};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThunkHandle {
    pub(super) session: u64,
    pub(super) thunk: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inspection {
    Unevaluated,
    Evaluated(ValueHandle),
    Failed(RuntimeError),
    Truncated,
    Cancelled,
}
#[derive(Debug, Clone)]
pub struct Allocation {
    pub handle: ThunkHandle,
    pub origin: Origin,
    pub activation: ActivationId,
    pub state: Inspection,
}
#[derive(Debug, Clone)]
pub struct BindingView {
    pub name: String,
    pub activation: ActivationId,
    pub handle: ThunkHandle,
    pub state: Inspection,
}
#[derive(Debug, Clone)]
pub struct Child {
    pub field: Field,
    pub handle: ThunkHandle,
    pub state: Inspection,
}
#[derive(Debug, Clone)]
pub struct InspectionBudget {
    pub steps: u64,
    pub heap_entries: usize,
    pub host_bytes: usize,
    pub cancellation: Arc<AtomicBool>,
}
impl Default for InspectionBudget {
    fn default() -> Self {
        Self {
            steps: 100_000,
            heap_entries: 50_000,
            host_bytes: 16 * 1024 * 1024,
            cancellation: Arc::new(AtomicBool::new(false)),
        }
    }
}
pub(super) struct Checkpoint {
    pub thunks: usize,
    values: usize,
    envs: usize,
    stats: Stats,
    pub(super) limits: Limits,
    pub undo: HashMap<usize, ThunkState>,
    tracking: Option<dependencies::TrackingCheckpoint>,
    evidence: evidence::Snapshot,
}
impl Session {
    /// Lexical bindings and dictionary captures visible in one activation.
    pub fn activation_bindings(&self, activation: ActivationId) -> Vec<BindingView> {
        if !self.source_scope_visible(activation.0 as usize) {
            return vec![];
        }
        let Some(mut env) = self.envs.iter().position(|e| e.activation == activation) else {
            return vec![];
        };
        let mut found = std::collections::BTreeSet::new();
        let mut result = Vec::new();
        loop {
            let frame = &self.envs[env];
            for (local, t) in frame.bindings.iter() {
                if found.insert(*local) {
                    if let Some(name) = self.program.local_names.get(local) {
                        result.push(BindingView {
                            name: name.clone(),
                            activation: frame.activation,
                            handle: self.thunk_handle(*t),
                            state: self.thunk_state(*t),
                        });
                    }
                }
            }
            let Some(parent) = frame.parent else {
                break;
            };
            env = parent;
        }
        result.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(a.activation.0.cmp(&b.activation.0))
        });
        result
    }
    pub(super) fn thunk_handle(&self, t: ThunkId) -> ThunkHandle {
        ThunkHandle {
            session: self.id,
            thunk: t.0,
        }
    }
    pub fn global_handle(&self, name: &GlobalName) -> Result<ThunkHandle, Outcome> {
        if self.abandoned {
            return Err(Outcome::Abandoned);
        }
        self.globals
            .get(&Global::Value(name.clone()))
            .map(|t| self.thunk_handle(*t))
            .ok_or_else(|| Outcome::UnknownGlobal(name.clone()))
    }
    /// Current allocation states, optionally restricted to a selected activation.
    /// An empty result means no allocation was observed, not a fabricated value.
    pub fn inspect_node(
        &self,
        node: knot_core::NodeRef,
        activation: Option<ActivationId>,
    ) -> Vec<Allocation> {
        self.thunks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                if !self.source_scope_visible(t.activation.0 as usize) {
                    return None;
                }
                let origin = self.current_source_origin(t.origin)?;
                (origin.node == node && activation.is_none_or(|a| a == t.activation)).then(|| {
                    Allocation {
                        handle: self.thunk_handle(ThunkId(i)),
                        origin,
                        activation: t.activation,
                        state: self.thunk_state(ThunkId(i)),
                    }
                })
            })
            .collect()
    }
    fn thunk_state(&self, t: ThunkId) -> Inspection {
        match self.cell_state(t) {
            ThunkState::Done(Ok(v)) if self.validation.current(v.stamp) => {
                Inspection::Evaluated(ValueHandle {
                    session: self.id,
                    value: v.value,
                })
            }
            ThunkState::Done(Err(e)) => Inspection::Failed(e.as_ref().clone()),
            _ => Inspection::Unevaluated,
        }
    }
    pub fn inspection_state(&self, handle: ThunkHandle) -> Result<Inspection, Outcome> {
        self.validate_thunk(handle)?;
        Ok(self.thunk_state(ThunkId(handle.thunk)))
    }
    pub(super) fn validate_thunk(&self, handle: ThunkHandle) -> Result<(), Outcome> {
        if self.abandoned {
            return Err(Outcome::Abandoned);
        }
        if handle.session != self.id {
            return Err(Outcome::ForeignValue);
        }
        Ok(())
    }
    /// Non-forcing, paginated field handles. IO internals and function closures
    /// are deliberately opaque. Bounds describe enumeration, not evaluation.
    pub fn children(
        &self,
        value: ValueHandle,
        offset: usize,
        count: usize,
    ) -> Result<Vec<Child>, Outcome> {
        let child = |field, t| Child {
            field,
            handle: self.thunk_handle(t),
            state: self.thunk_state(t),
        };
        Ok(match self.read_value(value)? {
            Value::Ctor(_, fields) | Value::DecimalRatio(_, _, fields) | Value::Tuple(fields) => {
                fields
                    .iter()
                    .enumerate()
                    .skip(offset)
                    .take(count)
                    .map(|(i, t)| child(Field::Index(i), *t))
                    .collect()
            }
            Value::Record(fields) => fields
                .iter()
                .skip(offset)
                .take(count)
                .map(|(n, t)| child(Field::Named(n.clone()), *t))
                .collect(),
            _ => vec![],
        })
    }
    /// Force only WHNF, with a separate work budget and bounded additional heap.
    /// On interruption, restore touched thunks and discard new allocations.
    /// No IO runner is reachable from this operation. Completed pure results and
    /// language errors are memoized; interrupted queries can be retried.
    pub fn inspect(
        &mut self,
        handle: ThunkHandle,
        budget: InspectionBudget,
    ) -> Result<Inspection, Outcome> {
        self.validate_thunk(handle)?;
        if self.limits.cancellation.load(Ordering::Relaxed) {
            return Ok(Inspection::Cancelled);
        }
        self.work_clock();
        let old_limits = self.limits.clone();
        self.inspection = Some(Checkpoint {
            thunks: self.thunks.len(),
            values: self.values.len(),
            envs: self.envs.len(),
            stats: self.stats,
            limits: old_limits,
            undo: HashMap::new(),
            tracking: self.tracking.as_ref().map(Tracking::checkpoint),
            evidence: self.evidence.snapshot(),
        });
        self.limits.steps = self.stats.steps.saturating_add(budget.steps);
        self.limits.heap_entries = self
            .limits
            .heap_entries
            .min(self.heap_entries().saturating_add(budget.heap_entries));
        self.limits.host_bytes = self
            .limits
            .host_bytes
            .min(self.stats.host_bytes.saturating_add(budget.host_bytes));
        self.limits.cancellation = budget.cancellation;
        let result = self.machine(State::Force(ThunkId(handle.thunk)));
        let checkpoint = self.inspection.take().expect("inspection checkpoint");
        let interrupted = matches!(result, Err(Outcome::BudgetExhausted | Outcome::Cancelled));
        if interrupted {
            for (id, state) in checkpoint.undo {
                self.thunks[id].state = state;
            }
            if let (Some(tracking), Some(saved)) = (&mut self.tracking, checkpoint.tracking) {
                tracking.rollback(saved);
            }
            self.thunks.truncate(checkpoint.thunks);
            self.values.truncate(checkpoint.values);
            self.envs.truncate(checkpoint.envs);
            #[cfg(feature = "incremental")]
            if let Some(scopes) = &self.source_scopes {
                scopes.truncate(checkpoint.envs);
            }
            self.evidence.rollback(checkpoint.evidence);
            self.abandoned = false;
        } else {
            self.evidence.commit(checkpoint.evidence);
        }
        // Drain rollback COW work before restoring the independent execution
        // budget; it must not leak into the next evaluation or cost interval.
        self.work_clock();
        let work = self.stats.steps.saturating_sub(checkpoint.stats.steps);
        #[cfg(feature = "incremental")]
        let maintenance = self.stats.maintenance_steps - checkpoint.stats.maintenance_steps;
        self.limits = checkpoint.limits;
        if interrupted {
            self.stats = checkpoint.stats;
        }
        self.stats.steps = checkpoint.stats.steps;
        #[cfg(feature = "incremental")]
        {
            self.stats.maintenance_steps = checkpoint.stats.maintenance_steps;
            self.stats.inspection_maintenance_steps = checkpoint
                .stats
                .inspection_maintenance_steps
                .saturating_add(maintenance);
        }
        self.stats.inspection_steps = checkpoint.stats.inspection_steps.saturating_add(work);
        match result {
            Ok(value) => Ok(Inspection::Evaluated(ValueHandle {
                session: self.id,
                value,
            })),
            Err(Outcome::Runtime(error)) => Ok(Inspection::Failed(error)),
            Err(Outcome::BudgetExhausted) => Ok(Inspection::Truncated),
            Err(Outcome::Cancelled) => Ok(Inspection::Cancelled),
            Err(other) => Err(other),
        }
    }
}

/// Evaluation/observation access without authority to replace the session,
/// install code, apply unchecked arguments or run IO. Forcing consumes the same
/// cumulative execution budget; this is not a separate inspection transaction.
pub struct ValueReader<'a> {
    session: &'a mut Session,
}
#[cfg(feature = "incremental")]
mod observation;
#[cfg(feature = "incremental")]
pub use observation::{DataObservation, DataObservationKey, ObservationBudget};
impl Session {
    pub fn read_values(&mut self) -> ValueReader<'_> {
        ValueReader { session: self }
    }
}
impl ValueReader<'_> {
    /// Cumulative runtime work, including demands made through this reader.
    pub fn stats(&self) -> Stats {
        self.session.stats()
    }
    pub fn force(&mut self, value: ThunkHandle) -> Result<ValueHandle, Outcome> {
        self.session.force(value)
    }
    pub fn view(&self, value: ValueHandle) -> Result<ValueView<'_>, Outcome> {
        self.session.view(value)
    }
    pub fn children(
        &self,
        value: ValueHandle,
        offset: usize,
        count: usize,
    ) -> Result<Vec<Child>, Outcome> {
        self.session.children(value, offset, count)
    }
}
