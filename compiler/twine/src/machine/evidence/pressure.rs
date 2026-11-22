//! Conservative online reclamation boundary. Active observations, recovery
//! recipes remain mandatory; optional indexed/global results compete for space.
use super::*;
mod selection;

#[derive(Debug, Clone, Copy)]
pub struct EvidencePressure {
    /// Maximum work per attempt, also capped by newly performed useful work / 8.
    pub steps: u64,
    /// Separate bounded temporary storage for root discovery and mark/sweep.
    pub scratch_bytes: usize,
    /// Select optional results by measured benefit and complete proof storage.
    /// False preserves all existing indexes/globals as a comparison control.
    pub select_optional: bool,
}
impl Default for EvidencePressure {
    fn default() -> Self {
        Self {
            steps: 1_000_000,
            scratch_bytes: 8 * 1024 * 1024,
            select_optional: true,
        }
    }
}

#[derive(Default)]
pub(in crate::machine) struct Schedule {
    last_work: u64,
    next_work: u64,
    cooldown: u64,
}

impl Store {
    fn under_pressure(&self, limits: &Limits) -> bool {
        let near = |used: usize, limit: usize| limit > 0 && used >= limit.saturating_sub(limit / 4);
        near(
            self.records.len().saturating_add(self.active_records),
            limits.dependency_records,
        ) || near(
            self.stats.reads.saturating_add(self.active_reads),
            limits.dependency_reads,
        ) || near(
            self.stats.bytes.saturating_add(self.active_bytes),
            limits.dependency_bytes,
        )
    }
}

impl Session {
    pub(in crate::machine) fn maybe_collect_evidence(&mut self, reads: &Recorder, state: &State) {
        let Some(policy) = self.limits.evidence_pressure else {
            return;
        };
        // Inspection owns an allocation journal, not an undo log for deletion.
        // Suppressed recording cannot be repaired retroactively by reclamation.
        if self.inspection.is_some()
            || reads.suppressed > 0
            || !self.evidence.under_pressure(&self.limits)
        {
            return;
        }
        let now = self
            .stats
            .steps
            .saturating_sub(self.stats.maintenance_steps);
        if now < self.evidence_pressure.next_work {
            return;
        }
        let allowance = policy
            .steps
            .min(now.saturating_sub(self.evidence_pressure.last_work) / 8)
            .min(self.limits.steps.saturating_sub(self.stats.steps) / 8);
        // Both recovery discovery and the collector must scan arena slots.
        // Do not allocate or retry work that cannot cover even those two scans.
        let minimum = (self.evidence.records.slot_count() as u64)
            .saturating_mul(2)
            .max(1);
        if allowance < minimum {
            return;
        }
        self.evidence_pressure.last_work = now;
        let report = self.cache_work(|session| {
            session.collect_evidence_pressure(reads, state, policy, allowance)
        });
        // Failed attempts cannot self-fund retries through maintenance charges.
        // Even a zero-work decline requires another 1,024 units of useful work.
        let delay = if report.status == EvidenceCollectionStatus::Collected && report.reclaimed > 0
        {
            report.work.saturating_mul(8).max(1024)
        } else {
            // A too-small attempt must accumulate a larger useful-work allowance
            // next time, rather than repeatedly funding the same truncated scan.
            self.evidence_pressure
                .cooldown
                .saturating_mul(2)
                .max(report.work.saturating_mul(16))
                .max(1024)
                .min(policy.steps.saturating_mul(8).max(1024))
        };
        self.evidence_pressure.cooldown = delay;
        self.evidence_pressure.next_work = now.saturating_add(delay);
        self.stats.evidence_pressure_attempts += 1;
        self.stats.evidence_pressure_work += report.work;
        self.stats.evidence_pressure_reclaimed += report.reclaimed as u64;
        if report.status == EvidenceCollectionStatus::Collected {
            self.stats.evidence_pressure_collections += 1;
        } else {
            self.stats.evidence_pressure_declined += 1;
        }
    }

