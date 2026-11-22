//! Ordered production read evidence, independent of optional cache admission and
//! the diagnostic trace. IDs stay inside one Session; source transfer rebuilds
//! proof-only code/read records without retaining old runtime handles or results.
//! An incomplete observation is never published as an empty proof.
use super::*;
#[cfg(feature = "incremental")]
mod observation;
#[cfg(feature = "incremental")]
pub(super) use observation::Witness;
#[cfg(feature = "incremental")]
mod footprint;
#[cfg(feature = "incremental")]
pub(super) mod pressure;
#[cfg(feature = "incremental")]
pub(super) use footprint::StorageTraversal;
#[cfg(feature = "incremental")]
pub use pressure::EvidencePressure;
#[cfg(feature = "incremental")]
mod pricing;
#[cfg(feature = "incremental")]
mod regions;
#[cfg(feature = "incremental")]
mod summary;
#[cfg(feature = "incremental")]
pub(super) use summary::SummaryCode;
#[cfg(feature = "incremental")]
mod transfer;
#[cfg(feature = "incremental")]
pub(super) use pricing::RetainedCosts;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct Proof {
    #[cfg(feature = "incremental")]
    id: Option<storage::Id>,
}

#[cfg(feature = "incremental")]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EvidenceStats {
    pub certificates: usize,
    pub reads: usize,
    /// Logical retained payload, conservatively charging vector capacity.
    pub bytes: usize,
    pub unrecorded: usize,
    pub summaries: usize,
    pub summary_hits: u64,
    pub summary_index_bytes: usize,
    pub region_bytes: usize,
    pub region_normalizations: u64,
}
#[cfg(feature = "incremental")]
#[derive(Debug, Clone)]
pub enum DependencySource {
    Thunk(ThunkHandle),
    Call {
        function: ValueHandle,
        argument: ThunkHandle,
    },
    Introduced(ThunkHandle),
    Host {
        name: GlobalName,
        version: crate::HostVersion,
    },
    /// Code/read validity without per-computation handles: transferred history
    /// or a shared expression summary. This is not a computation/value identity.
    Historical,
}
#[cfg(feature = "incremental")]
#[derive(Debug, Clone)]
pub struct DependencyRead {
    pub source: DependencySource,
    pub expression: Option<ExprId>,
    /// None for proof-only history; no old result heap is retained for reporting.
    pub value: Option<ValueHandle>,
    pub cost: Option<cost::WorkCost>,
}

#[cfg(feature = "incremental")]
#[derive(Debug)]
pub(super) enum Recipe {
    Thunk(CellId, Pending),
    Call(ValueId, CellId),
    Introduced(CellId),
    Host(GlobalName, Arc<crate::host::HostIdentity>),
    Historical(Option<ExprId>),
    /// Code plus ordered reads; no result identity, activation or cost authority.
    Summary(SummaryCode),
}
#[cfg(feature = "incremental")]
pub(super) enum CodeValidity {
    Changed,
    CheckReads,
    /// A stronger exact static proof already covers code and all inputs.
    Proven,
}
#[cfg(feature = "incremental")]
struct Certificate {
    recipe: Recipe,
    result: Option<ValueId>,
    reads: Box<[Proof]>,
    cost: cost::Stored,
    bytes: usize,
}
#[cfg(feature = "incremental")]
impl storage::Trace for Certificate {
    fn edge_count(&self) -> usize {
        self.reads.len()
    }
    fn edge(&self, index: usize) -> storage::Id {
        self.reads[index].id.expect("complete read")
    }
}
#[derive(Default, Clone)]
pub(super) struct Store {
    #[cfg(feature = "incremental")]
    records: storage::Arena<Arc<Certificate>>,
    #[cfg(feature = "incremental")]
    stats: EvidenceStats,
    #[cfg(feature = "incremental")]
    active_records: usize,
    #[cfg(feature = "incremental")]
    active_reads: usize,
    #[cfg(feature = "incremental")]
    active_bytes: usize,
    #[cfg(feature = "incremental")]
    summary_index: Vec<Proof>,
    #[cfg(feature = "incremental")]
    regions: Option<Arc<regions::Regions>>,
    #[cfg(feature = "incremental")]
    regions_attempted: bool,
}
pub(super) struct Snapshot {
    #[cfg(feature = "incremental")]
    regions: Option<Arc<regions::Regions>>,
    #[cfg(feature = "incremental")]
    regions_attempted: bool,
    #[cfg(feature = "incremental")]
    summary_slots: usize,
    #[cfg(feature = "incremental")]
    stats: EvidenceStats,
}
#[cfg(feature = "incremental")]
struct Building {
    recipe: Recipe,
    reads: Vec<Proof>,
    complete: bool,
    bytes: usize,
}
#[derive(Default)]
pub(super) struct Recorder {
    #[cfg(feature = "incremental")]
    active: Vec<Building>,
    // Once the active-record budget is full, count nesting without allocating
    // an extra frame for every suppressed computation.
    #[cfg(feature = "incremental")]
    suppressed: usize,
}
#[cfg(feature = "incremental")]
const READ_BYTES: usize = 2 * std::mem::size_of::<Proof>();
#[cfg(feature = "incremental")]
const BASE_BYTES: usize = std::mem::size_of::<Certificate>() + 64;

