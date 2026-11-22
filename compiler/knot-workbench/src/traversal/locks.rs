//! Explicit entry-value locks retain original values across speculative revisions.
use super::*;

#[derive(Clone)]
pub(super) struct Lock {
    site: Site,
    request: TypedValue,
    ty: Ty,
    value: TypedValue,
    identity: solver_pins::Identity,
}

pub(super) struct Repair {
    pub candidate: Arc<ProjectSnapshot>,
    pub patches: Vec<LiteralPatch>,
    pub check_ms: f64,
    pub validation_ms: f64,
}

impl Walker<'_> {
    pub(super) fn prepare_user_locks(&mut self, names: &[String]) -> Result<(), EditError> {
        if names.len() > self.probes.limits.traversal.source_sites {
            return unsupported("value locks exceed the source site budget");
        }
        for name in names {
            self.probes.declarations.clear();
            let site = self.output_site(name)?;
            let identity = self.source_identity(&site).ok_or_else(|| {
                EditError::Unsupported("value lock needs an unambiguous checked source node".into())
            })?;
            if self.user_locks.iter().any(|lock| lock.identity == identity) {
                continue;
            }
            let value = self.source_value(&site)?;
            let ty = Ty::Named("Maybe".into(), vec![Ty::from_value(value.value_type())]);
            let reference = self.probes.reference(&value)?;
            self.probes.declarations.clear();
            let (request, _) = self
                .probes
                .evaluate_lazy(&format!("Just {reference}"), Some(&ty))?;
            if !self.fits(&site, &request, &ty)? {
                return Err(EditError::ValidationFailed);
            }
            self.user_locks.push(Lock {
                site,
                request,
                ty,
                value,
                identity,
            });
        }
        self.reset_user_pins();
        Ok(())
    }

    pub(super) fn reset_user_pins(&mut self) {
        self.solver_pins.user = self
            .user_locks
            .iter()
            .map(|lock| (lock.identity.clone(), lock.value.clone()))
            .collect();
    }

    pub(super) fn is_user_lock_site(&self, site: &Site) -> bool {
        self.source_identity(site)
            .is_some_and(|id| self.solver_pins.user.contains_key(&id))
    }

    pub(super) fn validate_user_locks(&mut self) -> Result<(), EditError> {
        for lock in self.user_locks.clone() {
            if !self.fits(&lock.site, &lock.request, &lock.ty)? {
                return Err(EditError::ValidationFailed);
            }
        }
        Ok(())
    }

    pub(super) fn repair_user_locks(
        &mut self,
        roots: &[std::path::PathBuf],
        libraries: &knot_core::Program,
    ) -> Result<Option<Repair>, EditError> {
        let mut latest = None;
        let mut check_ms = 0.0;
        let mut validation_ms = 0.0;
        loop {
            let phase = Instant::now();
            let mut broken = None;
            for lock in self.user_locks.clone() {
                if !self.fits(&lock.site, &lock.request, &lock.ty)? {
                    broken = Some(lock);
                    break;
                }
            }
            validation_ms += phase.elapsed().as_secs_f64() * 1000.0;
            let Some(lock) = broken else { break };
            if self.lock_repairs >= self.probes.limits.traversal.lock_repairs {
                return unsupported("value lock repair budget exceeded");
            }
            self.lock_repairs += 1;
            self.solver_pins.repairing = Some(lock.identity.clone());
            self.collect_proposals = true;
            let phase = Instant::now();
            let routed = self.walk(lock.site.clone(), lock.request.clone(), lock.ty.clone());
            self.solver_pins.repairing = None;
            routed?;
            validation_ms += phase.elapsed().as_secs_f64() * 1000.0;
            let (candidate, patches, check, validation) =
                self.validate_candidate(roots, libraries)?;
            check_ms += check;
            validation_ms += validation;
            let phase = Instant::now();
            if !self.fits(&lock.site, &lock.request, &lock.ty)? {
                return Err(EditError::ValidationFailed);
            }
            self.freeze_solutions()?;
            validation_ms += phase.elapsed().as_secs_f64() * 1000.0;
            latest = Some((candidate, patches));
        }
        Ok(latest.map(|(candidate, patches)| Repair {
            candidate,
            patches,
            check_ms,
            validation_ms,
        }))
    }
}
