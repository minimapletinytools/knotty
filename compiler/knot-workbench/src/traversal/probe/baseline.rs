//! A checked immutable baseline, shared by value forks, never by requests.
use super::*;
use crate::operation::{OperationSession, PreparedOperations};

impl ProbeContext {
    pub(in crate::traversal) fn seed(
        &mut self,
        previous: &OperationSession,
        limits: twine::Limits,
    ) -> Result<super::super::BaselineStats, EditError> {
        let entry = self.type_modules.last().expect("checked entry module");
        let prepared = PreparedOperations::from_linked(self.base.shared_program(), entry, &[])
            .map_err(EditError::Check)?;
        let modules = self
            .type_modules
            .iter()
            .map(Arc::as_ref)
            .collect::<Vec<_>>();
        let Ok(prepared) = prepared.with_source_context(&modules, 1024 * 1024) else {
            return Ok(Default::default());
        };
        let (session, reused) = prepared
            .start_reusing(
                previous,
                limits,
                Arc::new(twine::HostRegistry::default()),
                twine::RevisionReuse {
                    source: Some(twine::SourceForkLimits::default()),
                    ..Default::default()
                },
            )
            .map_err(operation_error)?;
        let stats = super::super::BaselineStats {
            work: session.stats().steps,
            shared_heap_pages: reused.shared_heap_pages,
            reused_globals: reused.globals,
        };
        if reused.shared_heap_pages > 0 || reused.globals > 0 {
            *self.baseline.get_mut().unwrap() = Some(session);
        }
        Ok(stats)
    }
}

impl Probes {
    pub(super) fn start_from_baseline(
        &mut self,
        prepared: &PreparedOperations,
        limits: &mut twine::Limits,
    ) -> Result<Option<OperationSession>, EditError> {
        let baseline = self.context.baseline.lock().unwrap();
        let Some(baseline) = baseline.as_ref() else {
            return Ok(None);
        };
        let attempt = prepared
            .try_start_from(baseline, limits.clone())
            .map_err(operation_error)?;
        self.runtime_fork_work = self.runtime_fork_work.saturating_add(attempt.work);
        if attempt.session.is_some() {
            self.runtime_forks += 1;
            // with_live charges all child work, including preparation, once.
        } else {
            self.usage.steps = self.usage.steps.saturating_add(attempt.work);
            limits.steps = limits.steps.saturating_sub(attempt.work);
        }
        Ok(attempt.session)
    }
}
