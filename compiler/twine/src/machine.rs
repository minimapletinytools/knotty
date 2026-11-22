#[cfg(feature = "incremental")]
mod admission;
mod bindings;
mod call_args;
use call_args::CallArgs;
mod closure;
mod cost;
#[cfg(feature = "incremental")]
mod fork;
#[cfg(feature = "incremental")]
mod heap;
mod index_hash;
#[cfg(feature = "incremental")]
pub use fork::{ForkAttempt, SessionFork, SnapshotInheritance};
#[cfg(feature = "incremental")]
use heap::Heap;
#[cfg(not(feature = "incremental"))]
type Heap<T> = Vec<T>;

mod evidence;
mod validation;
#[cfg(feature = "incremental")]
pub use validation::{InputForkAttempt, ValidationStats};
#[cfg(feature = "incremental")]
mod revisions;
#[cfg(feature = "incremental")]
mod source_code;
#[cfg(feature = "incremental")]
pub use source_code::{SourceForkAttempt, SourceForkLimits};
#[cfg(feature = "incremental")]
mod storage;
#[cfg(feature = "incremental")]
pub use revisions::{
    Revision, RevisionCollection, RevisionCollectionStatus, RevisionContext, RevisionError,
    RevisionForkAttempt, RevisionLimits, RevisionPreparation, RevisionReuse, RevisionStore,
};

