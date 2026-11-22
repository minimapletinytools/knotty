//! One revision-preparation path for the revision owner and typed operations.
use super::*;
pub struct RevisionPreparation {
    pub session: Session,
    pub reused: ReuseStats,
    pub inheritance: Option<SnapshotInheritance>,
}
impl Session {
    /// Prepare a checked code revision under one cumulative budget. Contexts
    /// must describe the old and receiving nominal/dictionary/semantic ABI.
    /// Equal exact bytes authorize comparison, not reuse by themselves. Missing
    /// or changed contexts start fresh. Only exact-code/world snapshots grant
    /// handle inheritance; source sharing and rebases never retag old handles.
    pub fn prepare_revision(
        &self,
        program: Arc<Program>,
        contexts: (&RevisionContext, &RevisionContext),
        execution: Limits,
        hosts: Arc<HostRegistry>,
        reuse: RevisionReuse,
    ) -> Result<RevisionPreparation, RevisionError> {
        if self.abandoned {
            return Err(RevisionError::Evaluation(Outcome::Abandoned));
        }
        if execution.cancellation.load(Ordering::Relaxed) {
            return Err(RevisionError::Evaluation(Outcome::Cancelled));
        }
        let mut execution = execution;
        let compatibility_work = match (contexts.0, contexts.1) {
            (Some(a), Some(b)) if Arc::ptr_eq(a, b) => 0,
            (Some(a), Some(b)) => a.len().min(b.len()) as u64 + 1,
            _ => 0,
        };
        if compatibility_work > execution.steps {
            return Err(RevisionError::Evaluation(Outcome::BudgetExhausted));
        }
        let compatible = matches!((contexts.0, contexts.1), (Some(a), Some(b)) if a == b);
        execution.steps -= compatibility_work;
        let (preparation, shared) = if compatible && Arc::ptr_eq(&program, &self.program) {
            if Arc::ptr_eq(&hosts, &self.hosts) {
                let attempt = self
                    .try_fork(execution.clone())
                    .map_err(RevisionError::Evaluation)?;
                (
                    attempt.work,
                    attempt
                        .fork
                        .map(|fork| (fork.session, Some(fork.inheritance))),
                )
            } else {
                let attempt = self
                    .try_fork_inputs(hosts.clone(), execution.clone())
                    .map_err(RevisionError::Evaluation)?;
                (attempt.work, attempt.session.map(|session| (session, None)))
            }
        } else if let Some(bound) = reuse.source.filter(|_| compatible) {
            let attempt = self
                .try_fork_source(program.clone(), hosts.clone(), execution.clone(), bound)
                .map_err(RevisionError::Evaluation)?;
            (attempt.work, attempt.session.map(|session| (session, None)))
        } else {
            (0, None)
        };
        let (mut session, reused, inheritance) = if let Some((session, inheritance)) = shared {
            let reused = ReuseStats {
                shared_heap_pages: session.shared_heap_pages(),
                preparation_steps: preparation,
                ..ReuseStats::default()
            };
            (session, reused, inheritance)
        } else {
            execution.steps -= preparation;
            let mut session =
                Session::with_hosts(program, execution, hosts).map_err(RevisionError::Host)?;
            if session.heap_entries() > session.limits.heap_entries {
                return Err(RevisionError::Capacity);
            }
            let mut reused = if compatible {
                session.reuse_validated(self, reuse.transfer)
            } else {
                ReuseStats::default()
            };
            session.limits.steps += preparation;
            session.stats.steps += preparation;
            session.stats.maintenance_steps += preparation;
            reused.preparation_steps += preparation;
            (session, reused, None)
        };
        session.limits.steps += compatibility_work;
        session.stats.steps += compatibility_work;
        session.stats.maintenance_steps += compatibility_work;
        if session.limits.cancellation.load(Ordering::Relaxed) {
            return Err(RevisionError::Evaluation(Outcome::Cancelled));
        }
        Ok(RevisionPreparation {
            session,
            reused,
            inheritance,
        })
    }
}
