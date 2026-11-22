//! Serialized revision transactions around the actual evaluator. Each Session is
//! an isolated completion overlay over shared heap pages. Same-code input
//! changes validate dynamic reads; source changes can opt into shared executable
//! versions with a bounded checked-transfer rebase.
//! Leases pin revisions, never their ancestors.
use super::*;
use std::sync::Weak;
mod prepare;
pub use prepare::RevisionPreparation;

pub type RevisionCollection = storage::Collection;
pub type RevisionCollectionStatus = storage::CollectionStatus;

/// An explicit root. Cloning keeps this revision's runtime handles alive. Cancel
/// invalidates all leases to that candidate; collecting an unpinned old revision
/// cannot affect another revision. Naked Session handles do not pin a revision.
#[derive(Debug, Clone)]
pub struct Revision {
    owner: u64,
    id: storage::Id,
    lease: Arc<()>,
}
impl PartialEq for Revision {
    fn eq(&self, other: &Self) -> bool {
        self.owner == other.owner && self.id == other.id
    }
}
impl Eq for Revision {}

#[derive(Debug, Clone, Copy)]
pub struct RevisionLimits {
    pub live_revisions: usize,
    pub code_expressions_per_revision: usize,
    pub context_bytes_per_revision: usize,
    pub heap_entries_per_revision: usize,
    pub dependency_bytes_per_revision: usize,
}
impl Default for RevisionLimits {
    fn default() -> Self {
        Self {
            live_revisions: 8,
            code_expressions_per_revision: 2_000_000,
            context_bytes_per_revision: 1024 * 1024,
            heap_entries_per_revision: 1_000_000,
            dependency_bytes_per_revision: 16 * 1024 * 1024,
        }
    }
}

/// Exact checked ABI/semantic context bytes supplied by the typed embedding.
/// They must cover nominal declarations, binding schemes, dictionary layouts,
/// bundled-library ABI and execution semantics. A digest alone is insufficient.
/// None disables transfer; an empty context is a valid explicit caller promise.
pub type RevisionContext = Option<Arc<[u8]>>;

