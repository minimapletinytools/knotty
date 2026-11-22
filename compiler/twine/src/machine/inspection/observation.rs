//! Receipts for fully observed pure data, for caches of decoded/native output.
//! No user code is evaluated here. These receipts retain neither runtime values
//! nor dependency certificates and deliberately miss after copying collection.
use super::*;
use std::{
    collections::HashSet,
    hash::{Hash, Hasher},
    sync::Weak,
};

/// Identity of one immutable runtime allocation, not a value equality test or
/// permission to reuse. Use `validate_data_observation` before reusing output.
#[derive(Clone, Debug)]
pub struct DataObservationKey(Weak<Value>);
impl PartialEq for DataObservationKey {
    fn eq(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for DataObservationKey {}
impl Hash for DataObservationKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_ptr().hash(state);
    }
}

/// Limits optional receipt work. `steps` is a remaining allowance shared across
/// calls; actual work also counts toward the session's cumulative execution cap.
#[derive(Clone, Debug)]
pub struct ObservationBudget {
    /// Maximum root plus observed field edges per receipt (sharing can repeat edges).
    pub nodes: usize,
    pub steps: u64,
}

#[derive(Clone)]
pub struct DataObservation {
    key: DataObservationKey,
    reads: Vec<evidence::Witness>,
}
impl DataObservation {
    pub fn key(&self) -> &DataObservationKey {
        &self.key
    }
    /// Conservative owned metadata estimate, excluding allocator bookkeeping.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + std::mem::size_of::<Value>()
            + 2 * std::mem::size_of::<usize>()
            + self.reads.capacity()
                * (std::mem::size_of::<evidence::Witness>() + evidence::Witness::weak_bytes())
    }
}

impl ValueReader<'_> {
    pub fn data_observation_key(&self, value: ValueHandle) -> Result<DataObservationKey, Outcome> {
        if self.session.abandoned {
            return Err(Outcome::Abandoned);
        }
        if value.session != self.session.id {
            return Err(Outcome::ForeignValue);
        }
        Ok(DataObservationKey(
            self.session.values.weak(value.value).expect("valid value"),
        ))
    }

    /// Capture complete pure data only. Lazy, stale, cyclic, opaque, failed,
    /// missing-evidence or over-budget graphs return None. Call after decoding;
    /// this never forces fields on the embedding's behalf.
    pub fn observe_data(
        &mut self,
        value: ValueHandle,
        budget: &mut ObservationBudget,
    ) -> Result<Option<DataObservation>, Outcome> {
        let key = self.data_observation_key(value)?;
        self.observation_work(budget, |session, limits, nodes| {
            if nodes == 0 {
                return Ok(None);
            }
            let mut pending = vec![(value.value, false)];
            let mut active = HashSet::new();
            let mut visited = HashSet::new();
            let mut reads = Vec::new();
            let mut edges = 1;
            while let Some((value, exit)) = pending.pop() {
                charge(session, limits)?;
                if exit {
                    active.remove(&value);
                    continue;
                }
                if active.contains(&value) {
                    return Ok(None);
                }
                if !visited.insert(value) {
                    continue;
                }
                active.insert(value);
                pending.push((value, true));
                let children: Vec<_> = match &session.values[value] {
                    Value::Lit(_) => vec![],
                    Value::Ctor(_, fields) | Value::Tuple(fields) => {
                        if fields.len() > nodes.saturating_sub(edges) {
                            return Ok(None);
                        }
                        fields.clone()
                    }
                    Value::Record(fields) => {
                        if fields.len() > nodes.saturating_sub(edges) {
                            return Ok(None);
                        }
                        fields.values().copied().collect()
                    }
                    _ => return Ok(None),
                };
                if children.len() > nodes.saturating_sub(edges) {
                    return Ok(None);
                }
                edges += children.len();
                for child in children {
                    charge(session, limits)?;
                    let ThunkState::Done(Ok(done)) = *session.cell_state(child) else {
                        return Ok(None);
                    };
                    if !session.validation.current(done.stamp) {
                        return Ok(None);
                    }
                    let Some(witness) = session.evidence.witness(done.proof) else {
                        return Ok(None);
                    };
                    reads.push(witness);
                    pending.push((done.value, false));
                }
            }
            Ok(Some(DataObservation { key, reads }))
        })
    }

    /// Validate in the current session using exact code and versioned host-input
    /// evidence. Outer WHNF identity alone never authorizes deep output reuse.
    pub fn validate_data_observation(
        &mut self,
        value: ValueHandle,
        receipt: &DataObservation,
        budget: &mut ObservationBudget,
    ) -> Result<bool, Outcome> {
        if self.data_observation_key(value)? != receipt.key {
            return Ok(false);
        }
        Ok(self
            .observation_work(budget, |session, limits, nodes| {
                if receipt.reads.len().saturating_add(1) > nodes {
                    return Ok(None);
                }
                charge(session, limits)?;
                for witness in &receipt.reads {
                    charge(session, limits)?;
                    if !session.validate_observation_proof(witness, limits)? {
                        return Ok(None);
                    }
                }
                Ok(Some(()))
            })?
            .is_some())
    }

    fn observation_work<T>(
        &mut self,
        budget: &mut ObservationBudget,
        operation: impl FnOnce(&mut Session, &Limits, usize) -> Result<Option<T>, Outcome>,
    ) -> Result<Option<T>, Outcome> {
        let result = self.session.cache_work(|session| {
            let before = session.stats.steps;
            let mut limits = session.limits.clone();
            limits.steps = limits.steps.min(before.saturating_add(budget.steps));
            let result = operation(session, &limits, budget.nodes);
            budget.steps = budget.steps.saturating_sub(session.stats.steps - before);
            result
        });
        match result {
            Err(Outcome::BudgetExhausted) => Ok(None),
            other => other,
        }
    }
}
fn charge(session: &mut Session, limits: &Limits) -> Result<(), Outcome> {
    if limits.cancellation.load(Ordering::Relaxed) {
        return Err(Outcome::Cancelled);
    }
    if session.stats.steps >= limits.steps {
        return Err(Outcome::BudgetExhausted);
    }
    session.stats.steps += 1;
    Ok(())
}
