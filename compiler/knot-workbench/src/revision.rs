//! In-memory edit identities and exact semantic fingerprints. No runtime values
//! are cached here. Fingerprints retain canonical bytes (not a collision-prone
//! hash); a future cache can add a digest index while still checking equality.
use knot_canonical::ast::{CDecl, CType, Ref};
use knot_checker::{
    program::TypedProgram,
    ty::{Scheme, Structure},
    var::{Substitution, TypeVarId},
};
use knot_core::{ExprId, Global, GlobalName, Kind, LocalId, NodeRef, Pattern, Program};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fingerprint(Arc<[u8]>);
impl std::fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fingerprint({} bytes)", self.0.len())
    }
}
impl Fingerprint {
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableId {
    pub lineage: u64,
    pub serial: u64,
}
#[derive(Debug, Clone)]
pub struct DefinitionIdentity {
    pub id: StableId,
    pub code: Fingerprint,
    pub abi: Option<Fingerprint>,
}
#[derive(Debug, Clone)]
pub struct NodeIdentity {
    pub id: StableId,
    pub owner: StableId,
    pub shape: Fingerprint,
}
#[derive(Debug, Clone)]
pub struct RevisionSnapshot {
    pub revision: u64,
    pub definitions: BTreeMap<Global, DefinitionIdentity>,
    /// Lowered source nodes; ambiguous synthetic origins shared by multiple
    /// generated globals are intentionally omitted.
    pub nodes: HashMap<NodeRef, NodeIdentity>,
    pub type_abi: Fingerprint,
    pub compiler: &'static str,
    owners: Vec<Option<Global>>,
    origins: Vec<NodeRef>,
}
impl RevisionSnapshot {
    /// Exact compatibility context for Twine revision overlays. Source spans,
    /// body edits and revision numbers are excluded; nominal shapes, all checked
    /// schemes and the compiler/bundled-library stamp are included. Runtime code
    /// correspondence and host version checks are still required for every reuse.
    pub fn runtime_context(&self, max_bytes: usize) -> Result<Arc<[u8]>, String> {
        let schemes: Vec<_> = self
            .definitions
            .iter()
            .map(|(global, definition)| (global, definition.abi.as_ref().map(Fingerprint::bytes)))
            .collect();
        encoded(&(self.compiler, self.type_abi.bytes(), schemes), &mut {
            max_bytes
        })
        .map(|fingerprint| fingerprint.0)
    }

