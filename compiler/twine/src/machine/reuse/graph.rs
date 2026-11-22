//! Stage a bounded graph transfer before mutating the receiving heap. Worklists
//! preserve sharing/cycles without using the native stack; every executable edge
//! and lexical binder must have a checked correspondence in the new program.
use super::super::index_hash::IndexHasher;
use super::*;
use knot_core::equivalence::Correspondence;
use std::hash::BuildHasherDefault;

type IndexMap<V> = HashMap<usize, V, BuildHasherDefault<IndexHasher>>;
#[cfg(feature = "incremental")]
pub(super) mod footprint;
#[derive(Default)]
pub(super) struct Maps {
    pub thunks: IndexMap<ThunkId>,
    pub values: IndexMap<ValueId>,
    pub envs: IndexMap<EnvId>,
}
type OriginKey = (knot_core::NodeRef, u32, u32, bool);
pub(super) struct Code {
    pub mapping: Correspondence,
    origins: HashMap<OriginKey, Origin>,
    hosts: std::collections::BTreeSet<GlobalName>,
    #[cfg(feature = "incremental")]
    projection: bool,
    #[cfg(feature = "incremental")]
    pub(super) collection: Option<super::collection::History>,
    #[cfg(feature = "incremental")]
    pub(super) source: Option<super::source::Validation>,
}
impl Code {
    pub fn transferred_proof(
        &self,
        _old: &Session,
        _new: &mut Session,
        _proof: evidence::Proof,
    ) -> evidence::Proof {
        #[cfg(feature = "incremental")]
        if let Some(source) = &self.source {
            return source.import(_old, _new, &self.mapping, _proof);
        }
        #[cfg(feature = "incremental")]
        if let Some(collection) = &self.collection {
            return collection.import(_old, _new, _proof);
        }
        Default::default()
    }
    pub fn maps_global(&self, global: &Global) -> bool {
        #[cfg(feature = "incremental")]
        if self.source.is_some() {
            return true;
        }
        self.mapping.globals.contains(global)
    }
    pub fn completion_valid(&self, _old: &Session, _new: &Session, _done: &Completed) -> bool {
        #[cfg(feature = "incremental")]
        if let Some(source) = &self.source {
            return _done.introduced || source.valid(_old, _new, &self.mapping, _done.proof);
        }
        true
    }
    pub fn call_valid(&self, _old: &Session, _new: &Session, _key: &memo::Call) -> bool {
        #[cfg(feature = "incremental")]
        if let Some(source) = &self.source {
            return _old
                .memo
                .get(_key)
                .is_some_and(|(_, _, _, proof, _)| source.valid(_old, _new, &self.mapping, proof));
        }
        true
    }
    #[cfg(feature = "incremental")]
    pub fn validation_work(&self) -> u64 {
        self.source.as_ref().map_or(0, |source| source.work())
    }
    #[cfg(feature = "incremental")]
    pub fn evidence_work(&self) -> u64 {
        self.source
            .as_ref()
            .map_or(0, |source| source.import_work())
    }
    #[cfg(feature = "incremental")]
    pub fn validation_allowance(&self) -> u64 {
        self.source.as_ref().map_or(0, |source| source.allowance())
    }
    #[cfg(feature = "incremental")]
    pub fn has_source_validation(&self) -> bool {
        self.source.is_some()
    }
    #[cfg(feature = "incremental")]
    pub fn extend_source(
        &mut self,
        old: &Session,
        new: &Session,
        versions: &knot_core::equivalence::BodyVersions<'_>,
        steps: usize,
    ) {
        if !versions.accepts(&old.program, &new.program) || steps == 0 {
            return;
        }
        let mut expressions = self.mapping.expressions.clone();
        let mut locals = self.mapping.locals.clone();
        for (a, b) in versions.expressions() {
            if expressions
                .insert(a, b)
                .is_some_and(|previous| previous != b)
            {
                return;
            }
        }
        for (a, b) in versions.locals() {
            if locals.insert(a, b).is_some_and(|previous| previous != b) {
                return;
            }
        }
        if expressions.len() == self.mapping.expressions.len() && !old.validation.changed() {
            return;
        }
        // Do not conflate source uses merely because executable bodies match.
        if expressions
            .values()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != expressions.len()
            || locals
                .values()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != locals.len()
        {
            return;
        }
        let mapping = Correspondence {
            globals: self
                .mapping
                .globals
                .iter()
                .chain(versions.globals())
                .cloned()
                .collect(),
            expressions,
            locals,
            contexts: self.mapping.contexts.clone(),
            fragments: self.mapping.fragments.clone(),
        };
        let Some(mut next) = Self::new(old, new, mapping, self.hosts.clone()) else {
            return;
        };
        next.source = Some(super::source::Validation::new(
            old,
            new,
            steps,
            self.mapping.expressions.keys().copied().collect(),
        ));
        *self = next;
    }
    pub fn new(
        old: &Session,
        new: &Session,
        mapping: Correspondence,
        hosts: std::collections::BTreeSet<GlobalName>,
    ) -> Option<Self> {
        let mut origins = HashMap::new();
        for (a, b) in mapping.contexts.iter().chain(&mapping.expressions) {
            let a = old.program.expressions[a.0].origin;
            let b = new.program.expressions[b.0].origin;
            if origins
                .insert(origin_key(a), b)
                .is_some_and(|previous| previous != b)
            {
                return None;
            }
        }
        Some(Self {
            mapping,
            origins,
            hosts,
            #[cfg(feature = "incremental")]
            projection: false,
            #[cfg(feature = "incremental")]
            collection: None,
            #[cfg(feature = "incremental")]
            source: None,
        })
    }
    fn projecting(&self) -> bool {
        #[cfg(feature = "incremental")]
        return self.projection;
        #[cfg(not(feature = "incremental"))]
        false
    }
    fn collecting(&self) -> bool {
        #[cfg(feature = "incremental")]
        return self.collection.is_some();
        #[cfg(not(feature = "incremental"))]
        false
    }
    fn same_code(&self) -> bool {
        self.projecting() || self.collecting()
    }
    #[cfg(feature = "incremental")]
    pub(super) fn collection(old: &Session, new: &Session, allowance: u64) -> Option<Self> {
        if !Arc::ptr_eq(&old.program, &new.program) || !Arc::ptr_eq(&old.hosts, &new.hosts) {
            return None;
        }
        let mut code = Self::projection();
        code.projection = false;
        code.collection = Some(super::collection::History::new(old, new, allowance));
        Some(code)
    }
    fn expression(&self, expression: ExprId) -> Option<ExprId> {
        if self.same_code() {
            Some(expression)
        } else {
            self.mapping.expressions.get(&expression).copied()
        }
    }
    fn local(&self, local: LocalId) -> Option<LocalId> {
        if self.same_code() {
            Some(local)
        } else {
            self.mapping.locals.get(&local).copied()
        }
    }
    fn origin(&self, origin: Origin) -> Option<Origin> {
        if self.same_code() {
            Some(origin)
        } else {
            self.origins.get(&origin_key(origin)).copied()
        }
    }
    #[cfg(feature = "incremental")]
    fn projection() -> Self {
        Self {
            mapping: Correspondence {
                globals: Default::default(),
                expressions: Default::default(),
                locals: Default::default(),
                contexts: Default::default(),
                fragments: Default::default(),
            },
            origins: Default::default(),
            hosts: Default::default(),
            projection: true,
            collection: None,
            source: None,
        }
    }
}
fn origin_key(o: Origin) -> OriginKey {
    (
        o.node,
        o.span.start,
        o.span.end,
        o.role == knot_core::Role::Primary,
    )
}
enum Work {
    Thunk(usize, usize),
    Value(usize, usize),
    Env(usize, usize),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum Root {
    Value(ValueId),
    Thunk(ThunkId),
    #[cfg(feature = "incremental")]
    Env(EnvId),
}
pub(super) struct Import {
    pub root: Root,
    thunks: Vec<Thunk>,
    values: Vec<Value>,
    envs: Vec<Env>,
    maps: Maps,
    pub bytes: usize,
    host_bytes: usize,
    #[cfg(feature = "incremental")]
    proofs: Vec<(usize, evidence::Proof)>,
    #[cfg(feature = "incremental")]
    recoveries: Vec<(CellId, Pending)>,
}
impl Import {
    #[allow(unused_mut)]
    pub fn commit_with_proofs(
        mut self,
        new: &mut Session,
        maps: &mut Maps,
        _old: &Session,
        _code: &Code,
    ) {
        #[cfg(feature = "incremental")]
        let proofs = std::mem::take(&mut self.proofs);
        self.commit(new, maps);
        #[cfg(feature = "incremental")]
        for (cell, proof) in proofs {
            let proof = _code.transferred_proof(_old, new, proof);
            let ThunkState::Done(Ok(done)) = &mut new.thunks[cell].state else {
                unreachable!("staged completion")
            };
            done.proof = proof;
        }
    }
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.thunks.len(), self.values.len(), self.envs.len())
    }
    pub fn entries(&self) -> usize {
        self.thunks.len() + self.values.len() + self.envs.len()
    }
    pub fn commit(self, new: &mut Session, maps: &mut Maps) {
        #[cfg(feature = "incremental")]
        for (cell, recipe) in self.recoveries {
            new.validation.retain_recovery(cell, recipe);
        }
        new.stats.host_bytes += self.host_bytes;
        new.stats.activations += self.envs.len() as u64;
        new.thunks.extend(self.thunks);
        new.values.extend(self.values);
        new.envs.extend(self.envs);
        maps.thunks.extend(self.maps.thunks);
        maps.values.extend(self.maps.values);
        maps.envs.extend(self.maps.envs);
    }
}
pub(super) struct Budget {
    pub entries: usize,
    pub bytes: usize,
}
struct Builder<'a, const DESCRIBE: bool = false> {
    old: &'a Session,
    new: &'a Session,
    code: &'a Code,
    existing: &'a Maps,
    maps: Maps,
    pending: Vec<Work>,
    thunks: Vec<Option<Thunk>>,
    values: Vec<Option<Value>>,
    envs: Vec<Option<Env>>,
    entries_left: usize,
    budget: &'a mut Budget,
    bytes: usize,
    host_bytes: usize,
    #[cfg(feature = "incremental")]
    proofs: Vec<(usize, evidence::Proof)>,
    #[cfg(feature = "incremental")]
    recoveries: Vec<(CellId, Pending)>,
    #[cfg(feature = "incremental")]
    describing: Option<Vec<Root>>,
}
pub(super) fn transfer(
    old: &Session,
    new: &Session,
    code: &Code,
    existing: &Maps,
    root: Root,
    budget: &mut Budget,
) -> Option<Import> {
    transfer_roots(old, new, code, existing, &[root], budget)
}
pub(super) fn transfer_roots(
    old: &Session,
    new: &Session,
    code: &Code,
    existing: &Maps,
    roots: &[Root],
    budget: &mut Budget,
) -> Option<Import> {
    let b = Builder {
        old,
        new,
        code,
        existing,
        maps: Maps::default(),
        pending: vec![],
        thunks: vec![],
        values: vec![],
        envs: vec![],
        entries_left: new.limits.heap_entries.saturating_sub(new.heap_entries()),
        budget,
        bytes: 0,
        host_bytes: 0,
        #[cfg(feature = "incremental")]
        proofs: vec![],
        #[cfg(feature = "incremental")]
        recoveries: vec![],
        #[cfg(feature = "incremental")]
        describing: None,
    };
    finish_roots(b, roots)
}
fn finish_roots(mut b: Builder<'_>, roots: &[Root]) -> Option<Import> {
    let mut first = None;
    for root in roots {
        let mapped = match *root {
            Root::Value(v) => Root::Value(b.value(v)?),
            Root::Thunk(t) => Root::Thunk(b.thunk(t)?),
            #[cfg(feature = "incremental")]
            Root::Env(env) => Root::Env(b.env(env)?),
        };
        first.get_or_insert(mapped);
    }
    let root = first?;
    while let Some(work) = b.pending.pop() {
        if b.new.limits.cancellation.load(Ordering::Relaxed) {
            return None;
        }
        b.copy(work)?;
    }
    Some(Import {
        root,
        thunks: b.thunks.into_iter().collect::<Option<_>>()?,
        values: b.values.into_iter().collect::<Option<_>>()?,
        envs: b.envs.into_iter().collect::<Option<_>>()?,
        maps: b.maps,
        bytes: b.bytes,
        host_bytes: b.host_bytes,
        #[cfg(feature = "incremental")]
        proofs: b.proofs,
        #[cfg(feature = "incremental")]
        recoveries: b.recoveries,
    })
}
/// Project a validated same-session call result. Scalar/native leaves have no
/// lexical edges. Other results require the original body activation and a staged
/// receiving activation; no borrowed source context is silently discarded.
#[cfg(feature = "incremental")]
pub(super) fn project(
    session: &Session,
    existing: &Maps,
    root: ValueId,
    scope: Option<(EnvId, Env)>,
    budget: &mut Budget,
) -> Option<Import> {
    if !session.values[root].needs_call_scope() {
        return Some(Import {
            root: Root::Value(root),
            thunks: vec![],
            values: vec![],
            envs: vec![],
            maps: Maps::default(),
            bytes: 0,
            host_bytes: 0,
            proofs: vec![],
            #[cfg(feature = "incremental")]
            recoveries: vec![],
        });
    }
    let (original, receiving) = scope?;
    let code = Code::projection();
    let mut b = Builder {
        old: session,
        new: session,
        code: &code,
        existing,
        maps: Maps::default(),
        pending: vec![],
        thunks: vec![],
        values: vec![],
        envs: vec![],
        entries_left: session
            .limits
            .heap_entries
            .saturating_sub(session.heap_entries()),
        budget,
        bytes: 0,
        host_bytes: 0,
        proofs: vec![],
        #[cfg(feature = "incremental")]
        recoveries: vec![],
        #[cfg(feature = "incremental")]
        describing: None,
    };
    b.allocate()?;
    b.maps.envs.insert(original, session.envs.len());
    b.envs.push(Some(receiving));
    finish_roots(b, &[Root::Value(root)])
}
fn object_bytes() -> usize {
    std::mem::size_of::<Thunk>()
        .max(std::mem::size_of::<Value>())
        .max(std::mem::size_of::<Env>())
        .saturating_mul(2)
        .saturating_add(256)
}
impl<const DESCRIBE: bool> Builder<'_, DESCRIBE> {
    fn allocate(&mut self) -> Option<()> {
        if self.code.collecting() {
            // Bound both the staged and retained object, worklist/mapping slots,
            // Arc/page overhead and the returned inheritance map. Payload edges
            // are charged separately below; no reachability scan is unbudgeted.
            self.payload(object_bytes())?;
        }
        self.budget.entries = self.budget.entries.checked_sub(1)?;
        self.entries_left = self.entries_left.checked_sub(1)?;
        Some(())
    }
    fn payload(&mut self, bytes: usize) -> Option<()> {
        self.budget.bytes = self.budget.bytes.checked_sub(bytes)?;
        self.bytes += bytes;
        Some(())
    }
    fn thunk(&mut self, old: ThunkId) -> Option<ThunkId> {
        #[cfg(feature = "incremental")]
        if DESCRIBE {
            self.describe_edge(Root::Thunk(old))?;
            return Some(old);
        }
        if let Some(id) = self
            .existing
            .thunks
            .get(&old.0)
            .or_else(|| self.maps.thunks.get(&old.0))
        {
            return Some(*id);
        }
        if self.code.projecting() && self.old.thunks[old.0].activation == ActivationId(0) {
            return Some(old);
        }
        self.allocate()?;
        let index = self.thunks.len();
        let id = ThunkId(self.new.thunks.len() + index);
        self.maps.thunks.insert(old.0, id);
        self.thunks.push(None);
        self.pending.push(Work::Thunk(old.0, index));
        Some(id)
    }
    fn value(&mut self, old: ValueId) -> Option<ValueId> {
        #[cfg(feature = "incremental")]
        if DESCRIBE {
            self.describe_edge(Root::Value(old))?;
            return Some(old);
        }
        if let Some(id) = self
            .existing
            .values
            .get(&old)
            .or_else(|| self.maps.values.get(&old))
        {
            return Some(*id);
        }
        if self.code.projecting()
            && matches!(self.old.values[old], Value::Lit(_) | Value::Native(_))
        {
            return Some(old);
        }
        self.allocate()?;
        let index = self.values.len();
        let id = self.new.values.len() + index;
        self.maps.values.insert(old, id);
        self.values.push(None);
        self.pending.push(Work::Value(old, index));
        Some(id)
    }
    fn env(&mut self, old: EnvId) -> Option<EnvId> {
        #[cfg(feature = "incremental")]
        if DESCRIBE {
            self.describe_edge(Root::Env(old))?;
            return Some(old);
        }
        if let Some(id) = self
            .existing
            .envs
            .get(&old)
            .or_else(|| self.maps.envs.get(&old))
        {
            return Some(*id);
        }
        if self.code.projecting() && old == 0 {
            return Some(0);
        }
        self.allocate()?;
        let index = self.envs.len();
        let id = self.new.envs.len() + index;
        self.maps.envs.insert(old, id);
        self.envs.push(None);
        self.pending.push(Work::Env(old, index));
        Some(id)
    }
    fn activation(&mut self, old: ActivationId) -> Option<ActivationId> {
        let index = usize::try_from(old.0).ok()?;
        // Activations are allocated with environments, including transferred
        // environments. Reject instead of assuming this if the invariant changes.
        if self.old.envs.get(index)?.activation != old {
            return None;
        }
        let env = self.env(index)?;
        #[cfg(feature = "incremental")]
        if DESCRIBE {
            return Some(old);
        }
        Some(if env < self.new.envs.len() {
            self.new.envs[env].activation
        } else {
            ActivationId(self.new.stats.activations + (env - self.new.envs.len()) as u64)
        })
    }
    fn thunks(&mut self, old: &[ThunkId]) -> Option<Vec<ThunkId>> {
        self.payload(old.len().checked_mul(std::mem::size_of::<ThunkId>())?)?;
        old.iter().map(|id| self.thunk(*id)).collect()
    }
    fn recipe(&mut self, p: &Pending) -> Option<Pending> {
        Some(match p {
            Pending::Expr(e, env) => Pending::Expr(self.code.expression(*e)?, self.env(*env)?),
            Pending::Apply(f, x) => Pending::Apply(self.thunk(*f)?, self.thunk(*x)?),
            Pending::Projection(t, i) => Pending::Projection(self.thunk(*t)?, *i),
            Pending::DecimalParts(n, env) => {
                self.payload(decimal_storage_bytes(n))?;
                Pending::DecimalParts(n.clone(), self.env(*env)?)
            }
            Pending::TextList {
                source,
                separator,
                offset,
                activation,
            } => {
                self.payload(
                    source
                        .len()
                        .saturating_add(separator.as_ref().map_or(0, |s| s.len())),
                )?;
                Pending::TextList {
                    source: source.clone(),
                    separator: separator.clone(),
                    offset: *offset,
                    activation: self.activation(*activation)?,
                }
            }
        })
    }
    fn cell(&mut self, cell: CellId) -> Option<CellId> {
        let projected = self.thunk(cell.owner())?;
        #[cfg(feature = "incremental")]
        if DESCRIBE {
            return Some(cell);
        }
        Some(if projected.0 < self.new.thunks.len() {
            self.new.cell_id(projected)
        } else {
            CellId(projected.0)
        })
    }
    fn copy(&mut self, work: Work) -> Option<()> {
        let old = self.old;
        match work {
            Work::Thunk(id, index) => {
                let t = old.thunks.get(id)?;
                let origin = self.code.origin(t.origin)?;
                let activation = self.activation(t.activation)?;
                let state = match &t.state {
                    ThunkState::Shared(cell) => ThunkState::Shared(self.cell(*cell)?),
                    ThunkState::Done(Ok(v))
                        if !self.code.completion_valid(self.old, self.new, v) =>
                    {
                        let recipe = match v.expression {
                            Some(expression) => Pending::Expr(expression, t.activation.0 as usize),
                            None => {
                                #[cfg(feature = "incremental")]
                                {
                                    self.old.recovery_recipe(CellId(id), v.proof)?.clone()
                                }
                                #[cfg(not(feature = "incremental"))]
                                {
                                    return None;
                                }
                            }
                        };
                        ThunkState::Pending(self.recipe(&recipe)?)
                    }
                    #[cfg(feature = "incremental")]
                    ThunkState::Done(Ok(v))
                        if (self.code.projecting() || self.code.source.is_some())
                            && !v.introduced
                            && v.expression.is_none() =>
                    {
                        let recipe = self.old.recovery_recipe(CellId(id), v.proof)?;
                        ThunkState::Pending(self.recipe(recipe)?)
                    }
                    ThunkState::Done(Ok(v)) => ThunkState::Done(Ok(Completed {
                        value: self.value(v.value)?,
                        cost: v.cost,
                        proof: if self.code.projecting() {
                            v.proof
                        } else {
                            Default::default()
                        },
                        stamp: if self.code.same_code() {
                            v.stamp
                        } else {
                            self.new.validation.stamp()
                        },
                        #[cfg(feature = "incremental")]
                        introduced: v.introduced,
                        expression: v.expression.and_then(|e| self.code.expression(e)),
                    })),
                    ThunkState::Pending(p) => ThunkState::Pending(self.recipe(p)?),
                    ThunkState::Done(Err(error)) if self.code.collecting() => {
                        let text = match &error.kind {
                            RuntimeErrorKind::Error(s)
                            | RuntimeErrorKind::InvalidNumeric(s)
                            | RuntimeErrorKind::Host(s)
                            | RuntimeErrorKind::Unsupported(s)
                            | RuntimeErrorKind::InvalidCore(s)
                            | RuntimeErrorKind::Io(s) => s.len(),
                            _ => 0,
                        };
                        self.payload(
                            (std::mem::size_of::<RuntimeError>()
                                + 2 * std::mem::size_of::<usize>())
                            .saturating_add(text)
                            .saturating_add(
                                error
                                    .trace
                                    .len()
                                    .saturating_mul(std::mem::size_of::<Origin>()),
                            ),
                        )?;
                        ThunkState::Done(Err(error.clone()))
                    }
                    ThunkState::Done(Err(_)) | ThunkState::Evaluating(_) => return None,
                };
                #[cfg(feature = "incremental")]
                if (self.code.source.is_some() || self.code.collecting())
                    && matches!(state, ThunkState::Done(Ok(_)))
                {
                    let ThunkState::Done(Ok(previous)) = &t.state else {
                        unreachable!("completed source")
                    };
                    self.payload(std::mem::size_of::<(usize, evidence::Proof)>())?;
                    self.proofs
                        .push((self.new.thunks.len() + index, previous.proof));
                }
                #[cfg(feature = "incremental")]
                if self.code.collecting() {
                    if let ThunkState::Done(Ok(done)) = &t.state {
                        if !done.introduced && done.expression.is_none() {
                            if let Some(recipe) = self.old.recovery_recipe(CellId(id), done.proof) {
                                self.payload(recipe.recovery_bytes())?;
                                let recipe = self.recipe(recipe)?;
                                self.recoveries
                                    .push((CellId(self.new.thunks.len() + index), recipe));
                            } else if !self.old.validation.current(done.stamp) {
                                return None;
                            }
                        }
                    }
                }
                self.thunks[index] = Some(Thunk {
                    origin,
                    activation,
                    state,
                });
            }
            Work::Value(id, index) => {
                let source = old.values.get(id)?;
                if self.code.collecting() {
                    let fields = match source {
                        Value::Record(fields) => fields.len(),
                        Value::Dict(fields) => fields.len(),
                        _ => 0,
                    };
                    self.payload(fields.saturating_mul(96))?;
                }
                match source {
                    Value::Ctor(n, _) | Value::DecimalRatio(n, _, _) => self
                        .payload(n.module.iter().map(String::len).sum::<usize>() + n.name.len())?,
                    Value::Record(fs) => self.payload(
                        fs.keys()
                            .map(|k| k.len() + std::mem::size_of::<ThunkId>())
                            .sum(),
                    )?,
                    Value::Dict(fs) => self.payload(
                        fs.keys()
                            .map(|k| format!("{k:?}").len() + std::mem::size_of::<ThunkId>())
                            .sum(),
                    )?,
                    Value::Partial(Callable::Host(n, _), _) => self
                        .payload(n.module.iter().map(String::len).sum::<usize>() + n.name.len())?,
                    Value::Partial(Callable::Primitive(n), _) => self.payload(n.len())?,
                    Value::Partial(Callable::Constructor(n, _), _) => self
                        .payload(n.module.iter().map(String::len).sum::<usize>() + n.name.len())?,
                    _ => {}
                }
                let value = match source {
                    Value::Lit(l) => {
                        self.payload(literal_bytes(l))?;
                        Value::Lit(l.clone())
                    }
                    Value::Ctor(n, ts) => Value::Ctor(n.clone(), self.thunks(ts)?),
                    Value::DecimalRatio(n, d, ts) => {
                        self.payload(decimal_storage_bytes(d))?;
                        Value::DecimalRatio(n.clone(), d.clone(), self.thunks(ts)?)
                    }
                    Value::Tuple(ts) => Value::Tuple(self.thunks(ts)?),
                    Value::Record(fs) => Value::Record(
                        fs.iter()
                            .map(|(f, t)| Some((f.clone(), self.thunk(*t)?)))
                            .collect::<Option<_>>()?,
                    ),
                    Value::Dict(fs) => Value::Dict(
                        fs.iter()
                            .map(|(f, t)| Some((f.clone(), self.thunk(*t)?)))
                            .collect::<Option<_>>()?,
                    ),
                    Value::Closure(param, body, env) => Value::Closure(
                        self.code.local(*param)?,
                        self.code.expression(*body)?,
                        closure::Capture {
                            scope: self.env(env.scope)?,
                            // Transfer the complete inspection scope and use lexical
                            // execution lookup. Never copy session-local capture handles.
                            #[cfg(feature = "compact-captures")]
                            inputs: None,
                            #[cfg(feature = "incremental")]
                            identity: None,
                        },
                    ),
                    Value::Partial(c, ts) if !matches!(c, Callable::Host(..)) => {
                        Value::Partial(c.clone(), self.thunks(ts)?)
                    }
                    Value::Native(value) => {
                        if !self.new.hosts.accepts_snapshot(value) {
                            return None;
                        }
                        let bytes = value.retained_bytes();
                        let total = self.host_bytes.checked_add(bytes)?;
                        if total
                            > self
                                .new
                                .limits
                                .host_bytes
                                .saturating_sub(self.new.stats.host_bytes)
                        {
                            return None;
                        }
                        let snapshot = value.snapshot_id()?;
                        self.payload(bytes)?;
                        self.payload(
                            value
                                .type_name()
                                .module
                                .iter()
                                .map(String::len)
                                .sum::<usize>()
                                + value.type_name().name.len()
                                + snapshot.namespace.len()
                                + snapshot.version.len(),
                        )?;
                        self.host_bytes = total;
                        Value::Native(value.clone())
                    }
                    Value::Partial(Callable::Host(name, arity), ts) => {
                        if !self.code.same_code() && !self.code.hosts.contains(name) {
                            return None;
                        }
                        Value::Partial(Callable::Host(name.clone(), *arity), self.thunks(ts)?)
                    }
                    Value::Io(..) => return None,
                    Value::Partial(..) => unreachable!(),
                };
                self.values[index] = Some(value);
            }
            Work::Env(id, index) => {
                let e = old.envs.get(id)?;
                let parent = match e.parent {
                    Some(p) => Some(self.env(p)?),
                    None => None,
                };
                let origin = match e.origin {
                    Some(o) => Some(self.code.origin(o)?),
                    None => None,
                };
                let mut bindings = Bindings::Empty;
                if self.code.collecting() {
                    self.payload(e.bindings.len().saturating_mul(64))?;
                }
                self.payload(
                    e.bindings
                        .iter()
                        .count()
                        .checked_mul(std::mem::size_of::<(LocalId, ThunkId)>())?,
                )?;
                for (local, thunk) in e.bindings.iter() {
                    bindings.insert(self.code.local(*local)?, self.thunk(*thunk)?);
                }
                self.envs[index] = Some(Env {
                    empty: e.empty,
                    parent,
                    bindings,
                    origin,
                    activation: ActivationId(self.new.stats.activations + index as u64),
                });
            }
        }
        Some(())
    }
}