#[cfg(feature = "incremental")]
pub use evidence::{DependencyRead, DependencySource, EvidencePressure, EvidenceStats};
#[cfg(feature = "incremental")]
pub use storage::{Collection as EvidenceCollection, CollectionStatus as EvidenceCollectionStatus};
#[cfg(feature = "incremental")]
mod identity;
#[cfg(feature = "incremental")]
mod identity_scope;
#[cfg(feature = "incremental")]
mod identity_support;
#[cfg(feature = "cost-accounting")]
pub use cost::WorkCost;
mod extension;
mod memo;
#[cfg(feature = "incremental")]
pub use memo::CacheIndexStats;
mod reuse;
use bindings::Bindings;
pub use extension::ExtensionError;
#[cfg(feature = "incremental")]
pub use reuse::{CacheRetention, CollectionBudget, RetainedRoot};
pub use reuse::{ReuseLimits, ReuseStats};
pub(crate) mod constructors;
pub mod dependencies;
mod inspection;
mod observed;
mod primitive_arguments;
use crate::{
    numeric, primitive, text, HostContext, HostError, HostLinkError, HostRegistry, HostValue,
    IoHandler, NativeValue,
};
use constructors::PRELUDE as ctor;
use dependencies::{DemandStatus, Tracking};
pub use inspection::*;
use knot_core::{
    DecimalLiteral, ExprId, Field, Global, GlobalName, Kind, Literal, LocalId, Origin, Pattern,
    Program,
};
use num_bigint::BigInt;
pub use observed::ObservedDataKey;
pub(crate) use primitive_arguments::PrimitiveArguments;
use std::collections::{BTreeMap, HashMap};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivationId(pub u64);
/// Session-local environment activation, with its source and lexical parent.
#[derive(Debug, Clone, Copy)]
pub struct Activation {
    pub id: ActivationId,
    pub parent: Option<ActivationId>,
    pub origin: Option<Origin>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueHandle {
    session: u64,
    value: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeErrorKind {
    Error(String),
    Loop,
    NonExhaustive,
    DivideByZero,
    InvalidNumeric(String),
    Host(String),
    Unsupported(String),
    InvalidCore(String),
    Io(String),
    EndOfInput,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError {
    pub kind: RuntimeErrorKind,
    pub origin: Origin,
    pub trace: Vec<Origin>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Runtime(RuntimeError),
    BudgetExhausted,
    Cancelled,
    Abandoned,
    UnknownGlobal(GlobalName),
    ForeignValue,
}
#[derive(Debug, Clone)]
pub struct Limits {
    pub steps: u64,
    pub heap_entries: usize,
    /// Combined call/expression/capture index slots; zero disables all indexes.
    pub application_cache_entries: usize,
    /// Minimum measured useful work for new optional call/expression entries.
    /// Zero preserves the admission control. This never disables thunk memoization
    /// or dependency recording; existing retained entries remain valid.
    #[cfg(feature = "incremental")]
    pub cache_minimum_work: u64,
    /// Optional cost/recency/index-byte eviction. Zero retains FIFO; positive
    /// samples inspect at most 16 oldest entries when online admission is full.
    #[cfg(feature = "incremental")]
    pub cache_eviction_samples: usize,
    /// Optional production dependency evidence, separate from diagnostic tracing.
    #[cfg(feature = "incremental")]
    pub dependency_records: usize,
    #[cfg(feature = "incremental")]
    pub dependency_reads: usize,
    #[cfg(feature = "incremental")]
    pub dependency_bytes: usize,
    /// Experimental sharing of code/read validity, not values or costs.
    #[cfg(feature = "incremental")]
    pub compact_evidence: bool,
    /// Experimental lexical code guards and bounded internal-proof reduction.
    #[cfg(feature = "incremental")]
    pub region_evidence: bool,
    /// Experimental ranking by marginal graph, proof and source-context storage.
    #[cfg(feature = "incremental")]
    pub joint_retention: bool,
    /// Experimental bounded evidence reclamation during ordinary evaluation.
    /// None disables it. Active/recovery proofs are protected; optional results
    /// can be selected together with their complete dependency closures.
    #[cfg(feature = "incremental")]
    pub evidence_pressure: Option<EvidencePressure>,
    pub primitive_bytes: usize,
    /// Maximum absolute decimal exponent expanded into exact Integer fields.
    pub decimal_power: u32,
    /// Cumulative host-reported native storage retained/allocated in this session.
    pub host_bytes: usize,
    pub cancellation: Arc<AtomicBool>,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            steps: 50_000_000,
            heap_entries: 2_000_000,
            application_cache_entries: 16_384,
            #[cfg(feature = "incremental")]
            cache_minimum_work: 0,
            #[cfg(feature = "incremental")]
            cache_eviction_samples: 0,
            #[cfg(feature = "incremental")]
            dependency_records: 65_536,
            #[cfg(feature = "incremental")]
            dependency_reads: 262_144,
            #[cfg(feature = "incremental")]
            dependency_bytes: 16 * 1024 * 1024,
            #[cfg(feature = "incremental")]
            compact_evidence: false,
            #[cfg(feature = "incremental")]
            region_evidence: false,
            #[cfg(feature = "incremental")]
            joint_retention: false,
            #[cfg(feature = "incremental")]
            evidence_pressure: None,
            primitive_bytes: 16 * 1024 * 1024,
            decimal_power: 4096,
            host_bytes: 256 * 1024 * 1024,
            cancellation: Arc::new(AtomicBool::new(false)),
        }
    }
}
#[derive(Debug, Default, Clone, Copy)]
pub struct Stats {
    /// Total charged execution and maintenance work.
    pub steps: u64,
    /// Subset of steps spent on cache metadata, validation and transfer.
    #[cfg(feature = "incremental")]
    pub maintenance_steps: u64,
    #[cfg(feature = "incremental")]
    pub retention_candidates: u64,
    #[cfg(feature = "incremental")]
    pub evidence_pressure_attempts: u64,
    #[cfg(feature = "incremental")]
    pub evidence_pressure_work: u64,
    #[cfg(feature = "incremental")]
    pub evidence_pressure_collections: u64,
    #[cfg(feature = "incremental")]
    pub evidence_pressure_prepared: u64,
    #[cfg(feature = "incremental")]
    pub evidence_pressure_reclaimed: u64,
    #[cfg(feature = "incremental")]
    pub evidence_pressure_declined: u64,
    #[cfg(feature = "incremental")]
    /// Peak complete mandatory trace (including attempts whose later sweep declined).
    pub evidence_pressure_mandatory_records: u64,
    #[cfg(feature = "incremental")]
    /// Peak proof count retained by a successful selecting sweep.
    pub evidence_pressure_selected_records: u64,
    #[cfg(feature = "incremental")]
    pub evidence_pressure_selected_roots: u64,
    #[cfg(feature = "incremental")]
    pub evidence_pressure_evicted_indexes: u64,
    #[cfg(feature = "incremental")]
    pub retention_admitted: u64,
    #[cfg(feature = "incremental")]
    pub retention_unknown_costs: u64,
    #[cfg(feature = "incremental")]
    pub retention_storage_queries: u64,
    #[cfg(feature = "incremental")]
    pub retention_unknown_storage: u64,
    #[cfg(feature = "incremental")]
    pub retention_storage_steps: u64,
    /// Graph/proof portions of retention_storage_steps, not wall-clock time.
    #[cfg(feature = "incremental")]
    pub retention_graph_storage_steps: u64,
    #[cfg(feature = "incremental")]
    pub retention_proof_storage_steps: u64,
    #[cfg(feature = "incremental")]
    pub retention_unknown_graph_storage: u64,
    #[cfg(feature = "incremental")]
    pub retention_unknown_proof_storage: u64,
    #[cfg(feature = "incremental")]
    pub retention_graph_description_hits: u64,
    #[cfg(feature = "incremental")]
    pub retention_graph_description_misses: u64,
    #[cfg(feature = "incremental")]
    pub retention_graph_description_evictions: u64,
    #[cfg(feature = "incremental")]
    pub retention_graph_description_bytes: usize,
    #[cfg(feature = "incremental")]
    pub retention_index_rejections: u64,
    #[cfg(feature = "incremental")]
    pub retention_steps: u64,
    #[cfg(feature = "incremental")]
    pub cache_evictions: u64,
    #[cfg(feature = "incremental")]
    pub cache_admission_rejections: u64,
    #[cfg(feature = "incremental")]
    pub cache_eviction_work: u64,
    /// Subset of inspection_steps spent on the same maintenance.
    #[cfg(feature = "incremental")]
    pub inspection_maintenance_steps: u64,
    pub thunk_evaluations: u64,
    pub memo_hits: u64,
    pub application_cache_hits: u64,
    #[cfg(feature = "incremental")]
    pub application_key_misses: u64,
    #[cfg(feature = "incremental")]
    pub application_key_skips: u64,
    #[cfg(feature = "incremental")]
    pub application_scope_misses: u64,
    #[cfg(feature = "incremental")]
    pub application_projected_hits: u64,
    #[cfg(feature = "incremental")]
    pub application_validity_misses: u64,
    pub expression_cache_hits: u64,
    pub captured_cache_hits: u64,
    pub activations: u64,
    pub primitive_calls: u64,
    pub host_calls: u64,
    pub inspection_steps: u64,
    pub host_bytes: usize,
}
/// WHNF only. This view neither traverses fields nor performs IO.
#[derive(Debug, PartialEq)]
pub enum ValueView<'a> {
    Literal(&'a Literal),
    Native(&'a GlobalName),
    Constructor(&'a GlobalName, usize),
    Tuple(usize),
    Record(Vec<&'a str>),
    Function,
    Dictionary,
    Io,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ThunkId(usize);
/// Identity of a computation owner, independent of the source use forcing it.
/// Owners live inline in the first use's arena slot; shared uses point directly
/// to that slot and never form alias chains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct CellId(usize);
impl CellId {
    fn owner(self) -> ThunkId {
        ThunkId(self.0)
    }
}
type EnvId = usize;
type ValueId = usize;
#[derive(Debug, Clone)]
pub(crate) enum Callable {
    Primitive(Arc<str>),
    Host(GlobalName, usize),
    Constructor(Arc<GlobalName>, usize),
}
#[derive(Debug, Clone)]
pub(crate) enum IoAction {
    Pure(ThunkId),
    Bind(ThunkId, ThunkId),
    PutLine(ThunkId),
    GetLine,
}
#[derive(Debug, Clone)]
pub(crate) enum Value {
    Lit(Literal),
    Native(NativeValue),
    Ctor(Arc<GlobalName>, Vec<ThunkId>),
    // An ordinary Ratio shell plus compact decimal provenance for direct Float
    // rounding. Matching/viewing uses its lazy fields just like Ctor.
    DecimalRatio(Arc<GlobalName>, Arc<DecimalLiteral>, Vec<ThunkId>),
    Tuple(Vec<ThunkId>),
    Record(BTreeMap<String, ThunkId>),
    Closure(LocalId, ExprId, closure::Capture),
    Partial(Callable, Vec<ThunkId>),
    Dict(BTreeMap<Field, ThunkId>),
    Io(IoAction, Origin),
}
impl Value {
    /// The existing projection path can return these immutable values directly:
    /// they have no Knot fields or lexical edges requiring a call-body witness.
    /// Other kinds retain the conservative source-scope contract.
    #[cfg(feature = "incremental")]
    fn needs_call_scope(&self) -> bool {
        !matches!(self, Self::Lit(_) | Self::Native(_))
    }
}

#[derive(Debug, Clone)]
enum Pending {
    Expr(ExprId, EnvId),
    Apply(ThunkId, ThunkId),
    DecimalParts(Arc<DecimalLiteral>, EnvId),
    Projection(ThunkId, usize),
    TextList {
        source: Arc<str>,
        separator: Option<Arc<str>>,
        offset: Option<usize>,
        activation: ActivationId,
    },
}
#[derive(Debug, Clone, Copy)]
struct Completed {
    value: ValueId,
    expression: Option<ExprId>,
    cost: cost::Stored,
    proof: evidence::Proof,
    stamp: validation::Stamp,
    #[cfg(feature = "incremental")]
    introduced: bool,
}
#[derive(Debug, Clone)]
enum ThunkState {
    Shared(CellId),
    Pending(Pending),
    Evaluating(Option<ExprId>),
    Done(Result<Completed, Arc<RuntimeError>>),
}
#[derive(Clone)]
struct Thunk {
    origin: Origin,
    activation: ActivationId,
    state: ThunkState,
}
#[derive(Clone)]
struct Env {
    // Empty lexical chains can use the same canonical cells as the root.
    empty: bool,
    parent: Option<EnvId>,
    bindings: Bindings,
    activation: ActivationId,
    origin: Option<Origin>,
}
#[derive(Clone)]
struct Matching<'code> {
    root: ThunkId,
    // Case arms live in immutable Core; retain their expression ID, not a copy.
    case: ExprId,
    arm: usize,
    // Borrow pattern code only for this machine run; never retain it in the heap.
    pending: Vec<(&'code Pattern, ThunkId)>,
    bindings: Bindings,
    env: EnvId,
    origin: Origin,
}
enum State {
    Eval(ExprId, EnvId),
    Force(ThunkId),
    Return(ValueId),
    Match,
}
enum RememberCall {
    Prepared(memo::Call),
    #[cfg(feature = "incremental")]
    Deferred(ValueId, CellId),
}
enum Frame<'code> {
    Update(CellId, cost::Mark),
    RememberApplication(Option<RememberCall>, cost::Mark, memo::Scope),
    Apply(CallArgs, Origin),
    Select(&'code Field, Origin),
    SelectIndex(usize, Origin),
    Match(&'code Pattern, ThunkId),
    PrimitiveMany {
        name: Arc<str>,
        args: CallArgs,
        values: Vec<ValueId>,
        origin: Origin,
    },
    Primitive {
        name: Arc<str>,
        args: CallArgs,
        first: Option<ValueId>,
        origin: Origin,
    },
    Host {
        name: GlobalName,
        args: Vec<ThunkId>,
        values: Vec<Value>,
        origin: Origin,
    },
    RecordUpdate(Vec<(&'code str, ThunkId)>, Origin),
    RatioNumerator(ThunkId, Origin),
    RatioDenominator(BigInt, Origin),
    ConcatHead(String, ThunkId, Origin),
    ConcatTail(String, Origin),
}
/// A session owns all recursive edges by integer ID. Drop it to release the
/// entire heap, including cycles. Control interruption abandons the session;
/// language errors are memoized and the session remains usable.
pub struct Session {
    program: Arc<Program>,
    hosts: Arc<HostRegistry>,
    id: u64,
    thunks: Heap<Thunk>,
    values: Heap<Value>,
    envs: Heap<Env>,
    globals: BTreeMap<Global, ThunkId>,
    memo: memo::Memo,
    evidence: evidence::Store,
    #[cfg(feature = "incremental")]
    evidence_pressure: evidence::pressure::Schedule,
    validation: validation::State,
    #[cfg(feature = "incremental")]
    identity_support: identity_support::Store,
    #[cfg(feature = "incremental")]
    source_code: Option<Arc<source_code::View>>,
    #[cfg(feature = "incremental")]
    source_scopes: Option<source_code::Scopes>,
    // Existing code is immutable; checked extensions only append. Resolve names once;
    // None preserves demand-time errors for unresolved hand-built core.
    global_refs: Heap<Option<ThunkId>>,
    limits: Limits,
    stats: Stats,
    abandoned: bool,
    inspection: Option<inspection::Checkpoint>,
    tracking: Option<Tracking>,
}
fn fresh_session_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .expect("Session ID space exhausted")
}
impl Session {
    fn current_source_origin(&self, origin: Origin) -> Option<Origin> {
        #[cfg(feature = "incremental")]
        if let Some(view) = &self.source_code {
            return view.origin(origin);
        }
        Some(origin)
    }
    fn source_scope_visible(&self, scope: EnvId) -> bool {
        #[cfg(feature = "incremental")]
        return self.source_scope_visibility(scope).0;
        #[cfg(not(feature = "incremental"))]
        {
            let _ = scope;
            true
        }
    }
    fn source_origin(&self, origin: Origin) -> Origin {
        self.current_source_origin(origin).unwrap_or(origin)
    }
    pub fn new(program: Arc<Program>, limits: Limits) -> Self {
        Self::create(program, limits, Arc::new(HostRegistry::default()))
    }
    /// Link all host references before evaluation, including references in
    /// unused branches. The immutable registry stays alive with the session.
    pub fn with_hosts(
        program: Arc<Program>,
        limits: Limits,
        hosts: Arc<HostRegistry>,
    ) -> Result<Self, HostLinkError> {
        for expr in &program.expressions {
            if let Kind::Host(name, signature) = &expr.kind {
                let f = hosts
                    .function(name)
                    .ok_or_else(|| HostLinkError::MissingFunction(name.clone()))?;
                if f.signature != *signature {
                    return Err(HostLinkError::SignatureMismatch(name.clone()));
                }
            }
        }
        Ok(Self::create(program, limits, hosts))
    }
    fn empty(program: Arc<Program>, limits: Limits, hosts: Arc<HostRegistry>) -> Self {
        #[cfg(feature = "compact-captures")]
        program.expressions.capture_layouts();
        let mut s = Self {
            program,
            hosts,
            id: fresh_session_id(),
            thunks: Default::default(),
            values: Default::default(),
            envs: Default::default(),
            globals: BTreeMap::new(),
            #[cfg(feature = "incremental")]
            source_code: None,
            #[cfg(feature = "incremental")]
            source_scopes: None,
            memo: Default::default(),
            evidence: Default::default(),
            #[cfg(feature = "incremental")]
            evidence_pressure: Default::default(),
            validation: Default::default(),
            #[cfg(feature = "incremental")]
            identity_support: identity_support::Store::new(
                limits.dependency_bytes,
                limits.application_cache_entries,
            ),
            global_refs: Default::default(),
            limits,
            stats: Stats::default(),
            abandoned: false,
            inspection: None,
            tracking: None,
        };
        s.env(None, Bindings::Empty, None);
        s
    }
    fn create(program: Arc<Program>, limits: Limits, hosts: Arc<HostRegistry>) -> Self {
        let mut s = Self::empty(program, limits, hosts);
        let env = 0;
        let globals: Vec<_> = s
            .program
            .globals
            .iter()
            .map(|(g, e)| (g.clone(), *e))
            .collect();
        for (g, e) in globals {
            let t = s.delay_expression(e, env);
            s.globals.insert(g, t);
        }
        s.global_refs = s
            .program
            .expressions
            .iter()
            .map(|expr| match &expr.kind {
                Kind::Global(g) => s.globals.get(g).copied(),
                _ => None,
            })
            .collect();
        s
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    /// Tighten cumulative work/allocation ceilings when this session shares an
    /// outer transaction budget with other checked operations. Never raises them.
    pub fn restrict_execution(&mut self, steps: u64, heap_entries: usize) {
        self.limits.steps = self.limits.steps.min(steps);
        self.limits.heap_entries = self.limits.heap_entries.min(heap_entries);
    }
    pub fn heap_entries(&self) -> usize {
        self.thunks.len() + self.values.len() + self.envs.len() + self.memo.len()
    }
    pub fn activations(&self) -> impl Iterator<Item = Activation> + '_ {
        self.envs
            .iter()
            .filter(|e| self.source_scope_visible(e.activation.0 as usize))
            .filter_map(|e| {
                Some(Activation {
                    id: e.activation,
                    parent: e.parent.map(|p| self.envs[p].activation),
                    origin: match e.origin {
                        Some(origin) => Some(self.current_source_origin(origin)?),
                        None => None,
                    },
                })
            })
    }
    /// Allocations include undemanded thunks. This is provenance, not a claim
    /// that a source node has been evaluated. Full inspection follows later.
    pub fn allocations(&self) -> impl Iterator<Item = (Origin, ActivationId)> + '_ {
        self.thunks
            .iter()
            .filter(|t| self.source_scope_visible(t.activation.0 as usize))
            .filter_map(|t| Some((self.current_source_origin(t.origin)?, t.activation)))
    }
    fn read_value(&self, handle: ValueHandle) -> Result<&Value, Outcome> {
        if self.abandoned {
            return Err(Outcome::Abandoned);
        }
        if handle.session != self.id {
            return Err(Outcome::ForeignValue);
        }
        Ok(&self.values[handle.value])
    }
    pub fn view(&self, handle: ValueHandle) -> Result<ValueView<'_>, Outcome> {
        Ok(match self.read_value(handle)? {
            Value::Lit(l) => ValueView::Literal(l),
            Value::Native(v) => ValueView::Native(v.type_name()),
            Value::Ctor(n, xs) | Value::DecimalRatio(n, _, xs) => {
                ValueView::Constructor(n, xs.len())
            }
            Value::Tuple(xs) => ValueView::Tuple(xs.len()),
            Value::Record(fs) => ValueView::Record(fs.keys().map(String::as_str).collect()),
            Value::Closure(..) | Value::Partial(..) => ValueView::Function,
            Value::Dict(_) => ValueView::Dictionary,
            Value::Io(..) => ValueView::Io,
        })
    }
    /// Read-only access for an embedding renderer. Does not force or run code.
    /// Cloning NativeValue retains its snapshot beyond this session's lifetime.
    pub fn native_value(&self, handle: ValueHandle) -> Result<Option<&NativeValue>, Outcome> {
        Ok(match self.read_value(handle)? {
            Value::Native(v) => Some(v),
            _ => None,
        })
    }
    pub fn evaluate(&mut self, name: &GlobalName) -> Result<ValueHandle, Outcome> {
        self.tick()?;
        let t = self
            .globals
            .get(&Global::Value(name.clone()))
            .copied()
            .ok_or_else(|| Outcome::UnknownGlobal(name.clone()))?;
        let value = self.machine(State::Force(t))?;
        Ok(ValueHandle {
            session: self.id,
            value,
        })
    }
    /// Build a lazy application of existing session-owned thunks. Like executable
    /// core, this low-level API assumes its caller has checked the argument types.
    /// Editor embedders should use knot-workbench's typed operation boundary.
    /// No argument is forced and no IO action is run. All handles are validated
    /// before allocation; interruption abandons this session as in `evaluate`.
    pub fn apply(
        &mut self,
        function: ThunkHandle,
        arguments: &[ThunkHandle],
    ) -> Result<ThunkHandle, Outcome> {
        self.validate_thunk(function)?;
        for argument in arguments {
            self.validate_thunk(*argument)?;
        }
        let mut result = ThunkId(function.thunk);
        for argument in arguments {
            self.tick()?;
            result = self.pending(
                Pending::Apply(result, ThunkId(argument.thunk)),
                self.thunks[result.0].origin,
            );
        }
        self.tick()?;
        Ok(ThunkHandle {
            session: self.id,
            thunk: result.0,
        })
    }
    /// Force an owned thunk to WHNF using the ordinary cumulative session budget.
    /// Use `inspect` instead when interruption must roll back a preview query.
    pub fn force(&mut self, handle: ThunkHandle) -> Result<ValueHandle, Outcome> {
        self.validate_thunk(handle)?;
        let value = self.machine(State::Force(ThunkId(handle.thunk)))?;
        Ok(ValueHandle {
            session: self.id,
            value,
        })
    }
    /// Each invocation executes the action anew, even when its pure description
    /// or intermediate continuation thunks have already been evaluated.
    pub fn run(&mut self, name: &GlobalName, host: &mut dyn IoHandler) -> Result<(), Outcome> {
        if let Some(tracking) = &mut self.tracking {
            tracking.in_io = true;
        }
        let result = self.run_action(name, host);
        if let Some(tracking) = &mut self.tracking {
            tracking.in_io = false;
        }
        result
    }
    fn run_action(&mut self, name: &GlobalName, host: &mut dyn IoHandler) -> Result<(), Outcome> {
        let handle = self.evaluate(name)?;
        let root = self.globals[&Global::Value(name.clone())];
        if let Some(tracking) = &mut self.tracking {
            tracking.executed_io(root);
        }
        let origin = self.thunks[root.0].origin;
        let mut action = handle.value;
        let mut continuations = Vec::new();
        loop {
            self.tick()?;
            let origin = match &self.values[action] {
                Value::Io(_, o) => *o,
                _ => origin,
            };
            let result = match self.values[action].clone() {
                Value::Io(IoAction::Bind(first, then), _) => {
                    continuations.push(then);
                    action = self.machine(State::Force(first))?;
                    continue;
                }
                Value::Io(IoAction::Pure(value), _) => value,
                Value::Io(IoAction::PutLine(text), _) => {
                    let value = self.machine(State::Force(text))?;
                    let Value::Lit(Literal::String(text)) = &self.values[value] else {
                        return Err(self.invalid(origin, "putStrLn argument"));
                    };
                    host.put_line(text)
                        .map_err(|e| self.runtime(RuntimeErrorKind::Io(e), origin))?;
                    self.ready(Value::Lit(Literal::Unit), origin)
                }
                Value::Io(IoAction::GetLine, _) => {
                    let text = host
                        .get_line()
                        .map_err(|e| self.runtime(RuntimeErrorKind::Io(e), origin))?
                        .ok_or_else(|| self.runtime(RuntimeErrorKind::EndOfInput, origin))?;
                    self.charge_bytes(text.len())?;
                    self.ready(Value::Lit(Literal::String(text)), origin)
                }
                _ => return Err(self.invalid(origin, "runner expected an IO action")),
            };
            if let Some(then) = continuations.pop() {
                let applied =
                    self.pending(Pending::Apply(then, result), self.thunks[then.0].origin);
                action = self.machine(State::Force(applied))?;
            } else {
                return Ok(());
            }
        }
    }
    fn work_clock(&mut self) -> u64 {
        #[cfg(feature = "incremental")]
        {
            let copied = self
                .thunks
                .take_copy_work()
                .saturating_add(self.values.take_copy_work())
                .saturating_add(self.envs.take_copy_work())
                .saturating_add(self.global_refs.take_copy_work());
            let before = self.stats.steps;
            self.stats.steps = before
                .saturating_add(copied)
                .min(self.limits.steps.max(before));
            self.stats.maintenance_steps += self.stats.steps - before;
        }
        self.stats.steps
    }
    fn tick(&mut self) -> Result<(), Outcome> {
        self.work_clock();
        if self.abandoned {
            return Err(Outcome::Abandoned);
        }
        let stop = if self.limits.cancellation.load(Ordering::Relaxed)
            || self
                .inspection
                .as_ref()
                .is_some_and(|c| c.limits.cancellation.load(Ordering::Relaxed))
        {
            Some(Outcome::Cancelled)
        } else if self.stats.steps >= self.limits.steps
            || self.heap_entries() > self.limits.heap_entries
        {
            Some(Outcome::BudgetExhausted)
        } else {
            None
        };
        if let Some(stop) = stop {
            self.abandoned = true;
            return Err(stop);
        }
        self.stats.steps += 1;
        Ok(())
    }
    fn check_payload(&mut self, bytes: usize) -> Result<(), Outcome> {
        if bytes > self.limits.primitive_bytes {
            self.abandoned = true;
            return Err(Outcome::BudgetExhausted);
        }
        Ok(())
    }
    fn charge_bytes(&mut self, bytes: usize) -> Result<(), Outcome> {
        self.check_payload(bytes)?;
        if bytes as u64 > self.limits.steps.saturating_sub(self.stats.steps) {
            self.abandoned = true;
            return Err(Outcome::BudgetExhausted);
        }
        self.stats.steps += bytes as u64;
        self.tick()
    }
    fn runtime(&self, kind: RuntimeErrorKind, origin: Origin) -> Outcome {
        Outcome::Runtime(RuntimeError {
            kind,
            origin: self.source_origin(origin),
            trace: vec![],
        })
    }
    fn invalid(&self, origin: Origin, message: &str) -> Outcome {
        self.runtime(RuntimeErrorKind::InvalidCore(message.into()), origin)
    }
    fn env(&mut self, parent: Option<EnvId>, bindings: Bindings, origin: Option<Origin>) -> EnvId {
        let id = self.envs.len();
        let activation = ActivationId(self.stats.activations);
        self.stats.activations += 1;
        let empty = bindings.len() == 0 && parent.is_none_or(|p| self.envs[p].empty);
        self.envs.push(Env {
            empty,
            parent,
            bindings,
            activation,
            origin,
        });
        id
    }
    fn local(&self, mut env: EnvId, id: LocalId) -> Option<ThunkId> {
        loop {
            let frame = &self.envs[env];
            if let Some(t) = frame.bindings.get(&id) {
                return Some(*t);
            }
            #[cfg(feature = "compact-captures")]
            if frame.bindings.is_capture_boundary() {
                return None;
            }
            env = frame.parent?;
        }
    }
    fn value(&mut self, value: Value) -> ValueId {
        let id = self.values.len();
        self.values.push(value);
        id
    }
    fn pending(&mut self, pending: Pending, origin: Origin) -> ThunkId {
        let id = ThunkId(self.thunks.len());
        let activation = match &pending {
            Pending::Expr(_, env) | Pending::DecimalParts(_, env) => self.envs[*env].activation,
            Pending::Apply(f, _) | Pending::Projection(f, _) => self.thunks[f.0].activation,
            Pending::TextList { activation, .. } => *activation,
        };
        if let Some(tracking) = &mut self.tracking {
            tracking.allocate();
        }
        self.thunks.push(Thunk {
            origin,
            activation,
            state: ThunkState::Pending(pending),
        });
        id
    }
    fn cell_id(&self, use_id: ThunkId) -> CellId {
        match self.thunks[use_id.0].state {
            ThunkState::Shared(cell) => cell,
            _ => CellId(use_id.0),
        }
    }
    fn cell_state(&self, use_id: ThunkId) -> &ThunkState {
        &self.thunks[self.cell_id(use_id).0].state
    }
    fn shared_use(&mut self, cell: CellId, origin: Origin, activation: ActivationId) -> ThunkId {
        debug_assert!(self.tracking.is_none());
        debug_assert!(!matches!(self.thunks[cell.0].state, ThunkState::Shared(_)));
        let id = ThunkId(self.thunks.len());
        self.thunks.push(Thunk {
            origin,
            activation,
            state: ThunkState::Shared(cell),
        });
        id
    }
    fn admit_pending_expression(&self) -> bool {
        #[cfg(feature = "incremental")]
        return self.limits.cache_minimum_work == 0;
        #[cfg(not(feature = "incremental"))]
        true
    }
    fn admit_call_completion(&self, cost: cost::Stored, value: ValueId) -> bool {
        #[cfg(feature = "incremental")]
        return self.admit_completed_cost(cost, value);
        #[cfg(not(feature = "incremental"))]
        {
            let _ = (cost, value);
            true
        }
    }
    fn expression_scope_valid(&mut self, cell: CellId, env: EnvId) -> bool {
        #[cfg(feature = "incremental")]
        if self.source_code.is_some() {
            return self.cache_work(|s| {
                s.checked_source_scope(env)
                    && s.checked_source_scope(s.thunks[cell.0].activation.0 as usize)
            });
        }
        let _ = (cell, env);
        true
    }
    fn delay(&mut self, expr: ExprId, env: EnvId) -> ThunkId {
        // A reference already denotes a lazy cell; another wrapper adds no
        // value sharing. Keep wrappers when collecting per-use provenance.
        // Recursive/local and global binding installation bypass this shortcut
        // because their environments may not yet contain all shadowing names.
        if self.tracking.is_none() {
            let existing = match &self.program.expressions[expr.0].kind {
                Kind::Local(id) => self.local(env, *id),
                Kind::Global(_) => self.global_refs.get(expr.0).copied().flatten(),
                _ => None,
            };
            if let Some(thunk) = existing {
                return thunk;
            }
            // Preserve the actual lazy-cell identity, including when it was
            // imported from a previous revision. This also keeps call keys valid.
            if self.envs[env].empty {
                if let Some(thunk) = self
                    .memo
                    .expression(expr)
                    .filter(|cell| self.expression_scope_valid(*cell, env))
                {
                    self.stats.expression_cache_hits += 1;
                    return self.shared_use(
                        thunk,
                        self.program.expressions[expr.0].origin,
                        self.envs[env].activation,
                    );
                }
            }
        }
        self.delay_expression(expr, env)
    }
    fn delay_expression(&mut self, expr: ExprId, env: EnvId) -> ThunkId {
        self.pending(
            Pending::Expr(expr, env),
            self.program.expressions[expr.0].origin,
        )
    }
    fn ready(&mut self, value: Value, origin: Origin) -> ThunkId {
        let value = self.value(value);
        let id = ThunkId(self.thunks.len());
        let proof = self.record_evidence(|store, limits, work| {
            store.introduced(CellId(id.0), value, limits, work)
        });
        if let Some(tracking) = &mut self.tracking {
            tracking.allocate();
        }
        self.thunks.push(Thunk {
            origin,
            activation: self.envs[0].activation,
            state: ThunkState::Done(Ok(Completed {
                value,
                expression: None,
                cost: Default::default(),
                proof,
                stamp: self.validation.stamp(),
                #[cfg(feature = "incremental")]
                introduced: true,
            })),
        });
        id
    }
    fn machine(&mut self, state: State) -> Result<ValueId, Outcome> {
        #[cfg(feature = "incremental")]
        self.cache_work(|session| {
            session.evidence.prepare_regions(
                &session.program,
                &session.limits,
                &mut session.stats.steps,
            );
        });
        // Hold executable code separately from the mutable heap for this run.
        // Evaluation never appends/replaces code; checked appends happen between runs.
        let program = self.program.clone();
        let mut state = state;
        let mut stack = Vec::new();
        let mut matches = Vec::new();
        let mut costs = cost::Meter::default();
        let mut reads = evidence::Recorder::default();
        let result = loop {
            if let Err(error) = self.tick() {
                if let Some(tracking) = &mut self.tracking {
                    tracking.stop(DemandStatus::Interrupted);
                }
                break Err(error);
            }
            #[cfg(feature = "incremental")]
            if self.limits.evidence_pressure.is_some() {
                self.maybe_collect_evidence(&reads, &state);
            }
            // A single step never recursively calls the evaluator.
            let step = self.step(
                &program,
                state,
                &mut stack,
                &mut matches,
                &mut costs,
                &mut reads,
            );
            match step {
                Ok(next) => state = next,
                Err(StepEnd::Value(value)) => break Ok(value),
                Err(StepEnd::Failure(failure)) => {
                    let mut failure = *failure;
                    if let Outcome::Runtime(ref mut error) = failure {
                        for frame in stack.iter().rev() {
                            if let Frame::Update(t, _) = frame {
                                if error.trace.len() < 32 {
                                    error
                                        .trace
                                        .push(self.source_origin(self.thunks[t.0].origin));
                                }
                            }
                        }
                        let shared = Arc::new(error.clone());
                        for frame in &stack {
                            if let Frame::Update(t, _) = frame {
                                self.thunks[t.0].state = ThunkState::Done(Err(shared.clone()));
                            }
                        }
                    }
                    if let Some(tracking) = &mut self.tracking {
                        tracking.stop(if matches!(failure, Outcome::Runtime(_)) {
                            DemandStatus::Failed
                        } else {
                            DemandStatus::Interrupted
                        });
                    }
                    break Err(failure);
                }
            }
        };
        self.evidence.abandon_pending();
        self.work_clock();
        result
    }
    // There is one dispatch-loop call site. Keep each state transition inside
    // that loop so the compiler can eliminate by-value state/result transfers.
    #[inline(always)]
    fn step<'code>(
        &mut self,
        program: &'code Program,
        state: State,
        stack: &mut Vec<Frame<'code>>,
        matches: &mut Vec<Matching<'code>>,
        costs: &mut cost::Meter,
        reads: &mut evidence::Recorder,
    ) -> Result<State, StepEnd> {
        if let State::Force(t) = &state {
            #[cfg(feature = "incremental")]
            self.validate_completion(self.cell_id(*t))?;
            if let Some(tracking) = &mut self.tracking {
                let origin = self.thunks[t.0].origin;
                #[cfg(feature = "incremental")]
                let origin = self
                    .source_code
                    .as_ref()
                    .and_then(|view| view.origin(origin))
                    .unwrap_or(origin);
                tracking.force(*t, &self.thunks[t.0], origin);
            }
        }
        let next = match state {
            State::Force(use_id) => {
                let t = self.cell_id(use_id);
                match &self.thunks[t.0].state {
                    ThunkState::Shared(_) => unreachable!("computation owners never forward"),
                    ThunkState::Done(result) => {
                        self.stats.memo_hits += 1;
                        let done = *result
                            .as_ref()
                            .map_err(|e| StepEnd::from(Outcome::Runtime(e.as_ref().clone())))?;
                        self.record_evidence(|store, limits, work| {
                            reads.hit(done.proof, store, limits, work)
                        });
                        State::Return(done.value)
                    }
                    ThunkState::Evaluating(_) => {
                        return Err(self
                            .runtime(RuntimeErrorKind::Loop, self.thunks[t.0].origin)
                            .into())
                    }
                    ThunkState::Pending(p) => {
                        self.stats.thunk_evaluations += 1;
                        let expression = match p {
                            Pending::Expr(e, _) => Some(*e),
                            _ => None,
                        };
                        if let Some(checkpoint) = &mut self.inspection {
                            if t.0 < checkpoint.thunks {
                                checkpoint
                                    .undo
                                    .entry(t.0)
                                    .or_insert_with(|| self.thunks[t.0].state.clone());
                            }
                        }
                        // Evaluation consumes the recipe. Only inspection rollback
                        // and retained evidence need their own copy; successful
                        // completed reads above never mutate a shared heap page.
                        let ThunkState::Pending(p) = std::mem::replace(
                            &mut self.thunks[t.0].state,
                            ThunkState::Evaluating(expression),
                        ) else {
                            unreachable!("pending state checked above")
                        };
                        self.record_evidence(|store, limits, work| {
                            reads.begin_thunk(t, &p, store, limits, work)
                        });
                        stack.push(Frame::Update(t, costs.begin(self.cost_clock())));
                        match p {
                            Pending::Expr(e, env) => {
                                if !self.envs[env].empty && self.tracking.is_none() {
                                    if let Some(previous) =
                                        self.cache_work(|session| session.reused_capture(e, env))?
                                    {
                                        if previous != t {
                                            self.stats.captured_cache_hits += 1;
                                            return Ok(State::Force(previous.owner()));
                                        }
                                    }
                                }
                                if self.envs[env].empty
                                    && self.tracking.is_none()
                                    && self.limits.application_cache_entries > 0
                                    && !matches!(
                                        self.program.expressions[e.0].kind,
                                        Kind::Local(_)
                                            | Kind::Global(_)
                                            | Kind::Lit(_)
                                            | Kind::Rational(_)
                                            | Kind::Ctor(..)
                                            | Kind::Prim(_)
                                    )
                                {
                                    if let Some(previous) = self.memo.expression(e) {
                                        if previous != t
                                            && self.expression_scope_valid(previous, env)
                                        {
                                            self.stats.expression_cache_hits += 1;
                                            #[cfg(feature = "incremental")]
                                            if self.limits.cache_eviction_samples > 0
                                                && self.inspection.is_none()
                                            {
                                                self.memo.note_expression_hit(e);
                                            }
                                            return Ok(State::Force(previous.owner()));
                                        }
                                    } else if self.inspection.is_none()
                                        && self.admit_pending_expression()
                                    {
                                        #[cfg(feature = "incremental")]
                                        let admit = self.cache_work(|session| {
                                            session.optional_cache_room(1, Default::default())
                                        });
                                        #[cfg(not(feature = "incremental"))]
                                        let admit = true;
                                        if admit {
                                            self.memo.remember_expression(
                                                e,
                                                t,
                                                self.limits.application_cache_entries,
                                            );
                                        }
                                    }
                                }
                                State::Eval(e, env)
                            }
                            Pending::TextList {
                                source,
                                separator,
                                offset,
                                activation,
                            } => self.text_list(
                                source,
                                separator,
                                offset,
                                activation,
                                self.thunks[t.0].origin,
                            )?,
                            Pending::Projection(parts, index) => {
                                stack.push(Frame::SelectIndex(index, self.thunks[t.0].origin));
                                State::Force(parts)
                            }
                            Pending::DecimalParts(literal, _) => {
                                let power = literal.exponent().magnitude().to_u32_digits();
                                let power = match power.as_slice() {
                                    [] => 0,
                                    [p] if *p <= self.limits.decimal_power => *p,
                                    _ => {
                                        self.abandoned = true;
                                        return Err(Outcome::BudgetExhausted.into());
                                    }
                                };
                                let bytes = (literal
                                    .significand()
                                    .bits()
                                    .saturating_add(u64::from(power) * 4))
                                .div_ceil(8) as usize
                                    + 1;
                                self.charge_numeric(bytes, 0)?;
                                let (n, d) = literal
                                    .rational_parts(power)
                                    .expect("checked expansion bound");
                                let origin = self.thunks[t.0].origin;
                                let n = self.ready(Value::Lit(Literal::Integer(n)), origin);
                                let d = self.ready(Value::Lit(Literal::Integer(d)), origin);
                                State::Return(self.value(Value::Tuple(vec![n, d])))
                            }
                            Pending::Apply(f, x) => {
                                stack.push(Frame::Apply(
                                    CallArgs::One([x]),
                                    self.thunks[t.0].origin,
                                ));
                                State::Force(f)
                            }
                        }
                    }
                }
            }
            State::Eval(e, env) => {
                let expr = &program.expressions[e.0];
                if let Some(tracking) = &mut self.tracking {
                    let current_origin = expr.origin;
                    #[cfg(feature = "incremental")]
                    let current_origin = self
                        .source_code
                        .as_ref()
                        .and_then(|view| view.origin(current_origin))
                        .unwrap_or(current_origin);
                    tracking.code(
                        e,
                        current_origin.node,
                        if let Kind::Global(g) = &expr.kind {
                            Some(g)
                        } else {
                            None
                        },
                    );
                }
                let o = expr.origin;
                if let Kind::Global(g) = &expr.kind {
                    // Avoid both the tree lookup and cloning the qualified name
                    // on every evaluation. Tracking above still records the read.
                    let t = self.global_refs[e.0]
                        .ok_or_else(|| self.invalid(o, &format!("missing global {g:?}")))?;
                    return Ok(State::Force(t));
                }
                match &expr.kind {
                    Kind::Lit(l) => {
                        self.charge_bytes(literal_bytes(l))?;
                        State::Return(self.value(Value::Lit(l.clone())))
                    }
                    Kind::Rational(literal) => {
                        self.charge_bytes(decimal_bytes(literal))?;
                        let literal = Arc::new(literal.clone());
                        let parts = self.pending(Pending::DecimalParts(literal.clone(), env), o);
                        let n = self.pending(Pending::Projection(parts, 0), o);
                        let d = self.pending(Pending::Projection(parts, 1), o);
                        State::Return(self.value(Value::DecimalRatio(
                            ctor.ratio.clone(),
                            literal,
                            vec![n, d],
                        )))
                    }
                    Kind::Local(id) => State::Force(
                        self.local(env, *id)
                            .ok_or_else(|| self.invalid(o, "unbound local"))?,
                    ),
                    Kind::Global(_) => unreachable!("handled before dispatch"),
                    Kind::Host(name, signature) => {
                        let f = self.hosts.function(name).ok_or_else(|| {
                            self.runtime(
                                RuntimeErrorKind::Host(format!("missing host function {name:?}")),
                                o,
                            )
                        })?;
                        if &f.signature != signature {
                            return Err(self.invalid(o, "host signature mismatch").into());
                        }
                        if signature.parameters.is_empty() {
                            self.host(name, vec![], o, reads)?
                        } else {
                            State::Return(self.value(Value::Partial(
                                Callable::Host(name.clone(), signature.parameters.len()),
                                vec![],
                            )))
                        }
                    }
                    Kind::Prim(name) => {
                        let spec = primitive::spec(name).ok_or_else(|| {
                            self.runtime(
                                RuntimeErrorKind::Unsupported(format!("primitive {name}")),
                                o,
                            )
                        })?;
                        if spec.arity == 0 {
                            self.primitive(name, &[], &[], o, stack)?
                        } else {
                            State::Return(self.value(Value::Partial(
                                Callable::Primitive(Arc::from(name.as_str())),
                                vec![],
                            )))
                        }
                    }
                    Kind::Ctor(name, arity) => State::Return(self.value(if *arity == 0 {
                        Value::Ctor(Arc::new(name.clone()), vec![])
                    } else {
                        Value::Partial(
                            Callable::Constructor(Arc::new(name.clone()), *arity),
                            vec![],
                        )
                    })),
                    Kind::Lam(param, body) => {
                        // A pending application can bind this lambda directly. Keep its
                        // source scope, as an escaping thunk may outlive the call.
                        #[cfg(not(any(
                            feature = "compact-captures",
                            feature = "cost-accounting"
                        )))]
                        if self.tracking.is_none() && self.inspection.is_none() {
                            if let Some(Frame::Apply(args, _)) = stack.last_mut() {
                                if !args.as_slice().is_empty() {
                                    let arg = args.take_first();
                                    if args.as_slice().is_empty() {
                                        stack.pop();
                                    }
                                    let child = self.env(
                                        Some(env),
                                        Bindings::One(*param, arg),
                                        Some(program.expressions[body.0].origin),
                                    );
                                    return Ok(State::Eval(*body, child));
                                }
                            }
                        }
                        let capture = self.capture(e, env);
                        State::Return(self.value(Value::Closure(*param, *body, capture)))
                    }
                    Kind::App(f, args) => {
                        let args = match args.as_slice() {
                            [one] => CallArgs::One([self.delay(*one, env)]),
                            [left, right] => {
                                CallArgs::Two([self.delay(*left, env), self.delay(*right, env)])
                            }
                            [] => CallArgs::Empty,
                            other => {
                                CallArgs::Many(other.iter().map(|e| self.delay(*e, env)).collect())
                            }
                        };
                        stack.push(Frame::Apply(args, o));
                        State::Eval(*f, env)
                    }
                    Kind::Let(bindings, body) => {
                        let child = self.env(Some(env), Bindings::Empty, Some(o));
                        for (id, e) in bindings {
                            // All recursive bindings must exist before resolving aliases.
                            let t = self.delay_expression(*e, child);
                            self.envs[child].bindings.insert(*id, t);
                            self.envs[child].empty = false;
                        }
                        State::Eval(*body, child)
                    }
                    Kind::Case(x, arms) => {
                        let root = self.delay(*x, env);
                        let pending = arms
                            .first()
                            .map(|(p, _)| vec![(p, root)])
                            .unwrap_or_default();
                        matches.push(Matching {
                            root,
                            case: e,
                            arm: 0,
                            pending,
                            bindings: Bindings::Empty,
                            env,
                            origin: o,
                        });
                        State::Match
                    }
                    Kind::Tuple(xs) => {
                        let xs = xs.iter().map(|x| self.delay(*x, env)).collect();
                        State::Return(self.value(Value::Tuple(xs)))
                    }
                    Kind::Record(fs) => {
                        let fs = fs
                            .iter()
                            .map(|(n, x)| (n.clone(), self.delay(*x, env)))
                            .collect();
                        State::Return(self.value(Value::Record(fs)))
                    }
                    Kind::Dict(fs) => {
                        let fs = fs
                            .iter()
                            .map(|(n, x)| (n.clone(), self.delay(*x, env)))
                            .collect();
                        State::Return(self.value(Value::Dict(fs)))
                    }
                    Kind::Select(x, field) => {
                        #[cfg(not(feature = "cost-accounting"))]
                        if self.tracking.is_none() && self.inspection.is_none() {
                            let cell = match &program.expressions[x.0].kind {
                                Kind::Global(_) => self.global_refs[x.0],
                                Kind::Local(id) => self.local(env, *id),
                                _ => None,
                            };
                            if let Some(cell) = cell {
                                if let ThunkState::Done(Ok(done)) = self.cell_state(cell) {
                                    let field = self.selected_thunk(done.value, field, o)?;
                                    self.stats.memo_hits += 1;
                                    if let ThunkState::Done(Ok(done)) = self.cell_state(field) {
                                        let value = done.value;
                                        self.stats.memo_hits += 1;
                                        return Ok(State::Return(value));
                                    }
                                    return Ok(State::Force(field));
                                }
                            }
                        }
                        stack.push(Frame::Select(field, o));
                        State::Eval(*x, env)
                    }
                    Kind::RecordUpdate(x, fs) => {
                        let fs = fs
                            .iter()
                            .map(|(n, e)| (n.as_str(), self.delay(*e, env)))
                            .collect();
                        stack.push(Frame::RecordUpdate(fs, o));
                        State::Eval(*x, env)
                    }
                    Kind::Unsupported(reason) => {
                        return Err(self
                            .runtime(RuntimeErrorKind::Unsupported(reason.clone()), o)
                            .into())
                    }
                }
            }
            State::Return(v) => match stack.pop() {
                None => return Err(StepEnd::Value(v)),
                Some(Frame::Update(t, mark)) => {
                    if let Some(tracking) = &mut self.tracking {
                        tracking.complete(t.owner());
                    }
                    let expression = match self.thunks[t.0].state {
                        ThunkState::Evaluating(e) => e,
                        _ => None,
                    };
                    let cost = costs.finish(mark, self.cost_clock());
                    let proof = self.record_evidence(|store, limits, work| {
                        reads.finish(v, cost, store, limits, work)
                    });
                    self.thunks[t.0].state = ThunkState::Done(Ok(Completed {
                        value: v,
                        expression,
                        cost,
                        proof,
                        stamp: self.validation.stamp(),
                        #[cfg(feature = "incremental")]
                        introduced: false,
                    }));
                    #[cfg(feature = "incremental")]
                    self.admit_expression_completion(t, expression, cost, v);
                    State::Return(v)
                }
                Some(Frame::RememberApplication(key, mark, scope)) => {
                    let cost = costs.finish(mark, self.cost_clock());
                    let proof = self.record_evidence(|store, limits, work| {
                        reads.finish(v, cost, store, limits, work)
                    });
                    if let Some(key) = key.filter(|_| self.admit_call_completion(cost, v)) {
                        let key = match key {
                            RememberCall::Prepared(key) => key,
                            #[cfg(feature = "incremental")]
                            RememberCall::Deferred(function, argument) => {
                                self.call_key(function, argument)
                            }
                        };
                        #[cfg(feature = "incremental")]
                        let admit = self.cache_work(|session| {
                            let slots = if session.memo.get(&key).is_some() {
                                0
                            } else {
                                key.slots()
                            };
                            session.optional_cache_room(slots, cost)
                        });
                        #[cfg(not(feature = "incremental"))]
                        let admit = true;
                        if admit {
                            #[cfg(feature = "incremental")]
                            let scope = if self.values[v].needs_call_scope() {
                                scope
                            } else {
                                Default::default()
                            };
                            self.memo.insert_completion(
                                key,
                                memo::Completion {
                                    #[cfg(feature = "incremental")]
                                    access: Default::default(),
                                    value: v,
                                    cost,
                                    proof,
                                    stamp: self.validation.stamp(),
                                    scope,
                                },
                                self.limits.application_cache_entries,
                            );
                        }
                    }
                    State::Return(v)
                }
                Some(Frame::ConcatTail(buffer, origin)) => {
                    self.concat_list(v, buffer, origin, stack)?
                }
                Some(Frame::ConcatHead(mut buffer, tail, origin)) => {
                    let len = primitive::string(&self.values[v])
                        .map_err(|e| self.runtime(e, origin))?
                        .len();
                    self.check_payload(buffer.len().saturating_add(len))?;
                    self.charge_bytes(len)?;
                    let piece =
                        primitive::string(&self.values[v]).map_err(|e| self.runtime(e, origin))?;
                    buffer.push_str(piece);
                    stack.push(Frame::ConcatTail(buffer, origin));
                    State::Force(tail)
                }
                Some(Frame::RatioNumerator(denominator, origin)) => {
                    let n = primitive::integer(&self.values[v])
                        .map_err(|e| self.runtime(e, origin))?
                        .clone();
                    stack.push(Frame::RatioDenominator(n, origin));
                    State::Force(denominator)
                }
                Some(Frame::RatioDenominator(n, origin)) => {
                    let d = primitive::integer(&self.values[v])
                        .map_err(|e| self.runtime(e, origin))?
                        .clone();
                    self.charge_numeric((n.bits() + d.bits()).div_ceil(8) as usize, 256)?;
                    let f = numeric::ratio_to_float(&n, &d).map_err(|e| self.runtime(e, origin))?;
                    State::Return(self.value(Value::Lit(Literal::Float(f))))
                }
                Some(Frame::Apply(mut args, o)) => {
                    self.tick()?;
                    let f = v;
                    if let Some(tracking) = &mut self.tracking {
                        let capture = if let Value::Closure(_, _, env) = &self.values[f] {
                            Some(self.envs[env.scope].activation)
                        } else {
                            None
                        };
                        tracking.call(f, args.as_slice(), capture);
                    }
                    if args.as_slice().is_empty() {
                        State::Return(f)
                    } else {
                        match self.values[f].clone() {
                            Value::Closure(param, body, env) => {
                                let first = args.take_first();
                                if !args.as_slice().is_empty() {
                                    stack.push(Frame::Apply(args, o));
                                }
                                let mut remember = None;
                                if self.tracking.is_none()
                                    && self.limits.application_cache_entries > 0
                                {
                                    #[cfg(feature = "incremental")]
                                    let lookup = self.call_may_match(f, body);
                                    #[cfg(not(feature = "incremental"))]
                                    let lookup = true;
                                    if lookup {
                                        let key = self.call_key(f, self.cell_id(first));
                                        if let Some((value, witness, argument, proof, stamp)) =
                                            self.memo.get(&key)
                                        {
                                            let scope = self.cache_work(|session| {
                                                session.call_scope_compatible(f, witness)
                                                    && session.argument_scope_compatible(
                                                        key.argument,
                                                        argument,
                                                    )
                                            });
                                            let valid = self.valid_in_world(stamp, proof)?;
                                            #[allow(unused_mut)]
                                            let mut result = (scope && valid).then_some(value);
                                            #[cfg(feature = "incremental")]
                                            if valid && !scope {
                                                result = self.cache_work(|session| {
                                                    session.project_call_result(
                                                        &key, first, witness, argument, value,
                                                    )
                                                })?;
                                                if result.is_some() {
                                                    self.stats.application_projected_hits += 1;
                                                }
                                            }
                                            #[cfg(feature = "incremental")]
                                            let stored_value = value;
                                            if let Some(value) = result {
                                                #[cfg(feature = "incremental")]
                                                if value == stored_value
                                                    && !self.validation.current(stamp)
                                                    && self.inspection.is_none()
                                                {
                                                    // Like a validated thunk, this exact stored
                                                    // completion is now known in the receiving
                                                    // world. A projection returning this same
                                                    // immutable value also certifies it; newly
                                                    // projected values keep a separate witness.
                                                    // Collection must not discard confirmed work.
                                                    self.memo.confirm_call(
                                                        &key,
                                                        self.validation.stamp(),
                                                    );
                                                }
                                                self.stats.application_cache_hits += 1;
                                                #[cfg(feature = "incremental")]
                                                if self.limits.cache_eviction_samples > 0
                                                    && self.inspection.is_none()
                                                {
                                                    self.memo.note_call_hit(&key);
                                                }
                                                #[cfg(feature = "incremental")]
                                                let proof = if scope {
                                                    proof
                                                } else {
                                                    let argument = self.cell_id(first);
                                                    self.record_evidence(|store, limits, work| {
                                                        store.projected_call(
                                                            f, argument, value, proof, limits, work,
                                                        )
                                                    })
                                                };
                                                self.record_evidence(|store, limits, work| {
                                                    reads.hit(proof, store, limits, work)
                                                });
                                                return Ok(State::Return(value));
                                            }
                                            #[cfg(feature = "incremental")]
                                            if !valid {
                                                self.stats.application_validity_misses += 1;
                                            } else {
                                                self.stats.application_scope_misses += 1;
                                            }
                                        } else {
                                            #[cfg(feature = "incremental")]
                                            {
                                                self.stats.application_key_misses += 1;
                                            }
                                        }
                                        // Inspection may read existing entries, but never retain
                                        // values that its interruption could roll back.
                                        if self.inspection.is_none() {
                                            remember = Some(RememberCall::Prepared(key));
                                        }
                                    } else {
                                        #[cfg(feature = "incremental")]
                                        {
                                            self.stats.application_key_skips += 1;
                                            if self.inspection.is_none() {
                                                remember = Some(RememberCall::Deferred(
                                                    f,
                                                    self.cell_id(first),
                                                ));
                                            }
                                        }
                                    }
                                }
                                let measured =
                                    if remember.is_some() || cfg!(feature = "cost-accounting") {
                                        let argument = self.cell_id(first);
                                        self.record_evidence(|store, limits, work| {
                                            reads.begin_call(
                                                f,
                                                argument,
                                                (param, body),
                                                store,
                                                limits,
                                                work,
                                            )
                                        });
                                        Some(costs.begin(self.cost_clock()))
                                    } else {
                                        None
                                    };
                                let child = self.env(
                                    Some(env.scope),
                                    env.bindings(param, first),
                                    Some(self.program.expressions[body.0].origin),
                                );
                                if let Some(mark) = measured {
                                    stack.push(Frame::RememberApplication(
                                        remember,
                                        mark,
                                        memo::Scope::new(child),
                                    ));
                                }
                                State::Eval(body, child)
                            }
                            Value::Partial(callable, mut supplied) => {
                                let arity = match &callable {
                                    Callable::Constructor(_, n) | Callable::Host(_, n) => *n,
                                    Callable::Primitive(n) => primitive::spec(n).unwrap().arity,
                                };
                                if !supplied.is_empty() {
                                    supplied.extend_from_slice(args.as_slice());
                                    args = CallArgs::Many(supplied);
                                }
                                if args.as_slice().len() < arity {
                                    State::Return(
                                        self.value(Value::Partial(callable, args.into_vec())),
                                    )
                                } else {
                                    if args.as_slice().len() > arity {
                                        let mut all = args.into_vec();
                                        let rest = all.split_off(arity);
                                        stack.push(Frame::Apply(CallArgs::Many(rest), o));
                                        args = CallArgs::Many(all);
                                    }
                                    match callable {
                                        Callable::Constructor(n, _) => State::Return(
                                            self.value(Value::Ctor(n, args.into_vec())),
                                        ),
                                        Callable::Host(name, _) => {
                                            let supplied = args.into_vec();
                                            let first = supplied[0];
                                            let values = Vec::with_capacity(supplied.len());
                                            stack.push(Frame::Host {
                                                name,
                                                args: supplied,
                                                values,
                                                origin: o,
                                            });
                                            State::Force(first)
                                        }
                                        Callable::Primitive(name) => {
                                            if primitive::spec(&name).unwrap().strict {
                                                let t = args.as_slice()[0];
                                                if arity > 2 {
                                                    stack.push(Frame::PrimitiveMany {
                                                        name,
                                                        args,
                                                        values: Vec::with_capacity(arity),
                                                        origin: o,
                                                    });
                                                } else {
                                                    stack.push(Frame::Primitive {
                                                        name,
                                                        args,
                                                        first: None,
                                                        origin: o,
                                                    });
                                                }
                                                State::Force(t)
                                            } else {
                                                self.primitive(
                                                    &name,
                                                    args.as_slice(),
                                                    &[],
                                                    o,
                                                    stack,
                                                )?
                                            }
                                        }
                                    }
                                }
                            }
                            _ => return Err(self.invalid(o, "application of non-function").into()),
                        }
                    }
                }

                Some(Frame::Select(field, o)) => State::Force(self.selected_thunk(v, field, o)?),
                Some(Frame::SelectIndex(index, o)) => {
                    State::Force(self.selected_thunk(v, &Field::Index(index), o)?)
                }
                Some(Frame::RecordUpdate(fs, o)) => {
                    let Value::Record(mut fields) = self.values[v].clone() else {
                        return Err(self.invalid(o, "record update").into());
                    };
                    for (n, t) in fs {
                        let Some(field) = fields.get_mut(n) else {
                            return Err(self.invalid(o, "unknown update field").into());
                        };
                        *field = t;
                    }
                    State::Return(self.value(Value::Record(fields)))
                }
                Some(Frame::Match(p, t)) => {
                    let m = matches.last_mut().expect("active match");
                    if !self.match_value(m, p, t, v) {
                        m.arm += 1;
                        m.bindings.clear();
                        let Kind::Case(_, arms) = &program.expressions[m.case.0].kind else {
                            unreachable!("matching retains its case expression")
                        };
                        m.pending = arms
                            .get(m.arm)
                            .map(|(p, _)| vec![(p, m.root)])
                            .unwrap_or_default();
                    }
                    State::Match
                }
                Some(Frame::Host {
                    name,
                    args,
                    mut values,
                    origin,
                }) => {
                    values.push(self.values[v].clone());
                    if values.len() == args.len() {
                        self.host(&name, values, origin, reads)?
                    } else {
                        let next = args[values.len()];
                        stack.push(Frame::Host {
                            name,
                            args,
                            values,
                            origin,
                        });
                        State::Force(next)
                    }
                }
                Some(Frame::PrimitiveMany {
                    name,
                    args,
                    mut values,
                    origin,
                }) => {
                    values.push(v);
                    if let Some(&next) = args.as_slice().get(values.len()) {
                        stack.push(Frame::PrimitiveMany {
                            name,
                            args,
                            values,
                            origin,
                        });
                        State::Force(next)
                    } else {
                        self.primitive(&name, args.as_slice(), &values, origin, stack)?
                    }
                }
                Some(Frame::Primitive {
                    name,
                    args,
                    first,
                    origin,
                }) => {
                    if let Some(left) = first {
                        self.primitive(&name, args.as_slice(), &[left, v], origin, stack)?
                    } else if let [_, right] = args.as_slice() {
                        let t = *right;
                        stack.push(Frame::Primitive {
                            name,
                            args,
                            first: Some(v),
                            origin,
                        });
                        State::Force(t)
                    } else {
                        debug_assert_eq!(args.as_slice().len(), 1);
                        self.primitive(&name, args.as_slice(), &[v], origin, stack)?
                    }
                }
            },
            State::Match => {
                let m = matches.last_mut().expect("active match");
                let Kind::Case(_, arms) = &program.expressions[m.case.0].kind else {
                    unreachable!("matching retains its case expression")
                };
                if m.arm >= arms.len() {
                    return Err(self
                        .runtime(RuntimeErrorKind::NonExhaustive, m.origin)
                        .into());
                }
                if let Some((p, t)) = m.pending.pop() {
                    match p {
                        Pattern::Wildcard => State::Match,
                        Pattern::Bind(id) => {
                            m.bindings.insert(*id, t);
                            State::Match
                        }
                        Pattern::As(id, p) => {
                            m.bindings.insert(*id, t);
                            m.pending.push((p.as_ref(), t));
                            State::Match
                        }
                        _ => {
                            stack.push(Frame::Match(p, t));
                            State::Force(t)
                        }
                    }
                } else {
                    let m = matches.pop().expect("completed match");
                    let body = arms[m.arm].1;
                    let env = self.env(Some(m.env), m.bindings, Some(m.origin));
                    State::Eval(body, env)
                }
            }
        };
        Ok(next)
    }
    fn selected_thunk(
        &self,
        value: ValueId,
        field: &Field,
        origin: Origin,
    ) -> Result<ThunkId, Outcome> {
        match (&self.values[value], field) {
            (Value::Dict(fields), field) => fields.get(field),
            (Value::Record(fields), Field::Named(name)) => fields.get(name),
            (Value::Tuple(fields), Field::Index(index)) => fields.get(*index),
            _ => None,
        }
        .copied()
        .ok_or_else(|| self.invalid(origin, &format!("missing field {field:?}")))
    }
    fn match_value<'code>(
        &self,
        m: &mut Matching<'code>,
        p: &'code Pattern,
        _t: ThunkId,
        v: ValueId,
    ) -> bool {
        match (p, &self.values[v]) {
            (Pattern::Lit(a), Value::Lit(b)) => a == b,
            (Pattern::Ctor(a, ps), Value::Ctor(b, ts) | Value::DecimalRatio(b, _, ts))
                if a == b.as_ref() && ps.len() == ts.len() =>
            {
                m.pending.extend(ps.iter().zip(ts.iter().copied()).rev());
                true
            }
            (Pattern::Tuple(ps), Value::Tuple(ts)) if ps.len() == ts.len() => {
                m.pending.extend(ps.iter().zip(ts.iter().copied()).rev());
                true
            }
            (Pattern::Record(fs), Value::Record(ts)) => {
                for (name, id) in fs {
                    let Some(t) = ts.get(name) else {
                        return false;
                    };
                    m.bindings.insert(*id, *t);
                }
                true
            }
            _ => false,
        }
    }
    fn host(
        &mut self,
        name: &GlobalName,
        values: Vec<Value>,
        origin: Origin,
        reads: &mut evidence::Recorder,
    ) -> Result<State, StepEnd> {
        let registry = self.hosts.clone();
        let function = registry.function(name).expect("validated host call");
        if let Some(tracking) = &mut self.tracking {
            tracking.host(name, function.version.as_ref());
            for value in &values {
                if let Value::Native(native) = value {
                    tracking.native(native);
                }
            }
        }
        let mut args = Vec::with_capacity(values.len());
        for (value, ty) in values.into_iter().zip(&function.signature.parameters) {
            let value = match value {
                Value::Lit(l) => {
                    self.charge_bytes(literal_bytes(&l))?;
                    match l {
                        Literal::Int(v) => HostValue::Int(v),
                        Literal::Integer(v) => HostValue::Integer(v),
                        Literal::Float(v) => HostValue::Float(v),
                        Literal::String(v) => HostValue::String(v),
                        Literal::Unit => HostValue::Unit,
                    }
                }
                Value::Native(v) => HostValue::Native(v),
                Value::Ctor(n, fields)
                    if fields.is_empty() && (n == ctor.true_ || n == ctor.false_) =>
                {
                    HostValue::Bool(n == ctor.true_)
                }
                _ => return Err(self.invalid(origin, "invalid host argument").into()),
            };
            if !value.matches(ty, &registry) {
                return Err(self.invalid(origin, "host argument type mismatch").into());
            }
            args.push(value);
        }
        self.tick()?;
        self.stats.host_calls += 1;
        let mut context = HostContext::new(
            &registry,
            &self.limits.cancellation,
            self.limits.steps.saturating_sub(self.stats.steps),
            self.limits.host_bytes.saturating_sub(self.stats.host_bytes),
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (function.call)(&mut context, &args)
        }))
        .unwrap_or_else(|_| Err(HostError::Message("host callback panicked".into())));
        let _ = context.check_cancelled();
        self.stats.steps += context.work_used;
        self.stats.host_bytes += context.bytes_used;
        let result = match context.stopped {
            Some(error) => Err(error),
            None => result,
        };
        let result = match result {
            Ok(value) => value,
            Err(HostError::Message(message)) => {
                return Err(self.runtime(RuntimeErrorKind::Host(message), origin).into())
            }
            Err(error) => {
                self.abandoned = true;
                return Err(match error {
                    HostError::Cancelled => Outcome::Cancelled,
                    _ => Outcome::BudgetExhausted,
                }
                .into());
            }
        };
        if !result.matches(&function.signature.result, &registry) {
            return Err(self
                .runtime(
                    RuntimeErrorKind::Host("host result type mismatch".into()),
                    origin,
                )
                .into());
        }
        if let Some(tracking) = &mut self.tracking {
            if let HostValue::Native(native) = &result {
                tracking.native(native);
            }
        }
        let value = match result {
            HostValue::Int(v) => Value::Lit(Literal::Int(v)),
            HostValue::Integer(v) => Value::Lit(Literal::Integer(v)),
            HostValue::Float(v) => Value::Lit(Literal::Float(v)),
            HostValue::String(v) => Value::Lit(Literal::String(v)),
            HostValue::Unit => Value::Lit(Literal::Unit),
            HostValue::Bool(v) => Value::Ctor(
                if v {
                    ctor.true_.clone()
                } else {
                    ctor.false_.clone()
                },
                vec![],
            ),
            HostValue::Native(v) => Value::Native(v),
        };
        if let Value::Lit(l) = &value {
            self.charge_bytes(literal_bytes(l))?;
        }
        let value = self.value(value);
        self.record_evidence(|store, limits, work| {
            reads.host(name, function, value, store, limits, work)
        });
        Ok(State::Return(value))
    }
    // Produce one list cell per force, sharing UTF-8 storage between tails.
    fn text_list(
        &mut self,
        source: Arc<str>,
        separator: Option<Arc<str>>,
        offset: Option<usize>,
        activation: ActivationId,
        origin: Origin,
    ) -> Result<State, StepEnd> {
        let Some(offset) = offset else {
            return Ok(State::Return(
                self.value(Value::Ctor(ctor.nil.clone(), vec![])),
            ));
        };
        let (end, next) = match &separator {
            Some(sep) => match source[offset..].find(sep.as_ref()) {
                Some(index) => (offset + index, Some(offset + index + sep.len())),
                None => (source.len(), None),
            },
            None => match source[offset..].chars().next() {
                Some(c) => (offset + c.len_utf8(), Some(offset + c.len_utf8())),
                None => {
                    return Ok(State::Return(
                        self.value(Value::Ctor(ctor.nil.clone(), vec![])),
                    ))
                }
            },
        };
        self.charge_bytes(end - offset)?;
        let head = self.ready(
            Value::Lit(Literal::String(source[offset..end].into())),
            origin,
        );
        let tail = self.pending(
            Pending::TextList {
                source,
                separator,
                offset: next,
                activation,
            },
            origin,
        );
        Ok(State::Return(
            self.value(Value::Ctor(ctor.cons.clone(), vec![head, tail])),
        ))
    }
    fn concat_list<'code>(
        &mut self,
        list: ValueId,
        buffer: String,
        origin: Origin,
        stack: &mut Vec<Frame<'code>>,
    ) -> Result<State, StepEnd> {
        match &self.values[list] {
            Value::Ctor(n, fields) if *n == ctor.nil && fields.is_empty() => Ok(State::Return(
                self.value(Value::Lit(Literal::String(buffer))),
            )),
            Value::Ctor(n, fields) if *n == ctor.cons && fields.len() == 2 => {
                stack.push(Frame::ConcatHead(buffer, fields[1], origin));
                Ok(State::Force(fields[0]))
            }
            _ => Err(self.invalid(origin, "String.concat expected a list").into()),
        }
    }
    // Bound indivisible BigInt conversion/expansion work before allocation.
    // Scratch storage is bounded and charged linearly, never squared as operand work.
    fn charge_numeric(&mut self, bytes: usize, scratch: usize) -> Result<(), Outcome> {
        self.charge_bytes(bytes.saturating_add(scratch))?;
        let cost = (bytes as u64).saturating_mul(bytes as u64);
        if cost > self.limits.steps.saturating_sub(self.stats.steps) {
            self.abandoned = true;
            return Err(Outcome::BudgetExhausted);
        }
        self.stats.steps += cost;
        Ok(())
    }
    fn primitive<'code>(
        &mut self,
        name: &str,
        args: &[ThunkId],
        values: &[ValueId],
        origin: Origin,
        stack: &mut Vec<Frame<'code>>,
    ) -> Result<State, StepEnd> {
        self.stats.primitive_calls += 1;
        if name == "primRationalToFloat" {
            return match &self.values[values[0]] {
                Value::DecimalRatio(_, literal, _) => {
                    self.charge_numeric(decimal_bytes(literal).saturating_mul(4), 256)?;
                    let Value::DecimalRatio(_, literal, _) = &self.values[values[0]] else {
                        unreachable!("decimal argument is immutable");
                    };
                    let result = literal.to_f64();
                    Ok(State::Return(
                        self.value(Value::Lit(Literal::Float(result))),
                    ))
                }
                Value::Ctor(n, fields) if *n == ctor.ratio && fields.len() == 2 => {
                    stack.push(Frame::RatioNumerator(fields[1], origin));
                    Ok(State::Force(fields[0]))
                }
                _ => Err(self.invalid(origin, "rational conversion argument").into()),
            };
        }
        if name == "primFloatToRational" {
            let f =
                primitive::float(&self.values[values[0]]).map_err(|e| self.runtime(e, origin))?;
            self.charge_bytes(256)?;
            let (n, d) = numeric::float_to_ratio(f).map_err(|e| self.runtime(e, origin))?;
            let n = self.ready(Value::Lit(Literal::Integer(n)), origin);
            let d = self.ready(Value::Lit(Literal::Integer(d)), origin);
            return Ok(State::Return(
                self.value(Value::Ctor(ctor.ratio.clone(), vec![n, d])),
            ));
        }
        let bytes: usize = values
            .iter()
            .map(|v| match &self.values[*v] {
                Value::Lit(l) => literal_bytes(l),
                _ => 0,
            })
            .sum();
        self.charge_bytes(bytes)?;
        match name {
            "primStringToList" | "primStringSplitOn" => {
                let separator = if name == "primStringSplitOn" {
                    let sep = primitive::string(&self.values[values[0]])
                        .map_err(|e| self.runtime(e, origin))?;
                    if sep.is_empty() {
                        return Err(self
                            .runtime(
                                RuntimeErrorKind::Error("String.splitOn: empty separator".into()),
                                origin,
                            )
                            .into());
                    }
                    Some(Arc::from(sep))
                } else {
                    None
                };
                let source = Arc::from(
                    primitive::string(&self.values[*values.last().unwrap()])
                        .map_err(|e| self.runtime(e, origin))?,
                );
                let activation = self.thunks[args.last().unwrap().0].activation;
                return self.text_list(source, separator, Some(0), activation, origin);
            }
            "primStringConcat" => return self.concat_list(values[0], String::new(), origin, stack),
            "primStringToInt" | "primStringToFloat" => {
                let input = primitive::string(&self.values[values[0]])
                    .map_err(|e| self.runtime(e, origin))?;
                let parsed = if name == "primStringToInt" {
                    text::integer(input).map(Literal::Int)
                } else {
                    text::float(input).map(Literal::Float)
                };
                let result = match parsed {
                    None => Value::Ctor(ctor.nothing.clone(), vec![]),
                    Some(literal) => {
                        let field = self.ready(Value::Lit(literal), origin);
                        Value::Ctor(ctor.just.clone(), vec![field])
                    }
                };
                return Ok(State::Return(self.value(result)));
            }
            _ => {}
        }
        if let Some(bytes) =
            primitive::text_output_bytes(name, &PrimitiveArguments::new(&self.values, values))
                .map_err(|e| self.runtime(e, origin))?
        {
            self.charge_bytes(bytes)?;
        }
        if name.starts_with("primInteger")
            && matches!(
                name,
                "primIntegerMul" | "primIntegerQuot" | "primIntegerRem"
            )
        {
            // Conservative work bound before entering an indivisible BigInt operation.
            let cost = (bytes as u64).saturating_mul(bytes as u64);
            if cost > self.limits.steps.saturating_sub(self.stats.steps) {
                self.abandoned = true;
                return Err(Outcome::BudgetExhausted.into());
            }
            self.stats.steps += cost;
        }
        if name == "primIntegerToFloat" {
            self.charge_numeric(bytes, 256)?;
        }
        if name == "primFloatShow" {
            self.charge_bytes(1024)?;
        }
        if name == "primFloatTruncate" {
            self.charge_bytes(128)?;
        }
        let value = primitive::execute(
            name,
            args,
            &PrimitiveArguments::new(&self.values, values),
            origin,
        )
        .map_err(|kind| self.runtime(kind, origin))?;
        if let Value::Lit(l) = &value {
            self.charge_bytes(literal_bytes(l))?;
        }
        Ok(State::Return(self.value(value)))
    }
}
// Reuse and evidence budgets conservatively charge each retained reference.
fn decimal_storage_bytes(literal: &DecimalLiteral) -> usize {
    decimal_bytes(literal)
        .saturating_add(std::mem::size_of::<DecimalLiteral>() + 2 * std::mem::size_of::<usize>())
}
fn decimal_bytes(l: &DecimalLiteral) -> usize {
    (l.significand().bits() + l.exponent().bits()).div_ceil(8) as usize
}
fn literal_bytes(l: &Literal) -> usize {
    match l {
        Literal::String(s) => s.len(),
        Literal::Integer(n) => n.bits().div_ceil(8) as usize,
        _ => 0,
    }
}
enum StepEnd {
    Value(ValueId),
    Failure(Box<Outcome>),
}
impl From<Outcome> for StepEnd {
    fn from(o: Outcome) -> Self {
        Self::Failure(Box::new(o))
    }
}

#[cfg(test)]
mod provenance_layout_tests {
    use super::*;
    #[test]
    fn completed_provenance_does_not_grow_thunk_state() {
        #[allow(dead_code)]
        enum PreviousState {
            Pending(Pending),
            Evaluating,
            Done(Result<ValueId, RuntimeError>),
        }
        assert!(std::mem::size_of::<ThunkState>() <= std::mem::size_of::<PreviousState>());
        assert_eq!(
            std::mem::align_of::<ThunkState>(),
            std::mem::align_of::<PreviousState>()
        );
        #[allow(dead_code)]
        struct PreviousEnv {
            parent: Option<EnvId>,
            bindings: Bindings,
            activation: ActivationId,
            origin: Option<Origin>,
        }
        eprintln!(
            "Thunk bytes: {}; environment bytes: {} -> {}",
            std::mem::size_of::<Thunk>(),
            std::mem::size_of::<PreviousEnv>(),
            std::mem::size_of::<Env>()
        );
        eprintln!(
            "Frame bytes: {}; call entry bytes: {}",
            std::mem::size_of::<Frame>(),
            std::mem::size_of::<(memo::Call, (ValueId, cost::Stored))>()
        );
    }
}

#[cfg(test)]
mod cell_tests;