    pub fn owner(&self, expr: ExprId) -> Option<&Global> {
        self.owners.get(expr.0)?.as_ref()
    }
}
pub struct RevisionTracker {
    lineage: u64,
    next: u64,
    revision: u64,
    previous: Option<RevisionSnapshot>,
    /// Limit retained canonical bytes during construction, including node shapes.
    pub fingerprint_bytes: usize,
}
impl Default for RevisionTracker {
    fn default() -> Self {
        Self::new()
    }
}
impl RevisionTracker {
    pub fn new() -> Self {
        static LINEAGE: AtomicU64 = AtomicU64::new(1);
        Self {
            lineage: LINEAGE.fetch_add(1, Ordering::Relaxed),
            next: 0,
            revision: 0,
            previous: None,
            fingerprint_bytes: 64 * 1024 * 1024,
        }
    }
    /// Supply all user typed modules corresponding to this linked core. Bundled
    /// library ABI is covered by the compiler/library build stamp. Capturing a
    /// new revision does not execute anything or transfer any runtime handles.
    pub fn capture(
        &mut self,
        program: &Program,
        modules: &[&TypedProgram],
    ) -> Result<RevisionSnapshot, String> {
        check_modules(program, modules)?;
        let mut remaining = self.fingerprint_bytes;
        let (abis, type_abi) = abi(modules, &mut remaining)?;
        let mut definitions = BTreeMap::new();
        let mut owners = vec![None; program.expressions.len()];
        let mut node_shapes: HashMap<NodeRef, (Global, Fingerprint)> = HashMap::new();
        let mut ambiguous_nodes = std::collections::HashSet::new();
        let mut next = self.next;
        let mut fresh = || {
            let id = StableId {
                lineage: self.lineage,
                serial: next,
            };
            next += 1;
            id
        };
        for (name, root) in &program.globals {
            let mut locals = HashMap::new();
            let (code, visited) = normalized(program, *root, &mut locals, &mut remaining)?;
            for expr in &visited {
                if owners[expr.0].as_ref().is_some_and(|n| n != name) {
                    return Err("core expression belongs to multiple globals".into());
                }
                owners[expr.0] = Some(name.clone());
            }
            // Prefer the primary expression for a source node. Generated wrappers
            // with the same origin are implementation details of its declaration.
            let mut roots = HashMap::new();
            for expr in visited {
                let origin = program.expressions[expr.0].origin;
                let entry = roots.entry(origin.node).or_insert((expr, false));
                if origin.role == knot_core::Role::Primary {
                    *entry = (expr, true);
                }
            }
            for (node, (root, _)) in roots {
                if node_shapes
                    .get(&node)
                    .is_some_and(|(owner, _)| owner != name)
                {
                    ambiguous_nodes.insert(node);
                    continue;
                }
                let (shape, _) = normalized(program, root, &mut locals.clone(), &mut remaining)?;
                node_shapes.insert(node, (name.clone(), shape));
            }
            definitions.insert(
                name.clone(),
                DefinitionIdentity {
                    id: fresh(),
                    code,
                    abi: abis.get(name).cloned(),
                },
            );
        }
        // Qualified names identify unambiguous edit candidates, not the IDs:
        // IDs are allocated by this tracker and survive body changes/unique renames.
        let mut old_unmatched = BTreeMap::new();
        let mut new_unmatched = BTreeMap::new();
        if let Some(old) = &self.previous {
            for (name, definition) in &mut definitions {
                if let Some(previous) = old.definitions.get(name) {
                    definition.id = previous.id;
                } else if let Global::Value(value) = name {
                    new_unmatched
                        .entry((
                            value.module.clone(),
                            definition.code.clone(),
                            definition.abi.clone(),
                        ))
                        .or_insert_with(Vec::new)
                        .push(name.clone());
                }
            }
            for (name, definition) in &old.definitions {
                if !definitions.contains_key(name) {
                    if let Global::Value(value) = name {
                        old_unmatched
                            .entry((
                                value.module.clone(),
                                definition.code.clone(),
                                definition.abi.clone(),
                            ))
                            .or_insert_with(Vec::new)
                            .push(definition.id);
                    }
                }
            }
            for (key, names) in new_unmatched {
                if let Some(ids) = old_unmatched.get(&key) {
                    if ids.len() == 1 && names.len() == 1 {
                        definitions.get_mut(&names[0]).unwrap().id = ids[0];
                    }
                }
            }
        }
        let mut old_nodes: BTreeMap<_, Vec<_>> = BTreeMap::new();
        if let Some(old) = &self.previous {
            for node in old.nodes.values() {
                old_nodes
                    .entry((node.owner, node.shape.clone()))
                    .or_default()
                    .push(node.id);
            }
        }
        let mut groups: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for (node, (owner, shape)) in node_shapes {
            if !ambiguous_nodes.contains(&node) {
                groups
                    .entry((definitions[&owner].id, shape))
                    .or_default()
                    .push(node);
            }
        }
        let mut nodes = HashMap::new();
        for ((owner, shape), refs) in groups {
            let prior = old_nodes.get(&(owner, shape.clone()));
            let inherited = if refs.len() == 1 {
                prior.filter(|ids| ids.len() == 1).map(|ids| ids[0])
            } else {
                None
            };
            for node in refs {
                nodes.insert(
                    node,
                    NodeIdentity {
                        id: inherited.unwrap_or_else(&mut fresh),
                        owner,
                        shape: shape.clone(),
                    },
                );
            }
        }
        let snapshot = RevisionSnapshot {
            revision: self.revision,
            definitions,
            nodes,
            type_abi,
            compiler: env!("KNOT_CORE_CACHE_BUILD_ID"),
            origins: program.expressions.iter().map(|e| e.origin.node).collect(),
            owners,
        };
        self.next = next;
        self.revision += 1;
        self.previous = Some(snapshot.clone());
        Ok(snapshot)
    }
}
fn check_modules(program: &Program, modules: &[&TypedProgram]) -> Result<(), String> {
    let library_revisions: std::collections::HashSet<_> = program
        .globals
        .iter()
        .filter_map(|(g, e)| {
            if let Global::Value(n) = g {
                if knot_canonical::prelude::LIBRARY
                    .iter()
                    .any(|(name, _)| n.module.join(".") == *name)
                {
                    return program.expressions.get(e.0).map(|e| e.origin.node.revision);
                }
            }
            None
        })
        .collect();
    for expr in &program.expressions {
        if !library_revisions.contains(&expr.origin.node.revision)
            && !modules
                .iter()
                .any(|p| p.revision == expr.origin.node.revision)
        {
            return Err("missing matching user typed module for revision metadata".into());
        }
    }
    Ok(())
}

/// Compatibility metadata for an exact prepared executable. All checked schemes
/// are retained, including factory parameters whose globals were lowered away.
/// Code-body identity and demanded dependencies remain separate runtime checks.
#[cfg(feature = "incremental")]
pub(crate) fn runtime_context_for(
    program: &Program,
    modules: &[&TypedProgram],
    max_bytes: usize,
) -> Result<Arc<[u8]>, String> {
    check_modules(program, modules)?;
    let mut remaining = max_bytes;
    let (schemes, types) = abi(modules, &mut remaining)?;
    let schemes = schemes
        .iter()
        .map(|(name, scheme)| (name, scheme.bytes()))
        .collect::<Vec<_>>();
    let globals = program.globals.keys().collect::<Vec<_>>();
    encoded(
        &(
            "prepared-source-context-v1",
            env!("KNOT_CORE_CACHE_BUILD_ID"),
            types.bytes(),
            schemes,
            globals,
        ),
        &mut remaining,
    )
    .map(|context| context.0)
}

fn encoded(value: &impl Serialize, remaining: &mut usize) -> Result<Fingerprint, String> {
    let size = bincode::serialized_size(value).map_err(|e| e.to_string())? as usize;
    if size > *remaining {
        return Err("revision fingerprint byte budget exceeded".into());
    }
    *remaining -= size;
    Ok(Fingerprint(
        bincode::serialize(value).map_err(|e| e.to_string())?.into(),
    ))
}
fn normalized(
    program: &Program,
    root: ExprId,
    locals: &mut HashMap<LocalId, LocalId>,
    remaining: &mut usize,
) -> Result<(Fingerprint, Vec<ExprId>), String> {
    let mut todo = vec![root];
    let mut indices = HashMap::from([(root, ExprId(0))]);
    let mut kinds = vec![];
    let mut cursor = 0;
    while cursor < todo.len() {
        let mut kind = program
            .expressions
            .get(todo[cursor].0)
            .ok_or("invalid core edge")?
            .kind
            .clone();
        cursor += 1;
        let mut edge = |e: &mut ExprId| {
            *e = *indices.entry(*e).or_insert_with(|| {
                let id = ExprId(todo.len());
                todo.push(*e);
                id
            });
        };
        let mut local = |l: &mut LocalId| {
            let next = LocalId(locals.len());
            *l = *locals.entry(*l).or_insert(next);
        };
        match &mut kind {
            Kind::Local(l) => local(l),
            Kind::Lam(l, e) => {
                local(l);
                edge(e);
            }
            Kind::Select(e, _) => edge(e),
            Kind::App(f, args) => {
                edge(f);
                for e in args {
                    edge(e);
                }
            }
            Kind::Let(bindings, body) => {
                for (l, e) in bindings {
                    local(l);
                    edge(e);
                }
                edge(body);
            }
            Kind::Case(scrutinee, arms) => {
                edge(scrutinee);
                for (p, e) in arms {
                    pattern_locals(p, &mut local);
                    edge(e);
                }
            }
            Kind::Tuple(es) => {
                for e in es {
                    edge(e);
                }
            }
            Kind::Record(fs) => {
                for (_, e) in fs {
                    edge(e);
                }
            }
            Kind::RecordUpdate(e, fs) => {
                edge(e);
                for (_, e) in fs {
                    edge(e);
                }
            }
            Kind::Dict(fs) => {
                for (_, e) in fs {
                    edge(e);
                }
            }
            _ => {}
        }
        kinds.push(kind);
        // Reject oversized graphs before temporary normalization can grow without
        // bound. The exact serialized byte check below remains authoritative.
        if kinds.len() > remaining.saturating_add(1) {
            return Err("revision fingerprint byte budget exceeded".into());
        }
    }
    Ok((encoded(&kinds, remaining)?, todo))
}
fn pattern_locals(p: &mut Pattern, local: &mut impl FnMut(&mut LocalId)) {
    match p {
        Pattern::Bind(l) => local(l),
        Pattern::As(l, p) => {
            local(l);
            pattern_locals(p, local);
        }
        Pattern::Ctor(_, ps) | Pattern::Tuple(ps) => {
            for p in ps {
                pattern_locals(p, local);
            }
        }
        Pattern::Record(fs) => {
            for (_, l) in fs {
                local(l);
            }
        }
        _ => {}
    }
}
fn qualified(r: &Ref, module: &[String]) -> GlobalName {
    match r {
        Ref::Builtin(name) => GlobalName {
            module: vec!["Prelude".into()],
            name: name.clone(),
        },
        Ref::Imported { module, name } => GlobalName {
            module: module.clone(),
            name: name.clone(),
        },
        Ref::TopLevel(name) => GlobalName {
            module: module.to_vec(),
            name: name.clone(),
        },
        _ => panic!("unresolved reference in checked type"),
    }
}
#[derive(Serialize)]
enum AbiType {
    Var,
    App(GlobalName, Vec<usize>),
    Ctor(GlobalName, Vec<usize>),
    Fn(usize, usize),
    Tuple(Vec<usize>),
    Predicate(Vec<usize>),
    Record(BTreeMap<String, usize>, Option<usize>),
    Unit,
    VarApp(usize, Vec<usize>),
}
fn scheme_key(
    p: &TypedProgram,
    sub: &mut Substitution,
    scheme: &Scheme,
    remaining: &mut usize,
) -> Result<Fingerprint, String> {
    fn copy(
        sub: &mut Substitution,
        id: TypeVarId,
        module: &[String],
        seen: &mut HashMap<TypeVarId, usize>,
        out: &mut Vec<AbiType>,
    ) -> usize {
        let id = sub.find(id);
        if let Some(index) = seen.get(&id) {
            return *index;
        }
        let index = out.len();
        seen.insert(id, index);
        out.push(AbiType::Var);
        if let Some(ty) = sub.resolve_structure(id) {
            let mut child = |v| copy(sub, v, module, seen, out);
            let ty = match ty {
                Structure::App(r, args) => AbiType::App(
                    qualified(&r, module),
                    args.into_iter().map(&mut child).collect(),
                ),
                Structure::Ctor(r, args) => AbiType::Ctor(
                    qualified(&r, module),
                    args.into_iter().map(&mut child).collect(),
                ),
                Structure::Fn(a, b) => AbiType::Fn(child(a), child(b)),
                Structure::Predicate(ts) => {
                    AbiType::Predicate(ts.into_iter().map(&mut child).collect())
                }
                Structure::Tuple(ts) => AbiType::Tuple(ts.into_iter().map(&mut child).collect()),
                Structure::Record(fs, e) => AbiType::Record(
                    fs.into_iter().map(|(n, t)| (n, child(t))).collect(),
                    e.map(&mut child),
                ),
                Structure::VarApp(f, args) => {
                    AbiType::VarApp(child(f), args.into_iter().map(&mut child).collect())
                }
                Structure::Unit => AbiType::Unit,
            };
            out[index] = ty;
        }
        index
    }
    let mut seen = HashMap::new();
    let mut out = vec![];
    let root = copy(sub, scheme.ty, &p.canonical.name, &mut seen, &mut out);
    let vars: Vec<_> = scheme
        .vars
        .iter()
        .map(|v| copy(sub, *v, &p.canonical.name, &mut seen, &mut out))
        .collect();
    let constraints: Vec<_> = scheme
        .constraints
        .iter()
        .map(|(v, c)| (copy(sub, *v, &p.canonical.name, &mut seen, &mut out), c))
        .collect();
    encoded(&(root, vars, constraints, out), remaining)
}
fn canonical_type(t: &CType, module: &[String]) -> String {
    match t {
        CType::Named(r, args) => format!(
            "Named({:?}, {:?})",
            qualified(r, module),
            args.iter()
                .map(|t| canonical_type(t, module))
                .collect::<Vec<_>>()
        ),
        CType::Var(n) => format!("Var({n:?})"),
        CType::VarApp(n, args) => format!(
            "VarApp({n:?},{:?})",
            args.iter()
                .map(|t| canonical_type(t, module))
                .collect::<Vec<_>>()
        ),
        CType::Fn(a, b) => format!(
            "Fn({},{})",
            canonical_type(a, module),
            canonical_type(b, module)
        ),
        CType::Tuple(ts) => format!(
            "Tuple({:?})",
            ts.iter()
                .map(|t| canonical_type(t, module))
                .collect::<Vec<_>>()
        ),
        CType::Record(fs, ext) => format!(
            "Record({:?},{ext:?})",
            fs.iter()
                .map(|(n, t)| (n, canonical_type(t, module)))
                .collect::<Vec<_>>()
        ),
        CType::Unit => "Unit".into(),
    }
}
fn abi(
    modules: &[&TypedProgram],
    remaining: &mut usize,
) -> Result<(BTreeMap<Global, Fingerprint>, Fingerprint), String> {
    let mut signatures = BTreeMap::new();
    let mut types = BTreeMap::new();
    for p in modules {
        // Type reads only path-compress this scratch arena. Copy it once per
        // module, rather than copying every slot for every exported binding.
        let mut sub = p.substitution.clone();
        for binding in p.bindings.values() {
            if let Some(name) = &binding.global {
                signatures.insert(
                    Global::Value(name.clone()),
                    scheme_key(p, &mut sub, &binding.scheme, remaining)?,
                );
            }
        }
        for instance in &p.instances {
            let scheme = Scheme {
                ty: instance.target,
                vars: vec![],
                constraints: instance
                    .context
                    .iter()
                    .map(|c| (c.ty, c.interface.clone()))
                    .collect(),
            };
            signatures.insert(
                Global::Instance(instance.interface.clone(), instance.head.clone()),
                scheme_key(p, &mut sub, &scheme, remaining)?,
            );
        }
        for decl in &p.canonical.decls {
            let (name, shape) = match &decl.node {
                CDecl::TypeDecl(name, params, variants, _) => (
                    name,
                    format!(
                        "ADT({params:?},{:?})",
                        variants
                            .iter()
                            .map(|(n, fs)| (
                                n,
                                fs.iter()
                                    .map(|t| canonical_type(t, &p.canonical.name))
                                    .collect::<Vec<_>>()
                            ))
                            .collect::<Vec<_>>()
                    ),
                ),
                CDecl::TypeAlias(name, params, body) => (
                    name,
                    format!(
                        "Alias({params:?},{})",
                        canonical_type(body, &p.canonical.name)
                    ),
                ),
                _ => continue,
            };
            types.insert(
                GlobalName {
                    module: p.canonical.name.clone(),
                    name: name.clone(),
                },
                shape,
            );
        }
        for name in &p.hosts.types {
            types.insert(name.clone(), "Native".into());
        }
    }
    Ok((signatures, encoded(&types, remaining)?))
}

#[derive(Debug, Default)]
pub struct ChangeAssessment {
    pub changed_definitions: BTreeSet<StableId>,
    pub changed_hosts: BTreeSet<GlobalName>,
    pub type_abi_changed: bool,
    pub compiler_changed: bool,
    pub incomplete: bool,
    pub unversioned_input: bool,
    pub executed_io: bool,
}
impl ChangeAssessment {
    /// Only a statement about observed dependencies. This is NOT permission to
    /// import closures, lazy children, IO, or arbitrary values into a new heap.
    pub fn unchanged(&self) -> bool {
        self.changed_definitions.is_empty()
            && self.changed_hosts.is_empty()
            && !self.type_abi_changed
            && !self.compiler_changed
            && !self.incomplete
            && !self.unversioned_input
            && !self.executed_io
    }
}
pub fn assess(
    before: &RevisionSnapshot,
    after: &RevisionSnapshot,
    graph: &twine::DependencyGraph,
    root: twine::AllocationId,
    hosts: &BTreeMap<GlobalName, Option<twine::HostVersion>>,
) -> ChangeAssessment {
    let footprint = graph.footprint(root);
    let mut result = ChangeAssessment {
        type_abi_changed: before.type_abi != after.type_abi,
        compiler_changed: before.compiler != after.compiler,
        incomplete: footprint.incomplete,
        unversioned_input: footprint.opaque_input,
        executed_io: footprint.executed_io,
        ..Default::default()
    };
    for (expr, origin) in footprint.code {
        if before.origins.get(expr.0) != Some(&origin) {
            result.incomplete = true;
            continue;
        }
        let Some(owner) = before.owner(expr) else {
            result.incomplete = true;
            continue;
        };
        let old = &before.definitions[owner];
        if !after
            .definitions
            .get(owner)
            .is_some_and(|new| new.id == old.id && new.code == old.code && new.abi == old.abi)
        {
            result.changed_definitions.insert(old.id);
        }
    }
    for (name, version) in footprint.hosts {
        if version.is_none() {
            result.unversioned_input = true;
        }
        if hosts.get(&name) != Some(&version) {
            result.changed_hosts.insert(name);
        }
    }
    result
}