/// Source sharing is opt-in while typed operation linkage is being integrated.
/// A bounded sharing miss rebases through checked transfer into the supplied
/// current Program, under the remaining execution allowance.
#[derive(Debug, Clone, Default)]
pub struct RevisionReuse {
    pub source: Option<SourceForkLimits>,
    pub transfer: ReuseLimits,
}
impl From<ReuseLimits> for RevisionReuse {
    fn from(transfer: ReuseLimits) -> Self {
        Self {
            source: None,
            transfer,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionError {
    ForeignOrExpired,
    Capacity,
    StaleBase,
    NotCandidate,
    Interrupted,
    Host(HostLinkError),
    Evaluation(Outcome),
    Extension(ExtensionError),
    NoInheritance,
}

/// A bounded attempt to stage a new revision. A miss publishes nothing; its
/// work must still be charged by an embedding before trying another strategy.
pub struct RevisionForkAttempt {
    pub work: u64,
    pub revision: Option<Revision>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Candidate(storage::Id),
    Published,
}
struct Entry {
    session: Session,
    context: RevisionContext,
    lease: Weak<()>,
    status: Status,
    inheritance: Option<(storage::Id, SnapshotInheritance)>,
}
impl storage::Trace for Entry {
    fn edge_count(&self) -> usize {
        0
    }
    fn edge(&self, _: usize) -> storage::Id {
        unreachable!("ancestor IDs are weak")
    }
    fn sweep_work(&self) -> u64 {
        // Reserve retirement work before dropping a whole overlay. Shared code
        // may survive elsewhere; conservatively charge its traversal regardless.
        1u64.saturating_add(self.session.heap_entries() as u64)
            .saturating_add(self.session.program.expressions.len() as u64)
            .saturating_add(self.context.as_ref().map_or(0, |c| c.len()) as u64)
            .saturating_add(self.session.evidence_stats().reads as u64)
            .saturating_add(self.session.evidence_stats().certificates as u64)
            .saturating_add(self.session.validation.clone_work() as u64)
            .saturating_add(self.session.identity_support.clone_work() as u64)
            .saturating_add(self.session.identity_support.bytes() as u64)
    }
}

/// Development revision owner. Evaluation is serialized through &mut self.
/// Exact Programs share heap pages with input validation; `RevisionReuse` also
/// enables source sharing and bounded rebases. Collection creates another candidate rather
/// than invalidating handles held by an accepted or explicitly pinned revision.
pub struct RevisionStore {
    owner: u64,
    entries: storage::Arena<Entry>,
    accepted: Revision,
    limits: RevisionLimits,
}
impl RevisionStore {
    pub fn new(
        program: Arc<Program>,
        context: RevisionContext,
        execution: Limits,
        hosts: Arc<HostRegistry>,
        limits: RevisionLimits,
    ) -> Result<Self, RevisionError> {
        Self::check_program(&program, &context, &limits)?;
        if limits.live_revisions == 0 {
            return Err(RevisionError::Capacity);
        }
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let owner = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| RevisionError::Capacity)?;
        let session =
            Session::with_hosts(program, Self::execution_limits(execution, &limits), hosts)
                .map_err(RevisionError::Host)?;
        if session.heap_entries() > session.limits.heap_entries {
            return Err(RevisionError::Capacity);
        }
        let lease = Arc::new(());
        let mut entries = storage::Arena::default();
        let id = entries
            .insert(
                Entry {
                    session,
                    context,
                    lease: Arc::downgrade(&lease),
                    status: Status::Published,
                    inheritance: None,
                },
                limits.live_revisions,
            )
            .map_err(|_| RevisionError::Capacity)?;
        Ok(Self {
            owner,
            entries,
            accepted: Revision { owner, id, lease },
            limits,
        })
    }
    fn check_program(
        program: &Program,
        context: &RevisionContext,
        limits: &RevisionLimits,
    ) -> Result<(), RevisionError> {
        if program.expressions.len() > limits.code_expressions_per_revision
            || context
                .as_ref()
                .is_some_and(|c| c.len() > limits.context_bytes_per_revision)
        {
            return Err(RevisionError::Capacity);
        }
        Ok(())
    }
    fn execution_limits(mut execution: Limits, limits: &RevisionLimits) -> Limits {
        execution.heap_entries = execution.heap_entries.min(limits.heap_entries_per_revision);
        execution.dependency_bytes = execution
            .dependency_bytes
            .min(limits.dependency_bytes_per_revision);
        execution
    }
    fn entry(&self, revision: &Revision) -> Result<&Entry, RevisionError> {
        if revision.owner != self.owner {
            return Err(RevisionError::ForeignOrExpired);
        }
        let entry = self
            .entries
            .get(revision.id)
            .ok_or(RevisionError::ForeignOrExpired)?;
        if !entry.lease.ptr_eq(&Arc::downgrade(&revision.lease)) {
            return Err(RevisionError::ForeignOrExpired);
        }
        Ok(entry)
    }
    pub fn accepted(&self) -> Revision {
        self.accepted.clone()
    }
    pub fn live_revisions(&self) -> usize {
        self.entries.len()
    }
    pub fn session(&self, revision: &Revision) -> Result<&Session, RevisionError> {
        Ok(&self.entry(revision)?.session)
    }
    /// Includes immutable historical versions retained by source sharing.
    pub fn code_expressions(&self, revision: &Revision) -> Result<usize, RevisionError> {
        Ok(self.entry(revision)?.session.program.expressions.len())
    }
    fn session_mut(&mut self, revision: &Revision) -> Result<&mut Session, RevisionError> {
        self.entry(revision)?;
        Ok(&mut self.entries.get_mut(revision.id).unwrap().session)
    }
    pub fn evaluate(
        &mut self,
        revision: &Revision,
        name: &GlobalName,
    ) -> Result<ValueHandle, RevisionError> {
        self.session_mut(revision)?
            .evaluate(name)
            .map_err(RevisionError::Evaluation)
    }
    pub fn force(
        &mut self,
        revision: &Revision,
        handle: ThunkHandle,
    ) -> Result<ValueHandle, RevisionError> {
        self.session_mut(revision)?
            .force(handle)
            .map_err(RevisionError::Evaluation)
    }
    pub fn apply(
        &mut self,
        revision: &Revision,
        function: ThunkHandle,
        arguments: &[ThunkHandle],
    ) -> Result<ThunkHandle, RevisionError> {
        self.session_mut(revision)?
            .apply(function, arguments)
            .map_err(RevisionError::Evaluation)
    }
    pub fn inspect(
        &mut self,
        revision: &Revision,
        handle: ThunkHandle,
        budget: InspectionBudget,
    ) -> Result<Inspection, RevisionError> {
        self.session_mut(revision)?
            .inspect(handle, budget)
            .map_err(RevisionError::Evaluation)
    }
    pub fn collect_dependency_evidence(
        &mut self,
        revision: &Revision,
        roots: &[ThunkHandle],
        work: u64,
    ) -> Result<EvidenceCollection, RevisionError> {
        self.session_mut(revision)?
            .collect_dependency_evidence(roots, work)
            .map_err(RevisionError::Evaluation)
    }

    /// Stage an exact-code/world compacted candidate with explicit live roots.
    /// Optional roots follow `budget.retention`. Use normal inheritance and
    /// expected-base publication; old leases continue to own the original heap.
    /// A conservative collection miss returns work with no candidate, never an
    /// implicit fresh evaluation that would lose the requested live values.
    pub fn collect_snapshot(
        &mut self,
        base: &Revision,
        roots: &[RetainedRoot],
        execution: Limits,
        budget: CollectionBudget,
    ) -> Result<RevisionForkAttempt, RevisionError> {
        let previous = self.entry(base)?;
        if previous.session.abandoned {
            return Err(RevisionError::Interrupted);
        }
        if self.entries.len() >= self.limits.live_revisions {
            return Err(RevisionError::Capacity);
        }
        let context = previous.context.clone();
        let attempt = previous
            .session
            .collect_snapshot(
                roots,
                Self::execution_limits(execution, &self.limits),
                budget,
            )
            .map_err(RevisionError::Evaluation)?;
        let revision = attempt
            .fork
            .map(|fork| {
                self.insert_candidate(
                    base,
                    fork.session,
                    context,
                    Some((base.id, fork.inheritance)),
                )
            })
            .transpose()?;
        Ok(RevisionForkAttempt {
            work: attempt.work,
            revision,
        })
    }

    /// Append a checked operation to an unpublished candidate. Prefix code and
    /// inherited handles remain immutable; compatibility context updates only
    /// after all runtime/link/budget checks succeed.
    pub fn extend_program(
        &mut self,
        candidate: &Revision,
        extension: &knot_core::link::ProgramExtension,
        context: RevisionContext,
    ) -> Result<(), RevisionError> {
        if !matches!(self.entry(candidate)?.status, Status::Candidate(_)) {
            return Err(RevisionError::NotCandidate);
        }
        Self::check_program(&extension.program(), &context, &self.limits)?;
        self.session_mut(candidate)?
            .extend_program(extension)
            .map_err(RevisionError::Extension)?;
        self.entries.get_mut(candidate.id).unwrap().context = context;
        Ok(())
    }
    pub fn inherit_thunk(
        &self,
        base: &Revision,
        candidate: &Revision,
        handle: ThunkHandle,
    ) -> Result<ThunkHandle, RevisionError> {
        self.entry(base)?
            .session
            .validate_thunk(handle)
            .map_err(RevisionError::Evaluation)?;
        let entry = self.entry(candidate)?;
        let Some((id, proof)) = &entry.inheritance else {
            return Err(RevisionError::NoInheritance);
        };
        if *id != base.id {
            return Err(RevisionError::NoInheritance);
        }
        entry
            .session
            .inherit_thunk(proof, handle)
            .map_err(RevisionError::Evaluation)
    }
    pub fn inherit_value(
        &self,
        base: &Revision,
        candidate: &Revision,
        handle: ValueHandle,
    ) -> Result<ValueHandle, RevisionError> {
        self.entry(base)?
            .session
            .view(handle)
            .map_err(RevisionError::Evaluation)?;
        let entry = self.entry(candidate)?;
        let Some((id, proof)) = &entry.inheritance else {
            return Err(RevisionError::NoInheritance);
        };
        if *id != base.id {
            return Err(RevisionError::NoInheritance);
        }
        entry
            .session
            .inherit_value(proof, handle)
            .map_err(RevisionError::Evaluation)
    }

    /// Stage an isolated completion overlay. Caller may fork a pinned old base,
    /// but publishing it will fail if the accepted revision has changed.
    pub fn fork(
        &mut self,
        base: &Revision,
        program: Arc<Program>,
        context: RevisionContext,
        execution: Limits,
        hosts: Arc<HostRegistry>,
        reuse: impl Into<RevisionReuse>,
    ) -> Result<(Revision, ReuseStats), RevisionError> {
        let reuse = reuse.into();
        let previous = self.entry(base)?;
        if previous.session.abandoned {
            return Err(RevisionError::Interrupted);
        }
        Self::check_program(&program, &context, &self.limits)?;
        if self.entries.len() >= self.limits.live_revisions {
            return Err(RevisionError::Capacity);
        }
        let mut reuse = reuse;
        if let Some(bound) = &mut reuse.source {
            bound.retained_expressions = bound
                .retained_expressions
                .min(self.limits.code_expressions_per_revision);
        }
        let prepared = previous.session.prepare_revision(
            program,
            (&previous.context, &context),
            Self::execution_limits(execution, &self.limits),
            hosts,
            reuse,
        )?;
        let reused = prepared.reused;
        let session = prepared.session;
        let inheritance = prepared.inheritance.map(|proof| (base.id, proof));
        let revision = self.insert_candidate(base, session, context, inheritance)?;
        Ok((revision, reused))
    }
    fn insert_candidate(
        &mut self,
        base: &Revision,
        session: Session,
        context: RevisionContext,
        inheritance: Option<(storage::Id, SnapshotInheritance)>,
    ) -> Result<Revision, RevisionError> {
        let lease = Arc::new(());
        let id = self
            .entries
            .insert(
                Entry {
                    session,
                    context,
                    lease: Arc::downgrade(&lease),
                    status: Status::Candidate(base.id),
                    inheritance,
                },
                self.limits.live_revisions,
            )
            .map_err(|_| RevisionError::Capacity)?;
        Ok(Revision {
            owner: self.owner,
            id,
            lease,
        })
    }
    /// Publish runtime state after the embedding has checked the candidate and
    /// external-input/source freshness. This does not write source files. Both
    /// the declared base and current accepted revision must match exactly.
    pub fn publish(
        &mut self,
        candidate: &Revision,
        expected: &Revision,
    ) -> Result<(), RevisionError> {
        self.entry(expected)?;
        let entry = self.entry(candidate)?;
        let Status::Candidate(base) = entry.status else {
            return Err(RevisionError::NotCandidate);
        };
        if base != expected.id || self.accepted != *expected {
            return Err(RevisionError::StaleBase);
        }
        if entry.session.abandoned || entry.session.limits.cancellation.load(Ordering::Relaxed) {
            return Err(RevisionError::Interrupted);
        }
        self.entries.get_mut(candidate.id).unwrap().status = Status::Published;
        self.accepted = candidate.clone();
        Ok(())
    }
    /// Invalidate an unpublished candidate. Existing leases become expired; its
    /// storage is reclaimed by collect, with a bounded retirement allowance.
    pub fn cancel(&mut self, candidate: &Revision) -> Result<(), RevisionError> {
        let entry = self.entry(candidate)?;
        if !matches!(entry.status, Status::Candidate(_)) {
            return Err(RevisionError::NotCandidate);
        }
        // Changing the pin identity expires all clones without dropping an
        // unmetered Session here. Entry lookups check this identity too.
        self.entries.get_mut(candidate.id).unwrap().lease = Weak::new();
        Ok(())
    }
    /// Retire unpinned overlays, including cancelled candidates. Leases and the
    /// accepted revision are roots; ancestor IDs never keep old sessions alive.
    pub fn collect(&mut self, work: u64, cancellation: &AtomicBool) -> RevisionCollection {
        let mut roots = Vec::new();
        let mut scanned = 0u64;
        for (id, entry) in self.entries.entries() {
            if cancellation.load(Ordering::Relaxed) || scanned == work {
                return RevisionCollection {
                    status: if cancellation.load(Ordering::Relaxed) {
                        RevisionCollectionStatus::Cancelled
                    } else {
                        RevisionCollectionStatus::Truncated
                    },
                    work: scanned,
                    reclaimed: 0,
                    remaining: self.entries.len(),
                };
            }
            scanned += 1;
            if entry.lease.strong_count() > 0 {
                roots.push(id);
            }
        }
        let mut result = self
            .entries
            .collect(roots, work - scanned, cancellation, |_| {});
        result.work += scanned;
        result
    }
}