#[cfg(feature = "incremental")]
fn charge(work: &mut u64, limits: &Limits) -> bool {
    if *work >= limits.steps {
        return false;
    }
    *work += 1;
    true
}

impl Store {
    /// A projected hit observes the current call/result while retaining the
    /// original computation as historical validity evidence. Missing evidence
    /// must remain unknown, never become a dependency-free certificate.
    #[cfg(feature = "incremental")]
    pub(super) fn projected_call(
        &mut self,
        function: ValueId,
        argument: CellId,
        result: ValueId,
        previous: Proof,
        limits: &Limits,
        work: &mut u64,
    ) -> Proof {
        let bytes = BASE_BYTES + READ_BYTES;
        if !charge(work, limits) || self.get(previous).is_none() || !self.room(1, bytes, limits) {
            self.stats.unrecorded = self.stats.unrecorded.saturating_add(1);
            return Proof::default();
        }
        self.insert(
            Recipe::Call(function, argument),
            result,
            vec![previous],
            cost::Stored::default(),
            bytes,
            limits,
        )
    }

    #[cfg(feature = "incremental")]
    pub(super) fn clone_work(&self) -> usize {
        self.records
            .clone_work()
            .saturating_add(self.summary_index.len())
    }

    pub fn abandon_pending(&mut self) {
        #[cfg(feature = "incremental")]
        {
            self.active_records = 0;
            self.active_reads = 0;
            self.active_bytes = 0;
        }
    }
    pub fn snapshot(&mut self) -> Snapshot {
        #[cfg(feature = "incremental")]
        self.records.checkpoint();
        Snapshot {
            #[cfg(feature = "incremental")]
            regions: self.regions.clone(),
            #[cfg(feature = "incremental")]
            regions_attempted: self.regions_attempted,
            #[cfg(feature = "incremental")]
            summary_slots: self.summary_index.len(),
            #[cfg(feature = "incremental")]
            stats: self.stats,
        }
    }
    pub fn commit(&mut self, _snapshot: Snapshot) {
        #[cfg(feature = "incremental")]
        self.records.commit();
    }
    pub fn rollback(&mut self, _snapshot: Snapshot) {
        #[cfg(feature = "incremental")]
        {
            self.records.rollback();
            self.stats = _snapshot.stats;
            self.regions = _snapshot.regions;
            self.regions_attempted = _snapshot.regions_attempted;
            // Discard an index allocated by the inspection. An older weak
            // index may retain stale candidate IDs: generation checks reject
            // them without a rollback scan or any retained runtime authority.
            if _snapshot.summary_slots == 0 {
                self.summary_index = Vec::new();
            }
        }
    }
    pub fn introduced(
        &mut self,
        _cell: CellId,
        _value: ValueId,
        _limits: &Limits,
        _work: &mut u64,
    ) -> Proof {
        #[cfg(feature = "incremental")]
        {
            if !charge(_work, _limits) {
                return Proof::default();
            }
            if _limits.compact_evidence {
                return self.insert_summary(SummaryCode::Immutable, Vec::new(), _limits, _work);
            }
            self.insert(
                Recipe::Introduced(_cell),
                _value,
                Vec::new(),
                Default::default(),
                BASE_BYTES,
                _limits,
            )
        }
        #[cfg(not(feature = "incremental"))]
        Proof::default()
    }
    #[cfg(feature = "incremental")]
    fn insert(
        &mut self,
        recipe: Recipe,
        result: ValueId,
        reads: Vec<Proof>,
        cost: cost::Stored,
        bytes: usize,
        limits: &Limits,
    ) -> Proof {
        self.insert_record(recipe, Some(result), reads, cost, bytes, limits)
    }
    #[cfg(feature = "incremental")]
    fn insert_record(
        &mut self,
        recipe: Recipe,
        result: Option<ValueId>,
        reads: Vec<Proof>,
        cost: cost::Stored,
        bytes: usize,
        limits: &Limits,
    ) -> Proof {
        if !self.room(reads.len(), bytes, limits) {
            self.stats.unrecorded = self.stats.unrecorded.saturating_add(1);
            return Proof::default();
        }
        let read_count = reads.len();
        let summary = matches!(recipe, Recipe::Summary(_));
        let record = Certificate {
            recipe,
            result,
            reads: reads.into_boxed_slice(),
            cost,
            bytes,
        };
        match self
            .records
            .insert(Arc::new(record), limits.dependency_records)
        {
            Ok(id) => {
                self.stats.summaries += usize::from(summary);
                self.stats.reads += read_count;
                self.stats.bytes += bytes;
                self.stats.certificates = self.records.len();
                Proof { id: Some(id) }
            }
            Err(_) => {
                self.stats.unrecorded = self.stats.unrecorded.saturating_add(1);
                Proof::default()
            }
        }
    }
    #[cfg(feature = "incremental")]
    fn room(&self, reads: usize, bytes: usize, limits: &Limits) -> bool {
        self.records.len().saturating_add(self.active_records) < limits.dependency_records
            && reads
                <= limits
                    .dependency_reads
                    .saturating_sub(self.stats.reads)
                    .saturating_sub(self.active_reads)
            && bytes
                <= limits
                    .dependency_bytes
                    .saturating_sub(self.stats.bytes)
                    .saturating_sub(self.active_bytes)
    }
    #[cfg(feature = "incremental")]
    fn get(&self, proof: Proof) -> Option<&Certificate> {
        self.records.get(proof.id?).map(AsRef::as_ref)
    }
}

