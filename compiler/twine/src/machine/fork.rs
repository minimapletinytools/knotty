//! Explicit inheritance for an immutable runtime snapshot. This certifies only
//! the captured heap prefix, not sibling/later allocations or source publication.
use super::*;

#[derive(Debug, Clone)]
pub struct SnapshotInheritance {
    source: u64,
    destination: u64,
    thunks: Handles,
    values: Handles,
}
#[derive(Debug, Clone)]
enum Handles {
    Prefix(usize),
    Collected(Arc<HashMap<usize, usize>>),
}
impl Handles {
    fn translate(&self, id: usize) -> Option<usize> {
        match self {
            Self::Prefix(len) => (id < *len).then_some(id),
            Self::Collected(map) => map.get(&id).copied(),
        }
    }
}
impl SnapshotInheritance {
    pub(super) fn collected(
        source: u64,
        destination: u64,
        thunks: HashMap<usize, usize>,
        values: HashMap<usize, usize>,
    ) -> Self {
        Self {
            source,
            destination,
            thunks: Handles::Collected(Arc::new(thunks)),
            values: Handles::Collected(Arc::new(values)),
        }
    }
}
pub struct SessionFork {
    pub session: Session,
    pub inheritance: SnapshotInheritance,
}
pub struct ForkAttempt {
    /// Actual preparation work, including rejected snapshot scans. A successful
    /// child's Stats already includes this work; do not add it to that child twice.
    pub work: u64,
    pub fork: Option<SessionFork>,
}
impl Session {
    /// Attempt an exact-program/world fork without forcing values or running IO.
    /// None is a conservative sharing miss. Failed preparation still reports its
    /// work so an outer transaction can charge it before a fallback strategy.
    pub fn try_fork(&self, limits: Limits) -> Result<ForkAttempt, Outcome> {
        if self.abandoned {
            return Err(Outcome::Abandoned);
        }
        if limits.cancellation.load(Ordering::Relaxed) {
            return Err(Outcome::Cancelled);
        }
        let cancellation = limits.cancellation.clone();
        let mut work = 0;
        let session = self.snapshot(limits, &mut work);
        if cancellation.load(Ordering::Relaxed) {
            return Err(Outcome::Cancelled);
        }
        Ok(ForkAttempt {
            work,
            fork: session.map(|session| {
                let inheritance = SnapshotInheritance {
                    source: self.id,
                    destination: session.id,
                    thunks: Handles::Prefix(self.thunks.len()),
                    values: Handles::Prefix(self.values.len()),
                };
                SessionFork {
                    session,
                    inheritance,
                }
            }),
        })
    }
    pub fn inherit_thunk(
        &self,
        proof: &SnapshotInheritance,
        handle: ThunkHandle,
    ) -> Result<ThunkHandle, Outcome> {
        if proof.destination != self.id || handle.session != proof.source {
            return Err(Outcome::ForeignValue);
        }
        let inherited = ThunkHandle {
            session: self.id,
            thunk: proof
                .thunks
                .translate(handle.thunk)
                .ok_or(Outcome::ForeignValue)?,
        };
        self.validate_thunk(inherited)?;
        Ok(inherited)
    }
    pub fn inherit_value(
        &self,
        proof: &SnapshotInheritance,
        handle: ValueHandle,
    ) -> Result<ValueHandle, Outcome> {
        if proof.destination != self.id || handle.session != proof.source {
            return Err(Outcome::ForeignValue);
        }
        let inherited = ValueHandle {
            session: self.id,
            value: proof
                .values
                .translate(handle.value)
                .ok_or(Outcome::ForeignValue)?,
        };
        self.view(inherited)?;
        Ok(inherited)
    }
}
