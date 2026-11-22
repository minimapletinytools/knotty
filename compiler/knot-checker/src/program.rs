//! Owned, checked input to executable lowering. IDs are scoped to a module
//! revision; dictionary parameters are lexical binders, never type-variable IDs.
//! The graph-facing AST and source metadata remain available alongside the
//! lowering side tables. No unresolved dictionary evidence is exported.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use knot_canonical::ast::{CDecl, CModule, Ref};
use knot_syntax::span::Span;

use crate::ast::{NodeId, TDoStmt, TExpr, TPattern, TypedExpr, TypedPattern};
use crate::check::{CheckReport, CheckedModule};
use crate::constrain::decl::ConstrainedMethod;
use crate::error::{TypeError, TypeErrorKind};
use crate::interface::instance::{resolve, Evidence, InstanceTable, StructuralShape};
use crate::solve::{ObligationMap, SchemeEnv, SchemeKey};
use crate::ty::{Scheme, Structure};
use crate::var::{Substitution, TypeVarId};

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleRevisionId(u64);
impl ModuleRevisionId {
    pub fn fresh() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BindingId(u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstanceId(u32);

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeRef {
    pub revision: ModuleRevisionId,
    pub node: NodeId,
}

#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub struct GlobalName {
    pub module: Vec<String>,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DictionaryOwner {
    Binding(BindingId),
    Instance(InstanceId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DictParamId {
    pub owner: DictionaryOwner,
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DictionaryParameter {
    pub id: DictParamId,
    pub interface: String,
    pub ty: TypeVarId,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DictExpr {
    Instance {
        interface: String,
        head: GlobalName,
        args: Vec<DictExpr>,
    },
    Param(DictParamId),
    Super {
        from: Box<DictExpr>,
        interface: String,
    },
    Structural {
        interface: String,
        shape: StructuralShape,
        args: Vec<DictExpr>,
    },
}

/// An expression's explicit operation, or one retained do statement's
/// implicit bind. Statement indices are relative to this block, not spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EvidenceSite {
    Expr(NodeId),
    DoBind { block: NodeId, statement: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedReference {
    Binding(BindingId),
    Global(GlobalName),
    /// The class dictionary's slot in this site's ordered dictionary vector.
    /// Remaining slots are the method's own extra constraints.
    Method {
        interface: String,
        name: String,
        dictionary_index: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Declaration,
    Expression,
    Pattern,
}

#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub span: Span,
    /// Declarations can introduce several types, so only expression/pattern
    /// nodes carry a single type here.
    pub ty: Option<TypeVarId>,
    pub kind: NodeKind,
}

#[derive(Debug, Clone)]
pub struct BindingInfo {
    pub name: String,
    pub global: Option<GlobalName>,
    pub span: Span,
    pub scheme: Scheme,
    /// In exactly the same order as scheme.constraints and reference arguments.
    pub parameters: Vec<DictionaryParameter>,
}

#[derive(Debug)]
pub struct Definition {
    pub binding: BindingId,
    pub body: TypedExpr,
}

#[derive(Debug)]
pub struct TypedInstance {
    pub id: InstanceId,
    pub declaration: usize,
    pub interface: String,
    pub head: GlobalName,
    pub target: TypeVarId,
    pub context: Vec<DictionaryParameter>,
    pub superclasses: Vec<(String, DictExpr)>,
    pub methods: Vec<Definition>,
    /// Deriving requests retain the type declaration in `canonical`.
    pub derived: bool,
    /// For derived instances: dictionaries for each constructor field, in
    /// declaration order. These use this instance's context parameters.
    /// Explicit instances have no generated field evidence.
    pub derived_fields: Vec<Vec<DictExpr>>,
}

#[derive(Debug)]
pub struct DefaultMethod {
    pub declaration: usize,
    pub interface: String,
    /// First parameter is the self dictionary; the rest are method constraints.
    pub definition: Definition,
}

#[derive(Debug)]
pub struct TypedProgram {
    pub kinds: HashMap<Ref, crate::kind::Kind>,
    pub hosts: crate::host::HostDeclarations,
    pub revision: ModuleRevisionId,
    /// Retains type/primitive declarations, imports and unchecked annotations.
    pub canonical: CModule,
    pub substitution: Substitution,
    /// Local/builtin schemes and referenced imported values. Complete import
    /// exports remain in their CheckedInterface, not this consumer's type arena.
    pub schemes: SchemeEnv,
    pub instances_table: std::sync::Arc<InstanceTable>,
    pub definitions: Vec<Definition>,
    pub instances: Vec<TypedInstance>,
    pub defaults: Vec<DefaultMethod>,
    pub bindings: BTreeMap<BindingId, BindingInfo>,
    pub nodes: BTreeMap<NodeId, NodeInfo>,
    /// One source origin per canonical declaration, including primitives,
    /// constructors/deriving and interfaces with no typed expression body.
    pub declaration_nodes: Vec<NodeId>,
    /// All names introduced by each pattern, including record fields/as names.
    pub pattern_bindings: BTreeMap<NodeId, Vec<BindingId>>,
    pub references: BTreeMap<EvidenceSite, ResolvedReference>,
    pub dictionaries: BTreeMap<EvidenceSite, Vec<DictExpr>>,
    pub warnings: Vec<crate::exhaustiveness::Warning>,
}

impl TypedProgram {
    /// Original right-hand side of a top-level function, before typed lambda elaboration.
    pub fn definition_body_span(&self, name: &str) -> Option<Span> {
        self.canonical.decls.iter().find_map(|decl| match &decl.node {
            CDecl::Fn(f) if f.name == name => Some(f.body.span),
            _ => None,
        })
    }

    /// Metadata for the actual checked definition, including a selected
    /// instance method. Never identify overloaded methods by spelling alone.
    /// Source metadata survives executable lowering/primitive fast paths here.
    pub fn annotations(&self, binding: BindingId) -> &[knot_canonical::ast::CAnnotation] {
        let Some(info) = self.bindings.get(&binding) else {
            return &[];
        };
        if info.global.is_some() {
            for decl in &self.canonical.decls {
                if let CDecl::Fn(f) = &decl.node {
                    if f.name == info.name {
                        return &f.annotations;
                    }
                }
            }
        }
        for instance in &self.instances {
            if instance.methods.iter().any(|m| m.binding == binding) {
                if let CDecl::Instance(i) = &self.canonical.decls[instance.declaration].node {
                    if let Some(f) = i.methods.iter().find(|f| f.name == info.name) {
                        return &f.annotations;
                    }
                }
            }
        }
        for default in &self.defaults {
            if default.definition.binding == binding {
                if let CDecl::Interface(i) = &self.canonical.decls[default.declaration].node {
                    if let Some(f) = i.defaults.iter().find(|f| f.name == info.name) {
                        return &f.annotations;
                    }
                }
            }
        }
        &[]
    }

    /// Qualify constructors, types and other non-local canonical references.
    pub fn qualify(&self, reference: &Ref) -> Option<GlobalName> {
        global(&self.canonical.name, reference)
    }

    pub fn node_ref(&self, node: NodeId) -> NodeRef {
        NodeRef {
            revision: self.revision,
            node,
        }
    }

    pub fn definition(&self, name: &str) -> Option<&Definition> {
        self.definitions
            .iter()
            .find(|d| self.bindings[&d.binding].name == name)
    }
}

type Scope = HashMap<String, BindingId>;

struct Exporter<'a> {
    module: &'a [String],
    sub: &'a mut Substitution,
    table: &'a InstanceTable,
    obligations: &'a ObligationMap,
    local_schemes: &'a HashMap<TypeVarId, Scheme>,
    globals: Scope,
    bindings: BTreeMap<BindingId, BindingInfo>,
    nodes: BTreeMap<NodeId, NodeInfo>,
    pattern_bindings: BTreeMap<NodeId, Vec<BindingId>>,
    references: BTreeMap<EvidenceSite, ResolvedReference>,
    dictionaries: BTreeMap<EvidenceSite, Vec<DictExpr>>,
    errors: Vec<TypeError>,
}

fn global(module: &[String], reference: &Ref) -> Option<GlobalName> {
    let (module, name) = match reference {
        Ref::TopLevel(name) => (module.to_vec(), name),
        Ref::Builtin(name) => (vec!["Prelude".into()], name),
        Ref::Imported { module, name } => (module.clone(), name),
        Ref::Local(_) | Ref::Unresolved(_) => return None,
    };
    Some(GlobalName {
        module,
        name: name.clone(),
    })
}

fn parameters(
    owner: DictionaryOwner,
    constraints: &[(TypeVarId, String)],
) -> Vec<DictionaryParameter> {
    constraints
        .iter()
        .enumerate()
        .map(|(index, (ty, interface))| DictionaryParameter {
            id: DictParamId { owner, index },
            interface: interface.clone(),
            ty: *ty,
        })
        .collect()
}

impl Exporter<'_> {
    fn binding(
        &mut self,
        name: &str,
        span: Span,
        scheme: Scheme,
        global: Option<GlobalName>,
    ) -> BindingId {
        let id = BindingId(self.bindings.len().try_into().expect("too many bindings"));
        let parameters = parameters(DictionaryOwner::Binding(id), &scheme.constraints);
        self.bindings.insert(
            id,
            BindingInfo {
                name: name.into(),
                global,
                span,
                scheme,
                parameters,
            },
        );
        id
    }

    fn pattern(
        &mut self,
        pattern: &TypedPattern,
        scope: &mut Scope,
        generalizable: bool,
    ) -> Vec<BindingId> {
        self.nodes.insert(
            pattern.id,
            NodeInfo {
                span: pattern.span,
                ty: Some(pattern.ty),
                kind: NodeKind::Pattern,
            },
        );
        let mut ids = Vec::new();
        match &pattern.node {
            TPattern::Var(name) => {
                let scheme = if generalizable {
                    self.local_schemes.get(&pattern.ty).cloned()
                } else {
                    None
                }
                .unwrap_or_else(|| Scheme::monomorphic(pattern.ty));
                let id = self.binding(name, pattern.span, scheme, None);
                scope.insert(name.clone(), id);
                ids.push(id);
            }
            TPattern::As(inner, name) => {
                ids.extend(self.pattern(inner, scope, false));
                let id = self.binding(name, pattern.span, Scheme::monomorphic(pattern.ty), None);
                scope.insert(name.clone(), id);
                ids.push(id);
            }
            TPattern::Record(fields) => {
                for (name, ty) in fields {
                    let id = self.binding(name, pattern.span, Scheme::monomorphic(*ty), None);
                    scope.insert(name.clone(), id);
                    ids.push(id);
                }
            }
            TPattern::Ctor(_, parts) | TPattern::Tuple(parts) => {
                for part in parts {
                    ids.extend(self.pattern(part, scope, false));
                }
            }
            TPattern::Cons(head, tail) => {
                ids.extend(self.pattern(head, scope, false));
                ids.extend(self.pattern(tail, scope, false));
            }
            _ => {}
        }
        self.pattern_bindings.insert(pattern.id, ids.clone());
        ids
    }

    fn proof(
        &mut self,
        interface: &str,
        ty: TypeVarId,
        active: &[DictionaryParameter],
        span: Span,
    ) -> Option<DictExpr> {
        // Only dictionaries bound in this lexical scope are available. The
        // solver's aggregate given map is deliberately not used here.
        let mut givens: HashMap<TypeVarId, HashSet<String>> = HashMap::new();
        for p in active {
            givens
                .entry(self.sub.find(p.ty))
                .or_default()
                .insert(p.interface.clone());
        }
        match resolve(self.sub, self.table, &givens, interface, ty) {
            Ok(evidence) => self.convert(evidence, active, span),
            Err(error) => {
                let kind = crate::interface::instance::reduce_error_kind(self.sub, error);
                self.errors.push(TypeError { span, kind });
                None
            }
        }
    }

    fn convert(
        &mut self,
        evidence: Evidence,
        active: &[DictionaryParameter],
        span: Span,
    ) -> Option<DictExpr> {
        match evidence {
            Evidence::Instance { dictionary, args } => Some(DictExpr::Instance {
                interface: dictionary.interface,
                head: global(self.module, &dictionary.head)?,
                args: args
                    .into_iter()
                    .map(|e| self.convert(e, active, span))
                    .collect::<Option<_>>()?,
            }),
            Evidence::Structural {
                interface,
                shape,
                args,
            } => Some(DictExpr::Structural {
                interface,
                shape,
                args: args
                    .into_iter()
                    .map(|e| self.convert(e, active, span))
                    .collect::<Option<_>>()?,
            }),
            Evidence::Super { from, interface } => Some(DictExpr::Super {
                from: Box::new(self.convert(*from, active, span)?),
                interface,
            }),
            Evidence::Given { interface, ty } => {
                let root = self.sub.find(ty);
                if let Some(param) = active.iter().rev().find(|p| {
                    p.interface == interface && crate::predicate::same(self.sub, p.ty, root)
                }) {
                    Some(DictExpr::Param(param.id))
                } else {
                    self.unresolved(interface, ty, span);
                    None
                }
            }
            Evidence::Residual {
                interface,
                obligation,
            } => {
                let ty = match obligation {
                    crate::interface::instance::Residual::Var(v)
                    | crate::interface::instance::Residual::Stuck(v)
                    | crate::interface::instance::Residual::Relation(v) => v,
                };
                self.unresolved(interface, ty, span);
                None
            }
        }
    }

    fn unresolved(&mut self, interface: String, ty: TypeVarId, span: Span) {
        self.errors.push(TypeError {
            span,
            kind: TypeErrorKind::UnresolvedDictionary {
                interface,
                ty: crate::print::print_type(self.sub, ty),
            },
        });
    }

    fn evidence(
        &mut self,
        site: EvidenceSite,
        obligations: Vec<(String, TypeVarId)>,
        active: &[DictionaryParameter],
        span: Span,
    ) {
        let dictionaries = obligations
            .into_iter()
            .filter_map(|(interface, ty)| self.proof(&interface, ty, active, span))
            .collect();
        self.dictionaries.insert(site, dictionaries);
    }

    fn reference(
        &mut self,
        site: EvidenceSite,
        reference: &Ref,
        key: TypeVarId,
        scope: &Scope,
        active: &[DictionaryParameter],
        span: Span,
    ) {
        let binding = match reference {
            Ref::Local(name) => scope.get(name).copied(),
            Ref::TopLevel(name) => self.globals.get(name).copied(),
            _ => None,
        };
        // Unsigned recursive references share a header during inference and
        // never instantiate a lookup. They still need the final scheme's
        // dictionaries when called after elaboration.
        let obligations = self.obligations.get(&key).cloned().unwrap_or_else(|| {
            binding
                .map(|id| {
                    self.bindings[&id]
                        .scheme
                        .constraints
                        .iter()
                        .map(|(ty, iface)| (iface.clone(), *ty))
                        .collect()
                })
                .unwrap_or_default()
        });
        let target = if let Some(id) = binding {
            Some(ResolvedReference::Binding(id))
        } else if let Ref::Builtin(name) = reference {
            if let Some(iface) = knot_canonical::prelude::prelude().method_owner(name) {
                // Prelude schemes append the class constraint after a
                // method's additional constraints; preserve that ABI.
                Some(ResolvedReference::Method {
                    interface: iface.name.clone(),
                    name: name.clone(),
                    dictionary_index: iface
                        .methods
                        .iter()
                        .find(|(n, _)| n == name)
                        .unwrap()
                        .1
                        .constraints
                        .len(),
                })
            } else {
                global(self.module, reference).map(ResolvedReference::Global)
            }
        } else {
            global(self.module, reference).map(ResolvedReference::Global)
        };
        if let Some(target) = target {
            self.references.insert(site, target);
        } else {
            self.errors.push(TypeError {
                span,
                kind: TypeErrorKind::UnboundValue(crate::print::ref_name(reference)),
            });
        }
        self.evidence(site, obligations, active, span);
    }

    fn definition(
        &mut self,
        binding: BindingId,
        body: &TypedExpr,
        scope: &Scope,
        inherited: &[DictionaryParameter],
    ) {
        let mut active = inherited.to_vec();
        active.extend(self.bindings[&binding].parameters.clone());
        self.expr(body, scope, &active);
    }

    fn lets(
        &mut self,
        bindings: &[(TypedPattern, TypedExpr)],
        scope: &mut Scope,
        active: &[DictionaryParameter],
    ) {
        let mut bound = Vec::new();
        // Let groups are simultaneous even though type inference solves SCCs.
        for (p, _) in bindings {
            bound.push(self.pattern(p, scope, true));
        }
        for ((p, body), ids) in bindings.iter().zip(bound) {
            if matches!(p.node, TPattern::Var(_)) {
                self.definition(ids[0], body, scope, active);
            } else {
                self.expr(body, scope, active);
            }
        }
    }

    fn expr(&mut self, expr: &TypedExpr, scope: &Scope, active: &[DictionaryParameter]) {
        self.nodes.insert(
            expr.id,
            NodeInfo {
                span: expr.span,
                ty: Some(expr.ty),
                kind: NodeKind::Expression,
            },
        );
        let site = EvidenceSite::Expr(expr.id);
        match &expr.node {
            TExpr::Var(r) | TExpr::Ctor(r) => {
                self.reference(site, r, expr.ty, scope, active, expr.span)
            }
            TExpr::IntLit(_) | TExpr::FloatLit(_) => {
                let interface = if matches!(expr.node, TExpr::IntLit(_)) {
                    "Num"
                } else {
                    "Fractional"
                };
                self.evidence(site, vec![(interface.into(), expr.ty)], active, expr.span);
            }
            TExpr::Lambda(patterns, body) => {
                let mut scope = scope.clone();
                for pattern in patterns {
                    self.pattern(pattern, &mut scope, false);
                }
                self.expr(body, &scope, active);
            }
            TExpr::App(f, x) => {
                self.expr(f, scope, active);
                self.expr(x, scope, active);
            }
            TExpr::BinOp(r, l, rr, key) => {
                self.reference(site, r, *key, scope, active, expr.span);
                self.expr(l, scope, active);
                self.expr(rr, scope, active);
            }
            TExpr::Negate(x, key) => {
                self.reference(
                    site,
                    &Ref::Builtin("negate".into()),
                    *key,
                    scope,
                    active,
                    expr.span,
                );
                self.expr(x, scope, active);
            }
            TExpr::If(c, t, e) => {
                self.expr(c, scope, active);
                self.expr(t, scope, active);
                self.expr(e, scope, active);
            }
            TExpr::Let(bindings, body) => {
                let mut scope = scope.clone();
                self.lets(bindings, &mut scope, active);
                self.expr(body, &scope, active);
            }
            TExpr::Case(value, arms) => {
                self.expr(value, scope, active);
                for (p, body) in arms {
                    let mut scope = scope.clone();
                    self.pattern(p, &mut scope, false);
                    self.expr(body, &scope, active);
                }
            }
            TExpr::Do(stmts, body) => {
                let mut scope = scope.clone();
                for (statement, stmt) in stmts.iter().enumerate() {
                    match stmt {
                        TDoStmt::Let(bindings) => self.lets(bindings, &mut scope, active),
                        TDoStmt::Bind(_, value, key) | TDoStmt::Expr(value, key) => {
                            self.expr(value, &scope, active);
                            self.reference(
                                EvidenceSite::DoBind {
                                    block: expr.id,
                                    statement,
                                },
                                &Ref::Builtin(">>=".into()),
                                *key,
                                &scope,
                                active,
                                value.span,
                            );
                            if let TDoStmt::Bind(pattern, _, _) = stmt {
                                self.pattern(pattern, &mut scope, false);
                            }
                        }
                    }
                }
                self.expr(body, &scope, active);
            }
            TExpr::List(items) | TExpr::Tuple(items) => {
                for item in items {
                    self.expr(item, scope, active);
                }
            }
            TExpr::Record(fields) => {
                for (_, value) in fields {
                    self.expr(value, scope, active);
                }
            }
            TExpr::RecordUpdate(base, fields) => {
                self.expr(base, scope, active);
                for (_, value) in fields {
                    self.expr(value, scope, active);
                }
            }
            TExpr::FieldAccess(base, _) | TExpr::Annotated(_, base) => {
                self.expr(base, scope, active)
            }
            TExpr::StringLit(_) | TExpr::Unit => {}
            TExpr::Hole => self.errors.push(TypeError {
                span: expr.span,
                kind: TypeErrorKind::Hole {
                    expected: crate::print::print_type(self.sub, expr.ty),
                },
            }),
        }
    }

    fn method(&mut self, method: ConstrainedMethod, context: &[DictionaryParameter]) -> Definition {
        let id = self.binding(&method.name, method.body.span, method.scheme, None);
        self.definition(id, &method.body, &Scope::new(), context);
        Definition {
            binding: id,
            body: method.body,
        }
    }
}

pub(crate) fn export(
    module: &CModule,
    checked: CheckedModule,
) -> Result<TypedProgram, CheckReport> {
    let CheckedModule {
        kinds,
        hosts,
        is_prelude,
        mut sub,
        env,
        table,
        output,
        solved,
        mut report,
    } = checked;
    if !report.errors.is_empty() {
        return Err(report);
    }
    let mut ex = Exporter {
        module: &module.name,
        sub: &mut sub,
        table: &table,
        obligations: &solved.obligations,
        local_schemes: &solved.local_schemes,
        globals: Scope::new(),
        bindings: BTreeMap::new(),
        nodes: BTreeMap::new(),
        pattern_bindings: BTreeMap::new(),
        references: BTreeMap::new(),
        dictionaries: BTreeMap::new(),
        errors: Vec::new(),
    };
    let declaration_nodes = module
        .decls
        .iter()
        .map(|decl| {
            let id = ex.sub.fresh_node();
            ex.nodes.insert(
                id,
                NodeInfo {
                    span: decl.span,
                    ty: None,
                    kind: NodeKind::Declaration,
                },
            );
            id
        })
        .collect();
    let mut members: HashMap<_, _> = output
        .members
        .into_iter()
        .map(|m| (m.name.clone(), m))
        .collect();
    let mut definitions = Vec::new();
    // Export source order, independent of dependency-solving order.
    for d in &module.decls {
        if let CDecl::Fn(f) = &d.node {
            let member = members.remove(&f.name).expect("checked function body");
            let scheme = env
                .get(&SchemeKey::TopLevel(f.name.clone()))
                .expect("checked scheme")
                .clone();
            let id = ex.binding(
                &f.name,
                d.span,
                scheme,
                global(&module.name, &Ref::TopLevel(f.name.clone())),
            );
            ex.globals.insert(f.name.clone(), id);
            definitions.push(Definition {
                binding: id,
                body: member.elaborated_body,
            });
        }
    }
    for d in &definitions {
        ex.definition(d.binding, &d.body, &Scope::new(), &[]);
    }
    let mut instances = Vec::new();
    for (declaration, retained) in output.instances {
        let CDecl::Instance(inst) = &module.decls[declaration].node else {
            unreachable!()
        };
        let head = crate::interface::instance::dictionary_head(&inst.interface, &inst.targets);
        let id = InstanceId(instances.len().try_into().unwrap());
        let context = parameters(DictionaryOwner::Instance(id), &retained.context);
        let superclasses = crate::interface::table::superclasses(&inst.interface)
            .iter()
            .filter_map(|iface| {
                ex.proof(
                    iface,
                    retained.target,
                    &context,
                    module.decls[declaration].span,
                )
                .map(|e| (iface.clone(), e))
            })
            .collect();
        let methods = retained
            .methods
            .into_iter()
            .map(|m| ex.method(m, &context))
            .collect();
        instances.push(TypedInstance {
            id,
            declaration,
            interface: inst.interface.clone(),
            head: global(&module.name, &head).unwrap(),
            target: retained.target,
            context,
            superclasses,
            methods,
            derived: false,
            derived_fields: Vec::new(),
        });
    }
    // Deriving has no source method bodies, but does have finalized context
    // parameters and superclass proofs, just like declared dictionaries.
    for (declaration, d) in module.decls.iter().enumerate() {
        let CDecl::TypeDecl(name, params, variants, deriving) = &d.node else {
            continue;
        };
        for interface in deriving {
            let head = if is_prelude {
                Ref::Builtin(name.clone())
            } else {
                Ref::TopLevel(name.clone())
            };
            let args: Vec<_> = params
                .iter()
                .map(|p| ex.sub.fresh_rigid(p.clone()))
                .collect();
            let requirements = table
                .requirements(interface, &head)
                .expect("checked derived instance");
            let constraints: Vec<_> = requirements
                .iter()
                .map(|(pos, iface)| (args[*pos], iface.clone()))
                .collect();
            let mut vars: HashMap<_, _> =
                params.iter().cloned().zip(args.iter().copied()).collect();
            let target = ex.sub.fresh_bound(Structure::App(head.clone(), args));
            let id = InstanceId(instances.len().try_into().unwrap());
            let context = parameters(DictionaryOwner::Instance(id), &constraints);
            let superclasses = crate::interface::table::superclasses(interface)
                .iter()
                .filter_map(|iface| {
                    ex.proof(iface, target, &context, d.span)
                        .map(|e| (iface.clone(), e))
                })
                .collect();
            let derived_fields = variants
                .iter()
                .map(|(_, fields)| {
                    fields
                        .iter()
                        .filter_map(|field| {
                            let ty = crate::constrain::decl::instantiate_flexible(
                                ex.sub, field, &mut vars,
                            );
                            ex.proof(interface, ty, &context, d.span)
                        })
                        .collect()
                })
                .collect();
            instances.push(TypedInstance {
                id,
                declaration,
                interface: interface.clone(),
                head: global(&module.name, &head).unwrap(),
                target,
                context,
                superclasses,
                methods: Vec::new(),
                derived: true,
                derived_fields,
            });
        }
    }
    let mut defaults = Vec::new();
    for (declaration, methods) in output.defaults {
        let CDecl::Interface(iface) = &module.decls[declaration].node else {
            unreachable!()
        };
        for method in methods {
            defaults.push(DefaultMethod {
                declaration,
                interface: iface.name.clone(),
                definition: ex.method(method, &[]),
            });
        }
    }
    if !ex.errors.is_empty() {
        report.errors.extend(ex.errors);
        return Err(report);
    }
    let Exporter {
        bindings,
        nodes,
        pattern_bindings,
        references,
        dictionaries,
        ..
    } = ex;
    let revision = ModuleRevisionId::fresh();
    Ok(TypedProgram {
        kinds,
        hosts,
        revision,
        canonical: module.clone(),
        substitution: sub,
        schemes: env,
        instances_table: table,
        definitions,
        instances,
        defaults,
        bindings,
        nodes,
        pattern_bindings,
        references,
        dictionaries,
        declaration_nodes,
        warnings: report.warnings,
    })
}