impl Recorder {
    /// A successful host observation is a versioned input read even if its
    /// function/arguments were already WHNF. Unversioned reads are unknown.
    pub fn host(
        &mut self,
        _name: &GlobalName,
        _function: &crate::host::HostFunction,
        _value: ValueId,
        _store: &mut Store,
        _limits: &Limits,
        _work: &mut u64,
    ) {
        #[cfg(feature = "incremental")]
        {
            let proof = if let Some(identity) = _function
                .identity
                .as_ref()
                .filter(|_| charge(_work, _limits))
            {
                let bytes = _name
                    .module
                    .iter()
                    .chain(std::iter::once(&_name.name))
                    .map(String::len)
                    .fold(
                        BASE_BYTES.saturating_add(identity.bytes),
                        usize::saturating_add,
                    );
                if _store.room(0, bytes, _limits) {
                    _store.insert(
                        Recipe::Host(_name.clone(), identity.clone()),
                        _value,
                        Vec::new(),
                        Default::default(),
                        bytes,
                        _limits,
                    )
                } else {
                    _store.stats.unrecorded = _store.stats.unrecorded.saturating_add(1);
                    Proof::default()
                }
            } else {
                _store.stats.unrecorded = _store.stats.unrecorded.saturating_add(1);
                Proof::default()
            };
            self.hit(proof, _store, _limits, _work);
        }
    }
    pub fn begin_thunk(
        &mut self,
        _cell: CellId,
        _pending: &Pending,
        _store: &mut Store,
        _limits: &Limits,
        _work: &mut u64,
    ) {
        #[cfg(feature = "incremental")]
        {
            let payload = match _pending {
                Pending::TextList {
                    source, separator, ..
                } => source
                    .len()
                    .saturating_add(separator.as_ref().map_or(0, |s| s.len())),
                Pending::DecimalParts(n, _) => super::decimal_storage_bytes(n),
                _ => 0,
            };
            let bytes = BASE_BYTES.saturating_add(payload);
            if self.reserve(bytes, _store, _limits, _work) {
                let recipe = match _pending {
                    Pending::Expr(expr, _) if _limits.compact_evidence => Recipe::Summary(
                        _store.region_guard(SummaryCode::Expression(*expr), _limits),
                    ),
                    _ => Recipe::Thunk(_cell, _pending.clone()),
                };
                self.active.push(Building {
                    recipe,
                    reads: Vec::new(),
                    complete: true,
                    bytes,
                });
            }
        }
    }
    pub fn begin_call(
        &mut self,
        _function: ValueId,
        _argument: CellId,
        _code: (LocalId, ExprId),
        _store: &mut Store,
        _limits: &Limits,
        _work: &mut u64,
    ) {
        #[cfg(feature = "incremental")]
        if self.reserve(BASE_BYTES, _store, _limits, _work) {
            self.active.push(Building {
                recipe: if _limits.compact_evidence {
                    Recipe::Summary(
                        _store.region_guard(SummaryCode::Call(_code.0, _code.1), _limits),
                    )
                } else {
                    Recipe::Call(_function, _argument)
                },
                reads: Vec::new(),
                complete: true,
                bytes: BASE_BYTES,
            });
        }
    }
    #[cfg(feature = "incremental")]
    fn reserve(
        &mut self,
        bytes: usize,
        store: &mut Store,
        limits: &Limits,
        work: &mut u64,
    ) -> bool {
        let charged = charge(work, limits);
        if !charged
            || self.suppressed > 0
            || store.records.len().saturating_add(store.active_records) >= limits.dependency_records
            || bytes
                > limits
                    .dependency_bytes
                    .saturating_sub(store.stats.bytes)
                    .saturating_sub(store.active_bytes)
        {
            self.suppressed += 1;
            return false;
        }
        store.active_bytes += bytes;
        store.active_records += 1;
        true
    }
    pub fn hit(&mut self, _proof: Proof, _store: &mut Store, _limits: &Limits, _work: &mut u64) {
        #[cfg(feature = "incremental")]
        {
            let charged = charge(_work, _limits);
            if self.suppressed > 0 {
                return;
            }
            let Some(parent) = self.active.last_mut() else {
                return;
            };
            if !parent.complete {
                return;
            }
            if !charged
                || _store.get(_proof).is_none()
                || _store.stats.reads.saturating_add(_store.active_reads)
                    >= _limits.dependency_reads
                || READ_BYTES
                    > _limits
                        .dependency_bytes
                        .saturating_sub(_store.stats.bytes)
                        .saturating_sub(_store.active_bytes)
            {
                parent.complete = false;
                _store.active_reads -= parent.reads.len();
                let bytes = parent.reads.len() * READ_BYTES;
                _store.active_bytes -= bytes;
                parent.bytes -= bytes;
                parent.reads = Vec::new();
                return;
            }
            parent.reads.push(_proof);
            _store.active_reads += 1;
            _store.active_bytes += READ_BYTES;
            parent.bytes += READ_BYTES;
        }
    }
    pub fn finish(
        &mut self,
        _value: ValueId,
        _cost: cost::Stored,
        _store: &mut Store,
        _limits: &Limits,
        _work: &mut u64,
    ) -> Proof {
        #[cfg(feature = "incremental")]
        {
            let charged = charge(_work, _limits);
            let proof = if self.suppressed > 0 {
                self.suppressed -= 1;
                _store.stats.unrecorded = _store.stats.unrecorded.saturating_add(1);
                Proof::default()
            } else {
                let current = self.active.pop().expect("balanced computation boundaries");
                _store.active_records -= 1;
                _store.active_reads -= current.reads.len();
                _store.active_bytes -= current.bytes;
                if charged && current.complete {
                    if let Recipe::Summary(expr) = current.recipe {
                        _store.insert_summary(expr, current.reads, _limits, _work)
                    } else {
                        _store.insert(
                            current.recipe,
                            _value,
                            current.reads,
                            _cost,
                            current.bytes,
                            _limits,
                        )
                    }
                } else {
                    _store.stats.unrecorded = _store.stats.unrecorded.saturating_add(1);
                    Proof::default()
                }
            };
            self.hit(proof, _store, _limits, _work);
            proof
        }
        #[cfg(not(feature = "incremental"))]
        Proof::default()
    }
}