    fn collect_evidence_pressure(
        &mut self,
        reads: &Recorder,
        state: &State,
        policy: EvidencePressure,
        allowance: u64,
    ) -> EvidenceCollection {
        let mut remaining = allowance;
        let cancellation = &self.limits.cancellation;
        let mut selected = std::collections::HashSet::new();
        // Reserve conservative mark/worklist/garbage/free-list storage before
        // allocating roots. Arena slot high-water matters even after reclamation.
        let scan = self.evidence.records.slot_count();
        let collector_bytes = scan.saturating_mul(64);
        let capacity = policy.scratch_bytes.saturating_sub(collector_bytes) / 64;
        let capacity = capacity.min(16_384);
        let tick = |left: &mut u64, n: u64| {
            if cancellation.load(Ordering::Relaxed) {
                return Err(EvidenceCollectionStatus::Cancelled);
            }
            if n > *left {
                return Err(EvidenceCollectionStatus::Truncated);
            }
            *left -= n;
            Ok(())
        };
        let add = |selected: &mut std::collections::HashSet<storage::Id>,
                   left: &mut u64,
                   proof: Proof,
                   required: bool| {
            tick(left, 1)?;
            if self.evidence.get(proof).is_none() {
                return if required {
                    Err(EvidenceCollectionStatus::InvalidEdge)
                } else {
                    Ok(())
                };
            }
            let id = proof.id.unwrap();
            if selected.contains(&id) {
                return Ok(());
            }
            if selected.len() >= capacity {
                return Err(EvidenceCollectionStatus::Truncated);
            }
            tick(left, 64)?;
            selected.insert(id);
            Ok(())
        };
        let prepared = (|| {
            if self.inspection.is_some() {
                return Err(EvidenceCollectionStatus::CheckpointActive);
            }
            if collector_bytes > policy.scratch_bytes {
                return Err(EvidenceCollectionStatus::Truncated);
            }
            for frame in &reads.active {
                tick(&mut remaining, 1)?;
                if frame.complete {
                    for proof in &frame.reads {
                        add(&mut selected, &mut remaining, *proof, true)?;
                    }
                }
            }
            // A completed Force may be about to enter the recorder for the first
            // time. Protect it before this step reads the value/proof pair.
            if let State::Force(cell) = state {
                if let ThunkState::Done(Ok(done)) = self.cell_state(*cell) {
                    add(&mut selected, &mut remaining, done.proof, false)?;
                }
            }
            // Keep a non-expression recipe only when this proof is still the
            // completion's recovery owner. Revision transfer may already own a
            // detached recipe; an obsolete completion no longer needs this one.
            // This changes ownership roots, never the proof's validity meaning.
            tick(&mut remaining, scan as u64)?;
            for (id, record) in self.evidence.records.entries() {
                if let Recipe::Thunk(cell, pending) = &record.recipe {
                    if !matches!(pending, Pending::Expr(..)) {
                        tick(&mut remaining, 2)?;
                        if !self.validation.owns_recovery(*cell)
                            && self.thunks.get(cell.0).is_some_and(|thunk| {
                                matches!(&thunk.state, ThunkState::Done(Ok(done)) if done.proof.id == Some(id))
                            })
                        {
                            add(&mut selected, &mut remaining, Proof { id: Some(id) }, true)?;
                        }
                    }
                }
            }
            if !policy.select_optional {
                for cell in self.globals.values() {
                    tick(&mut remaining, 1)?;
                    if let ThunkState::Done(Ok(done)) = self.cell_state(*cell) {
                        add(&mut selected, &mut remaining, done.proof, false)?;
                    }
                }
                for candidate in self.memo.candidates() {
                    tick(&mut remaining, 1)?;
                    let proof = match candidate {
                        memo::Candidate::Call(_, done) => Some(done.proof),
                        memo::Candidate::Expression(_, cell)
                        | memo::Candidate::Capture(_, _, cell)
                        | memo::Candidate::Global(cell) => match self.cell_state(cell.owner()) {
                            ThunkState::Done(Ok(done)) => Some(done.proof),
                            _ => None,
                        },
                    };
                    if let Some(proof) = proof {
                        add(&mut selected, &mut remaining, proof, false)?;
                    }
                }
            }
            Ok(())
        })();
        let mut result = EvidenceCollection {
            status: prepared
                .err()
                .unwrap_or(EvidenceCollectionStatus::Collected),
            work: allowance - remaining,
            reclaimed: 0,
            remaining: self.evidence.records.len(),
        };
        if result.status == EvidenceCollectionStatus::Collected {
            self.stats.evidence_pressure_prepared += 1;
            let collected = if policy.select_optional {
                selection::collect(self, selected, policy, remaining)
            } else {
                let stats = &mut self.evidence.stats;
                self.evidence
                    .records
                    .collect(selected, remaining, cancellation, |record| {
                        stats.reads -= record.reads.len();
                        stats.bytes -= record.bytes;
                        stats.certificates -= 1;
                        stats.summaries -= usize::from(matches!(record.recipe, Recipe::Summary(_)));
                    })
            };
            result.work += collected.work;
            result.status = collected.status;
            result.reclaimed = collected.reclaimed;
            result.remaining = collected.remaining;
        }
        self.stats.steps += result.work;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn name(binding: &str) -> GlobalName {
        GlobalName {
            module: vec!["Test".into()],
            name: binding.into(),
        }
    }
    fn session(source: &str, limits: Limits) -> Session {
        let parsed = knot_syntax::parse(&format!("module Test exposing (..)\n{source}")).unwrap();
        let canonical = knot_canonical::canonicalize_module(&parsed, None).unwrap();
        let program = knot_core::lower(&[
            knot_checker::check::check_library_program("Prelude").unwrap(),
            knot_checker::check::check_program(&canonical).unwrap(),
        ]);
        Session::new(Arc::new(program), limits)
    }
    fn record(s: &mut Session, reads: Vec<Proof>) -> Proof {
        let bytes = BASE_BYTES + reads.len() * READ_BYTES;
        s.evidence.insert_record(
            Recipe::Historical(None),
            None,
            reads,
            Default::default(),
            bytes,
            &s.limits,
        )
    }
    fn cell(s: &mut Session, value: ValueId, proof: Proof) -> CellId {
        let id = s.thunks.len();
        let template = s.thunks[s.globals[&Global::Value(name("a"))].0].clone();
        s.thunks.push(Thunk {
            state: ThunkState::Done(Ok(Completed {
                value,
                expression: Some(ExprId(0)),
                cost: Default::default(),
                proof,
                stamp: s.validation.stamp(),
                introduced: false,
            })),
            ..template
        });
        CellId(id)
    }
    fn collect(
        s: &mut Session,
        reads: &Recorder,
        state: &State,
        policy: EvidencePressure,
        work: u64,
    ) -> EvidenceCollection {
        s.cache_work(|s| s.collect_evidence_pressure(reads, state, policy, work))
    }
    #[test]
    fn active_reads_recoveries_pending_force_and_every_index_kind_survive() {
        let mut s = session("a :: Int\na = 7\n", Limits::default());
        let value = s.evaluate(&name("a")).unwrap().value;
        let leaf = record(&mut s, vec![]);
        let active = record(&mut s, vec![leaf]);
        let called = record(&mut s, vec![leaf]);
        let closed = record(&mut s, vec![leaf]);
        let captured = record(&mut s, vec![leaf]);
        let forced = record(&mut s, vec![leaf]);
        let garbage = record(&mut s, vec![]);
        let root = s.cell_id(s.globals[&Global::Value(name("a"))]);
        let recovery_cell = cell(&mut s, value, Proof::default());
        let recovery = s.evidence.insert_record(
            Recipe::Thunk(recovery_cell, Pending::Projection(root.owner(), 0)),
            None,
            vec![leaf],
            Default::default(),
            BASE_BYTES + READ_BYTES,
            &s.limits,
        );
        if let ThunkState::Done(Ok(done)) = &mut s.thunks[recovery_cell.0].state {
            done.proof = recovery;
            done.expression = None;
        }
        let mut reads = Recorder::default();
        s.record_evidence(|store, limits, work| {
            reads.begin_thunk(root, &Pending::Expr(ExprId(0), 0), store, limits, work);
            reads.hit(active, store, limits, work);
        });
        let closed_cell = cell(&mut s, value, closed);
        let captured_cell = cell(&mut s, value, captured);
        let forced_cell = cell(&mut s, value, forced);
        s.memo.remember_expression(ExprId(0), closed_cell, 100);
        assert!(s
            .memo
            .remember_capture(ExprId(1), vec![(LocalId(0), root)], captured_cell, 100));
        s.memo.insert_completion(
            memo::Call::allocated(value, root),
            memo::Completion {
                value,
                proof: called,
                stamp: s.validation.stamp(),
                ..Default::default()
            },
            100,
        );
        let result = collect(
            &mut s,
            &reads,
            &State::Force(forced_cell.owner()),
            EvidencePressure::default(),
            100_000,
        );
        assert_eq!(result.status, EvidenceCollectionStatus::Collected);
        assert!(result.reclaimed > 0);
        assert!(s.evidence.get(garbage).is_none());
        for proof in [leaf, active, called, closed, captured, forced, recovery] {
            assert!(s.evidence.get(proof).is_some(), "protected {proof:?}");
        }
        let completed = s.record_evidence(|store, limits, work| {
            reads.finish(value, Default::default(), store, limits, work)
        });
        assert_eq!(s.evidence.get(completed).unwrap().reads.as_ref(), &[active]);
        assert!(matches!(
            s.evidence.recovery(recovery, recovery_cell),
            Some(Pending::Projection(..))
        ));
    }
    #[test]
    fn incomplete_root_discovery_budget_cancellation_and_journals_never_sweep() {
        let mut s = session("a :: Int\na = 7\n", Limits::default());
        let value = s.evaluate(&name("a")).unwrap().value;
        for _ in 0..32 {
            record(&mut s, vec![]);
        }
        let before = s.evidence.stats;
        let reads = Recorder::default();
        for (policy, work) in [
            (EvidencePressure::default(), 1),
            (
                EvidencePressure {
                    scratch_bytes: 0,
                    ..Default::default()
                },
                100_000,
            ),
        ] {
            let report = collect(&mut s, &reads, &State::Return(value), policy, work);
            assert_eq!(report.status, EvidenceCollectionStatus::Truncated);
            assert_eq!(report.reclaimed, 0);
            assert_eq!(s.evidence.stats, before);
        }
        s.limits.cancellation.store(true, Ordering::Relaxed);
        assert_eq!(
            collect(
                &mut s,
                &reads,
                &State::Return(value),
                EvidencePressure::default(),
                100_000
            )
            .status,
            EvidenceCollectionStatus::Cancelled
        );
        s.limits.cancellation.store(false, Ordering::Relaxed);
        let checkpoint = s.evidence.snapshot();
        assert_eq!(
            collect(
                &mut s,
                &reads,
                &State::Return(value),
                EvidencePressure::default(),
                100_000
            )
            .status,
            EvidenceCollectionStatus::CheckpointActive
        );
        s.evidence.rollback(checkpoint);
        assert_eq!(s.evidence.stats, before);
        let mut missing = Recorder::default();
        missing.active.push(Building {
            recipe: Recipe::Historical(None),
            reads: vec![Proof::default()],
            complete: true,
            bytes: BASE_BYTES,
        });
        assert_eq!(
            collect(
                &mut s,
                &missing,
                &State::Return(value),
                EvidencePressure::default(),
                100_000
            )
            .status,
            EvidenceCollectionStatus::InvalidEdge
        );
        assert_eq!(s.evidence.stats, before);
    }
    #[test]
    fn retry_budget_uses_only_new_useful_work_and_forks_reset_the_schedule() {
        let mut s = session(
            "a :: Int\na = 7\n",
            Limits {
                dependency_records: 128,
                evidence_pressure: Some(EvidencePressure::default()),
                ..Default::default()
            },
        );
        let value = s.evaluate(&name("a")).unwrap().value;
        for _ in 0..100 {
            record(&mut s, vec![]);
        }
        s.stats.steps += 100_000;
        let reads = Recorder::default();
        let own = s.stats.steps - s.stats.maintenance_steps;
        s.maybe_collect_evidence(&reads, &State::Return(value));
        assert_eq!(s.stats.evidence_pressure_attempts, 1);
        assert!(s.stats.evidence_pressure_reclaimed > 0);
        assert!(s.stats.evidence_pressure_work <= own / 8);
        let garbage = record(&mut s, vec![]);
        for _ in 0..100 {
            record(&mut s, vec![]);
        }
        for _ in 0..100 {
            s.maybe_collect_evidence(&reads, &State::Return(value));
        }
        assert_eq!(
            s.stats.evidence_pressure_attempts, 1,
            "maintenance does not fund another scan"
        );
        let fork = s.try_fork(s.limits.clone()).unwrap().fork.unwrap();
        assert_eq!(fork.session.evidence_pressure.next_work, 0);
        assert!(s.evidence.get(garbage).is_some());
    }

    #[test]
    fn truncated_attempts_back_off_until_a_larger_scan_can_finish() {
        let mut s = session(
            "a :: Int\na = 7\n",
            Limits {
                dependency_records: 128,
                evidence_pressure: Some(EvidencePressure::default()),
                ..Default::default()
            },
        );
        let value = s.evaluate(&name("a")).unwrap().value;
        for _ in 0..100 {
            record(&mut s, vec![]);
        }
        let reads = Recorder::default();
        s.stats.steps += 3000;
        s.maybe_collect_evidence(&reads, &State::Return(value));
        assert_eq!(s.stats.evidence_pressure_declined, 1);
        for _ in 0..8 {
            if s.stats.evidence_pressure_reclaimed > 0 {
                break;
            }
            let needed =
                s.evidence_pressure.next_work - (s.stats.steps - s.stats.maintenance_steps);
            s.stats.steps += needed;
            s.maybe_collect_evidence(&reads, &State::Return(value));
        }
        assert!(
            s.stats.evidence_pressure_reclaimed > 0,
            "repeated small budgets must not livelock"
        );
        assert!(s.stats.evidence_pressure_work <= (s.stats.steps - s.stats.maintenance_steps) / 8);
    }
    #[test]
    fn evaluation_keeps_lazy_results_and_costs_and_inspection_never_reclaims() {
        let source = "loop :: Int -> Int\nloop n = if n == 0 then 0 else 1 + loop (n - 1)\na = loop 240\nbox = { good = loop 260, bad = error \"unforced\" }\nanswer = box.good\n";
        for compact in [false, true] {
            let limits = Limits {
                dependency_records: 128,
                application_cache_entries: 0,
                compact_evidence: compact,
                region_evidence: compact,
                ..Default::default()
            };
            let mut control = session(source, limits.clone());
            let mut pressured = Session::new(
                control.program.clone(),
                Limits {
                    evidence_pressure: Some(EvidencePressure::default()),
                    ..limits
                },
            );
            for binding in ["a", "answer"] {
                let a = control.evaluate(&name(binding)).unwrap();
                let b = pressured.evaluate(&name(binding)).unwrap();
                assert_eq!(control.view(a).unwrap(), pressured.view(b).unwrap());
                let a = control.global_handle(&name(binding)).unwrap();
                let b = pressured.global_handle(&name(binding)).unwrap();
                assert_eq!(
                    control.work_cost(a).unwrap(),
                    pressured.work_cost(b).unwrap()
                );
            }
            assert!(pressured.stats.evidence_pressure_attempts > 0);
            assert!(
                pressured.stats.evidence_pressure_work
                    <= (pressured.stats.steps - pressured.stats.maintenance_steps) / 8
            );
            let box_value = pressured.evaluate(&name("box")).unwrap();
            let bad = pressured
                .children(box_value, 0, 2)
                .unwrap()
                .into_iter()
                .find(|c| c.field == Field::Named("bad".into()))
                .unwrap();
            assert!(matches!(bad.state, Inspection::Unevaluated));
            let attempts = pressured.stats.evidence_pressure_attempts;
            pressured
                .inspect(bad.handle, InspectionBudget::default())
                .unwrap();
            assert_eq!(pressured.stats.evidence_pressure_attempts, attempts);
        }
    }

    #[test]
    fn reclaimed_slots_do_not_certify_changed_inputs_or_callbacks_after_source_edits() {
        let source = |input, callback| {
            format!("input :: Int\ninput = {input}\ncallback :: Int -> Int\ncallback x = x + {callback}\napply :: (Int -> Int) -> Int -> Int\napply f x = f x\na :: Int\na = apply callback input\nbox = {{ good = a, bad = error \"unused\" }}\n")
        };
        let limits = Limits {
            dependency_records: 512,
            compact_evidence: true,
            evidence_pressure: Some(EvidencePressure::default()),
            ..Default::default()
        };
        let mut old = session(&source(1, 10), limits.clone());
        let stale = record(&mut old, vec![]);
        for _ in 0..400 {
            record(&mut old, vec![]);
        }
        old.stats.steps += 100_000;
        let value = old.evaluate(&name("a")).unwrap();
        assert_eq!(
            old.view(value).unwrap(),
            ValueView::Literal(&Literal::Int(11))
        );
        assert!(old.stats.evidence_pressure_reclaimed > 0);
        assert!(old.evidence.get(stale).is_none());
        let root = old.global_handle(&name("a")).unwrap();
        assert!(old.dependency_reads(root, 0, 100).unwrap().is_some());
        for (input, callback, expected) in [(2, 10, 12), (1, 20, 21)] {
            let mut next = session(&source(input, callback), limits.clone());
            next.reuse_validated(&old, ReuseLimits::default());
            let result = next.evaluate(&name("a")).unwrap();
            assert_eq!(
                next.view(result).unwrap(),
                ValueView::Literal(&Literal::Int(expected))
            );
            let box_value = next.evaluate(&name("box")).unwrap();
            let bad = next
                .children(box_value, 0, 2)
                .unwrap()
                .into_iter()
                .find(|c| c.field == Field::Named("bad".into()))
                .unwrap();
            assert!(matches!(bad.state, Inspection::Unevaluated));
        }
    }

    #[test]
    fn versioned_host_reads_still_invalidate_after_online_reclamation() {
        let hosts = |input: i64| {
            let mut hosts = HostRegistry::new();
            hosts
                .register_versioned_function(
                    GlobalName {
                        module: vec!["Input".into()],
                        name: "read".into(),
                    },
                    knot_core::HostSignature {
                        parameters: vec![knot_core::HostType::Unit],
                        result: knot_core::HostType::Int,
                    },
                    crate::HostVersion {
                        implementation: "read-v1".into(),
                        inputs: crate::SnapshotId {
                            namespace: "pressure-test".into(),
                            version: input.to_string(),
                        },
                    },
                    move |_, _| Ok(crate::HostValue::Int(input)),
                )
                .unwrap();
            Arc::new(hosts)
        };
        let first = hosts(1);
        let parsed = knot_syntax::parse(
            "module Test exposing (..)\nimport Input\na :: Int\na = Input.read () + 10\nf :: Int -> Int\nf x = Input.read () + x\nten :: Int\nten = 10\n",
        )
        .unwrap();
        let canonical =
            knot_canonical::canonicalize_module(&parsed, Some(first.declarations())).unwrap();
        let code = Arc::new(knot_core::lower(&[
            knot_checker::check::check_library_program("Prelude").unwrap(),
            knot_checker::check::check_program_with_hosts(&canonical, first.declarations())
                .unwrap(),
        ]));
        let limits = Limits {
            dependency_records: 512,
            compact_evidence: true,
            evidence_pressure: Some(EvidencePressure::default()),
            ..Default::default()
        };
        let mut old = Session::with_hosts(code, limits.clone(), first).unwrap();
        for _ in 0..400 {
            record(&mut old, vec![]);
        }
        old.stats.steps += 100_000;
        let result = old.evaluate(&name("a")).unwrap();
        assert_eq!(
            old.view(result).unwrap(),
            ValueView::Literal(&Literal::Int(11))
        );
        assert!(old.stats.evidence_pressure_reclaimed > 0);
        let function = old.global_handle(&name("f")).unwrap();
        let argument = old.global_handle(&name("ten")).unwrap();
        let applied = old.apply(function, &[argument]).unwrap();
        let result = old.force(applied).unwrap();
        assert_eq!(
            old.view(result).unwrap(),
            ValueView::Literal(&Literal::Int(11))
        );
        let owner = old.cell_id(ThunkId(applied.thunk));
        let ThunkState::Done(Ok(done)) = old.cell_state(owner.owner()) else {
            panic!("completed application")
        };
        let proof = done.proof;
        assert!(old.evidence.recovery(proof, owner).is_some());
        let mut next = old
            .try_fork_inputs(hosts(2), limits)
            .unwrap()
            .session
            .unwrap();
        assert!(next.validation.owns_recovery(owner));
        next.memo = Default::default();
        let report = collect(
            &mut next,
            &Recorder::default(),
            &State::Return(result.value),
            EvidencePressure::default(),
            100_000,
        );
        assert_eq!(report.status, EvidenceCollectionStatus::Collected);
        assert!(next.evidence.get(proof).is_none());
        // This private test addresses the retained child cell; input forks still
        // do not grant public inheritance of the parent's handles.
        let applied = next.thunk_handle(owner.owner());
        let result = next.force(applied).unwrap();
        assert_eq!(
            next.view(result).unwrap(),
            ValueView::Literal(&Literal::Int(12))
        );
        let result = next.evaluate(&name("a")).unwrap();
        assert_eq!(
            next.view(result).unwrap(),
            ValueView::Literal(&Literal::Int(12))
        );
    }

    #[test]
    fn detached_and_obsolete_recovery_records_do_not_pin_optional_proof_graphs() {
        let mut s = session("a :: Int\na = 7\n", Limits::default());
        let value = s.evaluate(&name("a")).unwrap().value;
        let input = s.globals[&Global::Value(name("a"))];
        let detached = cell(&mut s, value, Proof::default());
        let obsolete = cell(&mut s, value, Proof::default());
        let mut proofs = Vec::new();
        for owner in [detached, obsolete] {
            let leaf = record(&mut s, vec![]);
            let proof = s.evidence.insert_record(
                Recipe::Thunk(owner, Pending::Projection(input, 0)),
                None,
                vec![leaf],
                Default::default(),
                BASE_BYTES + READ_BYTES,
                &s.limits,
            );
            if let ThunkState::Done(Ok(done)) = &mut s.thunks[owner.0].state {
                done.expression = None;
                done.proof = proof;
            }
            proofs.extend([leaf, proof]);
        }
        s.validation
            .retain_recovery(detached, Pending::Projection(input, 0));
        // A newer completion no longer uses the obsolete recipe/proof.
        if let ThunkState::Done(Ok(done)) = &mut s.thunks[obsolete.0].state {
            done.proof = Proof::default();
            done.introduced = true;
        }
        let report = collect(
            &mut s,
            &Recorder::default(),
            &State::Return(value),
            EvidencePressure::default(),
            100_000,
        );
        assert_eq!(report.status, EvidenceCollectionStatus::Collected);
        assert!(proofs.iter().all(|proof| s.evidence.get(*proof).is_none()));
        assert!(matches!(
            s.recovery_recipe(detached, proofs[1]),
            Some(Pending::Projection(..))
        ));
        let before = s.stats.thunk_evaluations;
        assert_eq!(
            s.force(s.thunk_handle(detached.owner())).unwrap().value,
            value
        );
        assert_eq!(s.stats.thunk_evaluations, before);
    }

    #[test]
    fn optional_selection_keeps_expensive_support_and_atomically_prunes_all_index_kinds() {
        let mut s = session(
            "a :: Int\na = 7\n",
            Limits {
                dependency_records: 64,
                ..Default::default()
            },
        );
        let value = s.evaluate(&name("a")).unwrap().value;
        let mut expensive = record(&mut s, vec![]);
        for _ in 0..19 {
            expensive = record(&mut s, vec![expensive]);
        }
        let mut cheap = record(&mut s, vec![]);
        for _ in 0..34 {
            cheap = record(&mut s, vec![cheap]);
        }
        assert!(s.evidence.get(expensive).is_some() && s.evidence.get(cheap).is_some());
        let expensive_cell = cell(&mut s, value, expensive);
        let cheap_cell = cell(&mut s, value, cheap);
        let cost = |n| {
            let mut meter = cost::Meter::default();
            let mark = meter.begin(0);
            meter.finish(mark, n)
        };
        if let ThunkState::Done(Ok(done)) = &mut s.thunks[expensive_cell.0].state {
            done.cost = cost(100_000);
        }
        if let ThunkState::Done(Ok(done)) = &mut s.thunks[cheap_cell.0].state {
            done.cost = cost(10);
        }
        s.memo.remember_expression(ExprId(0), expensive_cell, 100);
        s.memo.remember_expression(ExprId(1), cheap_cell, 100);
        assert!(s.memo.remember_capture(
            ExprId(2),
            vec![(LocalId(0), cheap_cell)],
            cheap_cell,
            100
        ));
        let key = memo::Call::allocated(value, cheap_cell);
        s.memo.insert_completion(
            key.clone(),
            memo::Completion {
                value,
                cost: cost(10),
                proof: cheap,
                stamp: s.validation.stamp(),
                ..Default::default()
            },
            100,
        );
        let before = (s.memo.len(), s.evidence.stats);
        let failed = collect(
            &mut s,
            &Recorder::default(),
            &State::Return(value),
            EvidencePressure::default(),
            100,
        );
        assert_ne!(failed.status, EvidenceCollectionStatus::Collected);
        assert_eq!(
            (s.memo.len(), s.evidence.stats),
            before,
            "a failed plan cannot prune indexes or proofs"
        );
        let report = collect(
            &mut s,
            &Recorder::default(),
            &State::Return(value),
            EvidencePressure::default(),
            100_000,
        );
        assert_eq!(report.status, EvidenceCollectionStatus::Collected);
        assert!(s.evidence.get(expensive).is_some());
        assert!(s.evidence.get(cheap).is_none());
        assert_eq!(s.memo.expression(ExprId(0)), Some(expensive_cell));
        assert!(s.memo.expression(ExprId(1)).is_none());
        assert!(s
            .memo
            .captured(ExprId(2), &[(LocalId(0), cheap_cell)])
            .is_none());
        assert!(s.memo.get(&key).is_none());
        assert_eq!(s.stats.evidence_pressure_evicted_indexes, 3);
        let evaluations = s.stats.thunk_evaluations;
        assert_eq!(s.machine(State::Force(cheap_cell.owner())).unwrap(), value);
        assert_eq!(
            s.stats.thunk_evaluations, evaluations,
            "ordinary thunk memoization is preserved"
        );
    }
}
