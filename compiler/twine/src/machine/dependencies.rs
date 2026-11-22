//! Optional demand provenance. These IDs distinguish allocations/calls within a
//! session; they are NOT cross-revision value equality or cache keys.
use super::*;
use crate::{HostVersion, SnapshotId};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AllocationId {
    pub session: u64,
    pub serial: u64,
}
#[derive(Debug, Clone, Copy)]
pub struct TrackingLimits {
    /// Bounds records, dependency edges, visited expressions, and call arguments.
    /// Overflow marks the graph incomplete and stops recording; execution proceeds.
    pub entries: usize,
}
impl Default for TrackingLimits {
    fn default() -> Self {
        Self { entries: 1_000_000 }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DemandStatus {
    Running,
    Completed,
    Failed,
    Interrupted,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallObservation {
    pub function: ValueHandle,
    pub arguments: Vec<AllocationId>,
    /// Together with the session this identifies the closure's exact environment.
    /// Actual captured values must still be validated before any future reuse.
    pub capture: Option<ActivationId>,
}
#[derive(Debug, Clone)]
pub struct DemandRecord {
    pub allocation: AllocationId,
    pub origin: Origin,
    pub activation: ActivationId,
    pub status: DemandStatus,
    pub forced: BTreeSet<AllocationId>,
    pub code: BTreeMap<ExprId, knot_core::NodeRef>,
    pub globals: BTreeSet<Global>,
    pub calls: Vec<CallObservation>,
    pub hosts: BTreeMap<GlobalName, Option<HostVersion>>,
    pub native_snapshots: BTreeSet<(GlobalName, SnapshotId)>,
    /// Includes unversioned native values and values introduced outside a
    /// recorded evaluation (e.g. IO input). Never silently treat these as pure.
    pub opaque_input: bool,
    pub executed_io: bool,
}
#[derive(Debug, Default, Clone)]
pub struct DependencyGraph {
    pub records: BTreeMap<AllocationId, DemandRecord>,
    pub truncated: bool,
}
#[derive(Debug, Default)]
pub struct DemandFootprint {
    pub code: BTreeMap<ExprId, knot_core::NodeRef>,
    pub globals: BTreeSet<Global>,
    pub hosts: BTreeMap<GlobalName, Option<HostVersion>>,
    pub native_snapshots: BTreeSet<(GlobalName, SnapshotId)>,
    pub incomplete: bool,
    pub opaque_input: bool,
    pub executed_io: bool,
}
impl DependencyGraph {
    /// Follow actual force edges, including memo hits and branch conditions.
    /// Calls retain unforced argument identities without making them demands.
    pub fn footprint(&self, root: AllocationId) -> DemandFootprint {
        let mut out = DemandFootprint {
            incomplete: self.truncated,
            ..Default::default()
        };
        let mut seen = BTreeSet::new();
        let mut todo = vec![root];
        while let Some(id) = todo.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(r) = self.records.get(&id) else {
                out.incomplete = true;
                continue;
            };
            out.incomplete |= matches!(r.status, DemandStatus::Running | DemandStatus::Interrupted);
            out.opaque_input |= r.opaque_input;
            out.executed_io |= r.executed_io;
            out.code.extend(r.code.iter().map(|(e, n)| (*e, *n)));
            out.globals.extend(r.globals.iter().cloned());
            out.hosts.extend(r.hosts.clone());
            out.native_snapshots
                .extend(r.native_snapshots.iter().cloned());
            todo.extend(&r.forced);
        }
        out
    }
}
pub(super) struct Tracking {
    pub graph: DependencyGraph,
    ids: Vec<AllocationId>,
    next: u64,
    session: u64,
    active: Vec<AllocationId>,
    introduced: Vec<AllocationId>,
    used: usize,
    limit: usize,
    pub in_io: bool,
}
#[derive(Clone, Copy)]
pub(super) struct TrackingCheckpoint {
    ids: usize,
    introduced: usize,
    used: usize,
    truncated: bool,
}
impl Tracking {
    fn new(session: u64, count: usize, limits: TrackingLimits) -> Self {
        let mut t = Self {
            graph: DependencyGraph::default(),
            ids: Vec::new(),
            next: 0,
            session,
            active: vec![],
            introduced: vec![],
            used: 0,
            limit: limits.entries,
            in_io: false,
        };
        for _ in 0..count {
            t.allocate();
        }
        t
    }
    pub fn allocate(&mut self) {
        // IDs never rewind on inspector rollback, even when heap slots do.
        self.ids.push(AllocationId {
            session: self.session,
            serial: self.next,
        });
        self.next = self
            .next
            .checked_add(1)
            .expect("allocation identity overflow");
    }
    fn charge(&mut self, n: usize) -> bool {
        if self.graph.truncated {
            return false;
        }
        if n > self.limit.saturating_sub(self.used) {
            self.graph.truncated = true;
            return false;
        }
        self.used += n;
        true
    }
    pub fn checkpoint(&self) -> TrackingCheckpoint {
        TrackingCheckpoint {
            ids: self.ids.len(),
            introduced: self.introduced.len(),
            used: self.used,
            truncated: self.graph.truncated,
        }
    }
    pub fn rollback(&mut self, c: TrackingCheckpoint) {
        for id in self.introduced.drain(c.introduced..) {
            self.graph.records.remove(&id);
        }
        self.ids.truncate(c.ids);
        self.used = c.used;
        self.graph.truncated = c.truncated;
        self.active.clear();
    }
    pub fn force(&mut self, thunk: ThunkId, value: &Thunk, origin: Origin) {
        let id = self.ids[thunk.0];
        if !self.graph.records.contains_key(&id) && self.charge(1) {
            self.introduced.push(id);
            self.graph.records.insert(
                id,
                DemandRecord {
                    allocation: id,
                    origin,
                    activation: value.activation,
                    status: match value.state {
                        ThunkState::Done(Ok(_)) => DemandStatus::Completed,
                        ThunkState::Done(Err(_)) => DemandStatus::Failed,
                        _ => DemandStatus::Running,
                    },
                    forced: BTreeSet::new(),
                    code: BTreeMap::new(),
                    globals: BTreeSet::new(),
                    calls: vec![],
                    hosts: BTreeMap::new(),
                    native_snapshots: BTreeSet::new(),
                    opaque_input: matches!(value.state, ThunkState::Done(_)),
                    executed_io: self.in_io,
                },
            );
        }
        if let Some(parent) = self.active.last().copied() {
            if self
                .graph
                .records
                .get(&parent)
                .is_some_and(|r| !r.forced.contains(&id))
                && self.charge(1)
            {
                self.graph
                    .records
                    .get_mut(&parent)
                    .unwrap()
                    .forced
                    .insert(id);
            }
        }
        if matches!(value.state, ThunkState::Pending(_)) {
            self.active.push(id);
        }
    }
    pub fn code(&mut self, expr: ExprId, node: knot_core::NodeRef, global: Option<&Global>) {
        let Some(id) = self.active.last().copied() else {
            return;
        };
        if self
            .graph
            .records
            .get(&id)
            .is_some_and(|r| !r.code.contains_key(&expr))
            && self.charge(1)
        {
            let record = self.graph.records.get_mut(&id).unwrap();
            record.code.insert(expr, node);
            if let Some(global) = global {
                record.globals.insert(global.clone());
            }
        }
    }
    pub fn call(&mut self, function: ValueId, args: &[ThunkId], capture: Option<ActivationId>) {
        let Some(id) = self.active.last().copied() else {
            return;
        };
        if self.charge(1 + args.len()) {
            let arguments = args.iter().map(|t| self.ids[t.0]).collect();
            if let Some(record) = self.graph.records.get_mut(&id) {
                record.calls.push(CallObservation {
                    function: ValueHandle {
                        session: self.session,
                        value: function,
                    },
                    arguments,
                    capture,
                });
            }
        }
    }
    pub fn host(&mut self, name: &GlobalName, version: Option<&HostVersion>) {
        let Some(id) = self.active.last().copied() else {
            return;
        };
        if self.charge(1) {
            if let Some(record) = self.graph.records.get_mut(&id) {
                record.hosts.insert(name.clone(), version.cloned());
            }
        }
    }
    pub fn native(&mut self, value: &NativeValue) {
        let Some(id) = self.active.last().copied() else {
            return;
        };
        if self.charge(1) {
            if let Some(record) = self.graph.records.get_mut(&id) {
                match value.snapshot_id() {
                    Some(snapshot) => {
                        record
                            .native_snapshots
                            .insert((value.type_name().clone(), snapshot.clone()));
                    }
                    None => record.opaque_input = true,
                }
            }
        }
    }
    pub fn executed_io(&mut self, thunk: ThunkId) {
        if let Some(record) = self.graph.records.get_mut(&self.ids[thunk.0]) {
            record.executed_io = true;
        }
    }
    pub fn complete(&mut self, thunk: ThunkId) {
        let id = self.ids[thunk.0];
        debug_assert_eq!(self.active.pop(), Some(id));
        if let Some(r) = self.graph.records.get_mut(&id) {
            r.status = DemandStatus::Completed;
        }
    }
    pub fn stop(&mut self, status: DemandStatus) {
        for id in self.active.drain(..) {
            if let Some(r) = self.graph.records.get_mut(&id) {
                r.status = status;
            }
        }
    }
}
impl Session {
    /// Enable before any evaluation. Completed pre-existing values cannot acquire
    /// missing provenance retroactively. Default sessions do not record a graph.
    pub fn enable_dependency_tracking(
        &mut self,
        limits: TrackingLimits,
    ) -> Result<(), &'static str> {
        if self.stats.steps != 0 || !self.values.is_empty() || self.tracking.is_some() {
            return Err("dependency tracking must be enabled once, before evaluation");
        }
        self.tracking = Some(Tracking::new(self.id, self.thunks.len(), limits));
        Ok(())
    }
    pub fn dependency_graph(&self) -> Option<&DependencyGraph> {
        self.tracking.as_ref().map(|t| &t.graph)
    }
    pub fn allocation_id(&self, handle: ThunkHandle) -> Result<Option<AllocationId>, Outcome> {
        self.validate_thunk(handle)?;
        Ok(self.tracking.as_ref().map(|t| t.ids[handle.thunk]))
    }
}