#[cfg(all(test, feature = "incremental"))]
mod tests {
    use super::*;

    fn name(n: &str) -> GlobalName {
        GlobalName {
            module: vec!["Test".into()],
            name: n.into(),
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
    fn proof(s: &Session, n: &str) -> Proof {
        let cell = s.cell_id(s.globals[&Global::Value(name(n))]);
        let ThunkState::Done(Ok(done)) = s.thunks[cell.0].state else {
            panic!("completed root")
        };
        done.proof
    }
    fn reaches(store: &Store, root: Proof, target: Proof) -> bool {
        let mut todo = vec![root];
        let mut seen = std::collections::HashSet::new();
        while let Some(p) = todo.pop() {
            if p == target {
                return true;
            }
            if let Some(id) = p.id {
                if seen.insert(id) {
                    todo.extend(store.get(p).unwrap().reads.iter().copied());
                }
            }
        }
        false
    }
    #[test]
    fn thunk_hits_and_completed_call_hits_remain_actual_dependencies() {
        let mut s = session("shared :: Int\nshared = 7\nf :: Int -> Int\nf x = shared + x\ninput :: Int\ninput = 3\na = f input\nb = f input\n", Limits::default());
        s.evaluate(&name("a")).unwrap();
        let function = s.evaluate(&name("f")).unwrap().value;
        let argument = s.cell_id(s.globals[&Global::Value(name("input"))]);
        let key = s.call_key(function, argument);
        let (_, _, _, original_call, _) = s.memo.get(&key).unwrap();
        assert!(original_call.id.is_some());
        let calls = s
            .evidence
            .records
            .iter()
            .filter(|r| matches!(r.recipe, Recipe::Call(f, a) if f == function && a == argument))
            .count();
        let hits = s.stats.application_cache_hits;
        s.evaluate(&name("b")).unwrap();
        assert!(s.stats.application_cache_hits > hits);
        assert_eq!(
            calls,
            s.evidence
                .records
                .iter()
                .filter(
                    |r| matches!(r.recipe, Recipe::Call(f, a) if f == function && a == argument)
                )
                .count()
        );
        assert!(reaches(&s.evidence, proof(&s, "b"), original_call));
        assert!(reaches(&s.evidence, proof(&s, "b"), proof(&s, "shared")));
        let root = s.global_handle(&name("b")).unwrap();
        assert!(!s
            .dependency_reads(root, 0, 100)
            .unwrap()
            .unwrap()
            .is_empty());
        assert!(s
            .dependency_reads(root, usize::MAX, 100)
            .unwrap()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn branches_and_undemanded_fields_are_not_forced_to_record_evidence() {
        let mut s = session("bad :: Int\nbad = error \"unforced\"\ncondition = True\nchosen :: Int\nchosen = if condition then 7 else bad\nresult = { visible = chosen, later = bad }\n", Limits::default());
        let root = s.evaluate(&name("result")).unwrap();
        assert!(proof(&s, "result").id.is_some());
        let visible = s
            .children(root, 0, 2)
            .unwrap()
            .into_iter()
            .find(|c| c.field == Field::Named("visible".into()))
            .unwrap();
        s.force(visible.handle).unwrap();
        assert!(reaches(
            &s.evidence,
            proof(&s, "chosen"),
            proof(&s, "condition")
        ));
        let bad = s.globals[&Global::Value(name("bad"))];
        assert!(matches!(s.thunks[bad.0].state, ThunkState::Pending(_)));
        assert!(s
            .evidence
            .records
            .iter()
            .all(|r| !matches!(r.recipe, Recipe::Thunk(cell, _) if cell == s.cell_id(bad))));
    }
    #[test]
    fn exhaustion_produces_unknown_evidence_and_reserves_active_storage() {
        let mut store = Store::default();
        let mut recorder = Recorder::default();
        let mut work = 0;
        let mut limits = Limits {
            dependency_records: 1,
            dependency_bytes: BASE_BYTES,
            ..Default::default()
        };
        recorder.begin_call(
            0,
            CellId(0),
            (LocalId(0), ExprId(0)),
            &mut store,
            &limits,
            &mut work,
        );
        let child = store.introduced(CellId(1), 1, &limits, &mut work);
        assert!(child.id.is_none(), "active storage is reserved too");
        recorder.hit(child, &mut store, &limits, &mut work);
        assert!(recorder
            .finish(0, Default::default(), &mut store, &limits, &mut work)
            .id
            .is_none());
        assert_eq!(store.stats.certificates, 0);
        assert_eq!(store.active_bytes, 0);
        limits.dependency_records = 10;
        limits.dependency_bytes = 100 * BASE_BYTES;
        limits.dependency_reads = 0;
        let child = store.introduced(CellId(1), 1, &limits, &mut work);
        recorder.begin_call(
            0,
            CellId(0),
            (LocalId(0), ExprId(0)),
            &mut store,
            &limits,
            &mut work,
        );
        recorder.hit(child, &mut store, &limits, &mut work);
        assert!(recorder
            .finish(0, Default::default(), &mut store, &limits, &mut work)
            .id
            .is_none());
        assert_eq!(
            store.stats.certificates, 1,
            "never certify a missing read as an empty set"
        );
        assert_eq!(store.stats.reads, 0);
    }
    #[test]
    fn bounded_inspection_rolls_back_certificates_and_language_errors_drop_builders() {
        let mut s = session("f :: Int -> Int\nf n = if n == 0 then 0 else 1 + f (n - 1)\nanswer = f 100\nbad :: Int\nbad = error \"failed\"\ngood :: Int\ngood = 7\n", Limits::default());
        s.evaluate(&name("good")).unwrap();
        let old_proof = proof(&s, "good");
        let before = s.evidence_stats();
        let handle = s.global_handle(&name("answer")).unwrap();
        assert_eq!(
            s.inspect(
                handle,
                InspectionBudget {
                    steps: 200,
                    ..Default::default()
                }
            )
            .unwrap(),
            Inspection::Truncated
        );
        assert_eq!(s.evidence_stats(), before);
        assert!(s.evidence.get(old_proof).is_some());
        assert_eq!(s.evidence.active_records, 0);
        assert!(s.evaluate(&name("bad")).is_err());
        assert_eq!(s.evidence.active_bytes, 0);
        s.evaluate(&name("answer")).unwrap();
        assert!(proof(&s, "answer").id.is_some());
    }
    #[test]
    fn disabling_recording_changes_no_values_and_does_not_create_empty_certificates() {
        let mut s = session(
            "answer :: Int\nanswer = 1 + 2\n",
            Limits {
                dependency_records: 0,
                ..Default::default()
            },
        );
        let value = s.evaluate(&name("answer")).unwrap();
        assert_eq!(s.view(value).unwrap(), ValueView::Literal(&Literal::Int(3)));
        assert_eq!(s.evidence_stats().certificates, 0);
        assert!(s.evidence_stats().unrecorded > 0);
        let root = s.global_handle(&name("answer")).unwrap();
        assert!(s.dependency_reads(root, 0, 100).unwrap().is_none());
    }

    #[test]
    fn collecting_evidence_preserves_values_and_recycled_proofs_stay_expired() {
        let mut s = session(
            "a :: Int\na = 1 + 2\nb :: Int\nb = 3 + 4\nc = a + b\n",
            Limits {
                application_cache_entries: 0,
                ..Default::default()
            },
        );
        s.evaluate(&name("a")).unwrap();
        let a = s.global_handle(&name("a")).unwrap();
        let old = proof(&s, "a");
        let report = s.collect_dependency_evidence(&[], 100_000).unwrap();
        assert_eq!(report.status, EvidenceCollectionStatus::Collected);
        assert!(report.reclaimed > 0);
        assert_eq!(s.evidence_stats().certificates, 0);
        let value = s.force(a).unwrap();
        assert_eq!(s.view(value).unwrap(), ValueView::Literal(&Literal::Int(3)));
        s.evaluate(&name("b")).unwrap();
        assert!(s.evidence.get(old).is_none());
        assert!(s.dependency_reads(a, 0, 100).unwrap().is_none());
        let value = s.evaluate(&name("c")).unwrap();
        assert_eq!(
            s.view(value).unwrap(),
            ValueView::Literal(&Literal::Int(10))
        );
        assert!(
            proof(&s, "c").id.is_none(),
            "missing child evidence cannot certify the new parent"
        );
    }

    #[test]
    fn explicit_roots_keep_transitive_proofs_and_cancel_does_not_collect() {
        let mut s = session(
            "a :: Int\na = 1 + 2\nb :: Int\nb = 3 + 4\n",
            Limits {
                application_cache_entries: 0,
                ..Default::default()
            },
        );
        s.evaluate(&name("a")).unwrap();
        s.evaluate(&name("b")).unwrap();
        let a = s.global_handle(&name("a")).unwrap();
        let before = s.evidence_stats();
        s.limits.cancellation.store(true, Ordering::Relaxed);
        assert_eq!(
            s.collect_dependency_evidence(&[a], 100_000).unwrap().status,
            EvidenceCollectionStatus::Cancelled
        );
        assert_eq!(s.evidence_stats(), before);
        s.limits.cancellation.store(false, Ordering::Relaxed);
        let report = s.collect_dependency_evidence(&[a], 100_000).unwrap();
        assert_eq!(report.status, EvidenceCollectionStatus::Collected);
        assert!(report.reclaimed > 0);
        assert!(s.dependency_reads(a, 0, 100).unwrap().is_some());
        assert!(s.evidence.get(proof(&s, "b")).is_none());
    }
}

#[cfg(feature = "incremental")]
impl Session {
    pub fn evidence_stats(&self) -> EvidenceStats {
        self.evidence.stats
    }

    /// Collect optional dependency metadata. Values remain valid and are not
    /// collected here. Retained call certificates and the supplied completed
    /// roots keep their transitive proofs; other thunk proof references are weak.
    pub fn collect_dependency_evidence(
        &mut self,
        roots: &[ThunkHandle],
        work: u64,
    ) -> Result<EvidenceCollection, Outcome> {
        self.cache_work(|session| session.collect_dependency_evidence_impl(roots, work))
    }
    fn collect_dependency_evidence_impl(
        &mut self,
        roots: &[ThunkHandle],
        work: u64,
    ) -> Result<EvidenceCollection, Outcome> {
        if self.abandoned {
            return Err(Outcome::Abandoned);
        }
        let allowance = work.min(self.limits.steps.saturating_sub(self.stats.steps));
        let mut left = allowance;
        let mut selected = std::collections::HashSet::new();
        let mut status = EvidenceCollectionStatus::Collected;
        for root in roots {
            if self.limits.cancellation.load(Ordering::Relaxed) {
                status = EvidenceCollectionStatus::Cancelled;
                break;
            }
            if left == 0 {
                status = EvidenceCollectionStatus::Truncated;
                break;
            }
            left -= 1;
            if let Err(error) = self.validate_thunk(*root) {
                self.stats.steps += allowance - left;
                return Err(error);
            }
            let cell = self.cell_id(ThunkId(root.thunk));
            if let ThunkState::Done(Ok(done)) = self.thunks[cell.0].state {
                if self.evidence.get(done.proof).is_some() {
                    selected.insert(done.proof.id.unwrap());
                }
            }
        }
        if status == EvidenceCollectionStatus::Collected {
            for proof in self.memo.proofs() {
                if self.limits.cancellation.load(Ordering::Relaxed) {
                    status = EvidenceCollectionStatus::Cancelled;
                    break;
                }
                if left == 0 {
                    status = EvidenceCollectionStatus::Truncated;
                    break;
                }
                left -= 1;
                if self.evidence.get(proof).is_some() {
                    selected.insert(proof.id.unwrap());
                }
            }
        }
        let prepared = allowance - left;
        let mut result = EvidenceCollection {
            status,
            work: 0,
            reclaimed: 0,
            remaining: self.evidence.records.len(),
        };
        if status == EvidenceCollectionStatus::Collected {
            let stats = &mut self.evidence.stats;
            result = self.evidence.records.collect(
                selected,
                left,
                &self.limits.cancellation,
                |record| {
                    stats.reads -= record.reads.len();
                    stats.bytes -= record.bytes;
                    stats.certificates -= 1;
                    stats.summaries -= usize::from(matches!(record.recipe, Recipe::Summary(_)));
                },
            );
        }
        result.work += prepared;
        self.stats.steps += result.work;
        if self.stats.steps >= self.limits.steps {
            self.abandoned = true;
            return Err(Outcome::BudgetExhausted);
        }
        Ok(result)
    }

    /// Read a bounded page of direct dependencies without evaluating anything.
    /// None means no complete certificate, not an empty dependency set. These
    /// handles belong to this Session; no certificate ID escapes or transfers.
    pub fn dependency_reads(
        &self,
        handle: ThunkHandle,
        offset: usize,
        count: usize,
    ) -> Result<Option<Vec<DependencyRead>>, Outcome> {
        self.validate_thunk(handle)?;
        let cell = self.cell_id(ThunkId(handle.thunk));
        let ThunkState::Done(Ok(done)) = self.thunks[cell.0].state else {
            return Ok(None);
        };
        let Some(certificate) = self.evidence.get(done.proof) else {
            return Ok(None);
        };
        Ok(Some(
            certificate
                .reads
                .iter()
                .skip(offset)
                .take(count)
                .map(|proof| {
                    let read = self
                        .evidence
                        .get(*proof)
                        .expect("complete child certificate");
                    let (source, expression) = match &read.recipe {
                        Recipe::Thunk(cell, pending) => (
                            DependencySource::Thunk(self.thunk_handle(cell.owner())),
                            match pending {
                                Pending::Expr(expr, _) => Some(*expr),
                                _ => None,
                            },
                        ),
                        Recipe::Call(function, argument) => (
                            DependencySource::Call {
                                function: ValueHandle {
                                    session: self.id,
                                    value: *function,
                                },
                                argument: self.thunk_handle(argument.owner()),
                            },
                            None,
                        ),
                        Recipe::Host(name, identity) => (
                            DependencySource::Host {
                                name: name.clone(),
                                version: identity.version.clone(),
                            },
                            None,
                        ),
                        Recipe::Introduced(cell) => (
                            DependencySource::Introduced(self.thunk_handle(cell.owner())),
                            None,
                        ),
                        Recipe::Historical(expression) => {
                            (DependencySource::Historical, *expression)
                        }
                        Recipe::Summary(code) => (DependencySource::Historical, code.expression()),
                    };
                    DependencyRead {
                        source,
                        expression,
                        value: read.result.map(|value| ValueHandle {
                            session: self.id,
                            value,
                        }),
                        cost: read.cost.sample(),
                    }
                })
                .collect(),
        ))
    }
}

#[cfg(feature = "incremental")]
impl Store {
    pub(super) fn recovery(&self, proof: Proof, cell: CellId) -> Option<&Pending> {
        match &self.records.get(proof.id?)?.recipe {
            Recipe::Thunk(owner, pending) if *owner == cell => Some(pending),
            _ => None,
        }
    }

    /// Ordered DFS with bounded completed-proof memoization. Never evaluates an
    /// old read. A changed/unknown read stops validation before later dependencies.
    pub(super) fn validate(
        &self,
        proof: Proof,
        hosts: &HostRegistry,
        verdicts: &mut validation::Verdicts,
        limits: &Limits,
        work: &mut u64,
    ) -> Result<bool, Outcome> {
        self.validate_with_code(proof, hosts, verdicts, limits, work, |_| {
            CodeValidity::CheckReads
        })
    }

    /// Code identity and demanded-read identity are independent requirements.
    /// The caller's verdict table must belong to this exact target code/world.
    pub(super) fn validate_with_code(
        &self,
        proof: Proof,
        hosts: &HostRegistry,
        verdicts: &mut validation::Verdicts,
        limits: &Limits,
        work: &mut u64,
        code_matches: impl Fn(&Recipe) -> CodeValidity,
    ) -> Result<bool, Outcome> {
        let Some(root) = proof.id else {
            return Ok(false);
        };
        let depth_limit = limits.dependency_records.min(limits.dependency_bytes / 128);
        if depth_limit == 0 {
            return Ok(false);
        }
        let mut stack = vec![(root, 0usize)];
        let mut visiting = std::collections::HashSet::new();
        while let Some((id, next)) = stack.last_mut() {
            if limits.cancellation.load(Ordering::Relaxed) {
                return Err(Outcome::Cancelled);
            }
            if !charge(work, limits) {
                return Err(Outcome::BudgetExhausted);
            }
            if let Some(valid) = verdicts.lookup(*id) {
                if !valid {
                    return reject_path(&stack, verdicts, limits, work);
                }
                visiting.remove(id);
                stack.pop();
                continue;
            }
            let Some(record) = self.records.get(*id) else {
                return reject_path(&stack, verdicts, limits, work);
            };
            if *next == 0 {
                if !visiting.insert(*id) {
                    return Ok(false);
                }
                match code_matches(&record.recipe) {
                    CodeValidity::Changed => return reject_path(&stack, verdicts, limits, work),
                    CodeValidity::CheckReads => {}
                    CodeValidity::Proven => {
                        verdicts.remember(*id, true, limits);
                        visiting.remove(id);
                        stack.pop();
                        continue;
                    }
                }
                if let Recipe::Host(name, identity) = &record.recipe {
                    let bytes = identity.bytes as u64;
                    if bytes > limits.steps.saturating_sub(*work) {
                        return Err(Outcome::BudgetExhausted);
                    }
                    *work += bytes;
                    if hosts.function(name).and_then(|f| f.identity.as_ref()) != Some(identity) {
                        return reject_path(&stack, verdicts, limits, work);
                    }
                }
            }
            if let Some(read) = record.reads.get(*next) {
                *next += 1;
                let Some(child) = read.id else {
                    return Ok(false);
                };
                if stack.len() >= depth_limit {
                    return Ok(false);
                }
                stack.push((child, 0));
            } else {
                // This separate bounded table is proof authority only for this
                // immutable world. Generations prevent reuse after evidence GC.
                verdicts.remember(*id, true, limits);
                visiting.remove(id);
                stack.pop();
            }
        }
        Ok(true)
    }
}

#[cfg(feature = "incremental")]
fn reject_path(
    stack: &[(storage::Id, usize)],
    verdicts: &mut validation::Verdicts,
    limits: &Limits,
    work: &mut u64,
) -> Result<bool, Outcome> {
    verdicts.stats.rejected_paths += 1;
    // Every ancestor was waiting on this failed/unknown read. Record those
    // decisions once; never visit any later reads in the old execution path.
    for (id, _) in stack.iter().rev() {
        if limits.cancellation.load(Ordering::Relaxed) {
            return Err(Outcome::Cancelled);
        }
        if !charge(work, limits) {
            return Err(Outcome::BudgetExhausted);
        }
        verdicts.remember(*id, false, limits);
    }
    Ok(false)
}
