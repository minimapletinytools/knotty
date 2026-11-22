//! The solver: walks a `Constraint` tree, unifying every `Equal`,
//! instantiating schemes at every `Lookup`, and closing each binding group
//! when its definitions are done.
//!
//! **Levels.** Each `Constraint::Let` records the level it sits at; its
//! definitions (`header_con`) are solved one level deeper, and every type
//! variable created there starts at that deeper level (see `var.rs`).
//! Unification lowers a variable's level whenever it becomes connected to
//! something from an outer scope, so once `header_con` is solved, "free
//! and still deeper than the group" is exactly the set of variables this
//! group owns and may generalize — regardless of solving order.
//!
//! **Obligations.** Interface obligations accumulate in `pending`. When a
//! group closes, every pending obligation is reduced (`interface::instance::
//! reduce`) against what is now known, and each residual is sorted by the
//! level of the variables it mentions:
//! - all outer: still pending, for an enclosing scope to deal with;
//! - on a variable this group generalizes: becomes part of the scheme;
//! - on a variable the group owns but doesn't generalize (it appears in no
//!   member's type): ambiguous — defaulted (`Int`, then `Float`) when every
//!   interface on it is numeric or `Eq`/`Ord`/`Show` and at least one is
//!   numeric, an `AmbiguousConstraint` error otherwise.
//!
//! **The monomorphism restriction.** A group with no function-typed member
//! (`x = 5`) doesn't generalize its constrained variables; they stay at the
//! outer level, so a later use (`y = x + 1.0`) can still decide them, and
//! whatever is left at the end of the module is defaulted then.

use std::collections::{HashMap, HashSet};

use knot_canonical::ast::Ref;
use knot_syntax::span::Span;

use crate::constrain::{Constraint, LetMember};
use crate::error::{TypeError, TypeErrorKind, UnifyError};
use crate::interface::instance::{reduce, reduce_error_kind, InstanceTable, Residual};
use crate::print::Printer;
use crate::ty::{Scheme, Structure};
use crate::unify::{structure_children, unify};
use crate::var::{Substitution, TypeVarId};

/// Which namespace a `Constraint::Lookup` reference resolves against.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SchemeKey {
    TopLevel(String),
    Builtin(String),
    Imported(Vec<String>, String),
}

pub fn scheme_key(reference: &Ref) -> Option<SchemeKey> {
    match reference {
        Ref::TopLevel(name) => Some(SchemeKey::TopLevel(name.clone())),
        Ref::Builtin(name) => Some(SchemeKey::Builtin(name.clone())),
        Ref::Imported { module, name } => Some(SchemeKey::Imported(module.clone(), name.clone())),
        Ref::Local(_) | Ref::Unresolved(_) => None,
    }
}

/// Every scheme available to `Constraint::Lookup`.
#[derive(Debug, Default, Clone)]
pub struct SchemeEnv {
    schemes: HashMap<SchemeKey, Scheme>,
}

impl SchemeEnv {
    pub fn new() -> Self {
        SchemeEnv::default()
    }

    pub fn insert(&mut self, key: SchemeKey, scheme: Scheme) {
        self.schemes.insert(key, scheme);
    }

    pub fn get(&self, key: &SchemeKey) -> Option<&Scheme> {
        self.schemes.get(key)
    }
}

/// An interface obligation not yet discharged.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingInstance {
    pub span: Span,
    pub interface: String,
    pub ty: TypeVarId,
}

/// For every `Lookup`/`LookupLocal`, the instantiated scheme's obligations,
/// keyed by the reference's own type — what elaboration needs to pick
/// dictionaries.
pub type ObligationMap = HashMap<TypeVarId, Vec<(String, TypeVarId)>>;

pub struct SolveResult {
    pub errors: Vec<TypeError>,
    pub obligations: ObligationMap,
    pub given: HashMap<TypeVarId, HashSet<String>>,
    /// Final local schemes keyed by the original binding header, never its
    /// union-find root (unification may merge unrelated binding types).
    pub local_schemes: HashMap<TypeVarId, Scheme>,
}

/// Solves a whole module's constraint tree against `env` (extended in place
/// with every top-level scheme) and `table`, then closes the module scope:
/// defaulting what can be defaulted and reporting everything else that is
/// still unresolved.
pub fn solve_module(
    sub: &mut Substitution,
    env: &mut SchemeEnv,
    table: &InstanceTable,
    constraint: &Constraint,
) -> SolveResult {
    let mut solver = Solver {
        sub,
        env,
        table,
        local_env: HashMap::new(),
        given: HashMap::new(),
        pending: Vec::new(),
        errors: Vec::new(),
        obligations: HashMap::new(),
        holes: Vec::new(),
    };
    solver.sub.set_level(0);
    solver.solve(constraint);
    solver.close_module();
    solver.report_holes();
    SolveResult {
        errors: solver.errors,
        obligations: solver.obligations,
        given: solver.given,
        local_schemes: solver.local_env,
    }
}

struct Solver<'a> {
    sub: &'a mut Substitution,
    env: &'a mut SchemeEnv,
    table: &'a InstanceTable,
    local_env: HashMap<TypeVarId, Scheme>,
    given: HashMap<TypeVarId, HashSet<String>>,
    pending: Vec<PendingInstance>,
    errors: Vec<TypeError>,
    obligations: ObligationMap,
    holes: Vec<(Span, TypeVarId)>,
}

/// One reduced obligation.
struct Reduced {
    span: Span,
    interface: String,
    residual: Residual,
}

impl Solver<'_> {
    fn error(&mut self, span: Span, kind: TypeErrorKind) {
        self.errors.push(TypeError { span, kind });
    }

    fn unify_or_report(&mut self, span: Span, expected: TypeVarId, actual: TypeVarId) {
        let Err(e) = unify(self.sub, expected, actual) else {
            return;
        };
        let kind = match e {
            UnifyError::Mismatch { .. } => {
                let mut p = Printer::new();
                TypeErrorKind::Mismatch {
                    expected: p.ty(self.sub, expected),
                    actual: p.ty(self.sub, actual),
                }
            }
            UnifyError::Occurs { in_ty, .. } => TypeErrorKind::InfiniteType {
                ty: Printer::new().ty(self.sub, in_ty),
            },
            UnifyError::RigidEscape { rigid, .. } => TypeErrorKind::EscapedTypeVariable {
                name: self.sub.rigid_name(rigid).unwrap_or_default(),
            },
        };
        self.error(span, kind);
    }

    fn add_given(&mut self, var: TypeVarId, interface: &str) {
        let root = self.sub.find(var);
        let entry = self.given.entry(root).or_default();
        entry.insert(interface.to_string());
    }

    fn instantiate_into(&mut self, span: Span, scheme: &Scheme, expected: TypeVarId) {
        let (fresh_ty, fresh_constraints) = instantiate(self.sub, scheme);
        self.unify_or_report(span, expected, fresh_ty);
        self.obligations.insert(
            expected,
            fresh_constraints
                .iter()
                .map(|(ty, interface)| (interface.clone(), *ty))
                .collect(),
        );
        for (ty, interface) in fresh_constraints {
            self.pending.push(PendingInstance {
                span,
                interface,
                ty,
            });
        }
    }

    fn solve(&mut self, mut constraint: &Constraint) {
        loop {
            match constraint {
                Constraint::True => {}
                Constraint::Equal {
                    span,
                    expected,
                    actual,
                } => self.unify_or_report(*span, *expected, *actual),
                Constraint::HasInstance {
                    span,
                    interface,
                    ty,
                } => self.pending.push(PendingInstance {
                    span: *span,
                    interface: interface.clone(),
                    ty: *ty,
                }),
                Constraint::Lookup {
                    span,
                    reference,
                    expected,
                } => match scheme_key(reference).and_then(|k| self.env.get(&k).cloned()) {
                    Some(scheme) => self.instantiate_into(*span, &scheme, *expected),
                    None => self.error(
                        *span,
                        TypeErrorKind::UnboundValue(crate::print::ref_name(reference)),
                    ),
                },
                Constraint::LookupLocal {
                    span,
                    key,
                    expected,
                } => {
                    // A missing entry only happens after an earlier error left a
                    // binding unsolved; treat the reference as unconstrained.
                    if let Some(scheme) = self.local_env.get(key).cloned() {
                        self.instantiate_into(*span, &scheme, *expected);
                    }
                }
                Constraint::And(cs) => {
                    for c in cs {
                        self.solve(c);
                    }
                }
                Constraint::Install(schemes) => {
                    for (key, scheme) in schemes {
                        self.env.insert(key.clone(), scheme.clone());
                    }
                }
                Constraint::Hole { span, ty } => self.holes.push((*span, *ty)),
                Constraint::Let {
                    level,
                    top_level,
                    generalize,
                    members,
                    pattern_tys,
                    header_con,
                    body_con,
                } => {
                    for m in members {
                        if let Some(d) = &m.declared {
                            for (var, interface) in &d.given {
                                self.add_given(*var, interface);
                            }
                        }
                    }
                    self.sub.set_level(level + 1);
                    self.solve(header_con);
                    self.sub.set_level(*level);

                    let schemes = self.close_group(*level, members, pattern_tys, *generalize);
                    for (m, scheme) in members.iter().zip(schemes) {
                        if *top_level {
                            self.env.insert(SchemeKey::TopLevel(m.name.clone()), scheme);
                        } else {
                            self.local_env.insert(m.header_ty, scheme);
                        }
                    }
                    constraint = body_con;
                    continue;
                }
                Constraint::Given { level, given, body } => {
                    for (var, interface) in given {
                        self.add_given(*var, interface);
                    }
                    self.sub.set_level(level + 1);
                    self.solve(body);
                    self.sub.set_level(*level);
                    self.close_group(*level, &[], &[], true);
                }
            }
            break;
        }
    }

    /// Reduces every pending obligation, reporting the ones that fail.
    fn reduce_pending(&mut self) -> Vec<Reduced> {
        let pending = std::mem::take(&mut self.pending);
        let mut out = Vec::new();
        for p in pending {
            match reduce(self.sub, self.table, &self.given, &p.interface, p.ty) {
                Ok(residuals) => {
                    out.extend(residuals.into_iter().map(|(interface, residual)| Reduced {
                        span: p.span,
                        interface,
                        residual,
                    }))
                }
                Err(e) => {
                    let kind = reduce_error_kind(self.sub, e);
                    self.error(p.span, kind);
                }
            }
        }
        out
    }

    fn residual_ty(r: &Residual) -> TypeVarId {
        match r {
            Residual::Var(v) | Residual::Stuck(v) | Residual::Relation(v) => *v,
        }
    }

    /// Closes a group at `level`: decides what its members generalize over
    /// and disposes of every obligation the group owns. Returns one scheme
    /// per member (a signed member's is its declared signature).
    fn close_group(
        &mut self,
        level: u32,
        members: &[LetMember],
        pattern_tys: &[TypeVarId],
        generalize: bool,
    ) -> Vec<Scheme> {
        let unsigned: Vec<TypeVarId> = members
            .iter()
            .filter(|m| m.declared.is_none())
            .map(|m| m.header_ty)
            .collect();

        if !generalize {
            for &ty in unsigned.iter().chain(pattern_tys) {
                for v in free_vars(self.sub, ty) {
                    self.sub.lower_level(v, level);
                }
            }
        }

        let reduced = self.reduce_pending();

        // The variables this group could quantify over.
        let member_ftvs: Vec<Vec<TypeVarId>> = unsigned
            .iter()
            .map(|&ty| {
                free_vars(self.sub, ty)
                    .into_iter()
                    .filter(|v| self.is_inner(*v, level))
                    .collect()
            })
            .collect();
        let mut quantified: HashSet<TypeVarId> = member_ftvs.iter().flatten().copied().collect();

        let is_function = unsigned
            .iter()
            .any(|&ty| matches!(self.sub.resolve_structure(ty), Some(Structure::Fn(..))));

        let mut owned = Vec::new();
        for r in reduced {
            let vars = free_vars(self.sub, Self::residual_ty(&r.residual));
            if vars.iter().all(|v| !self.is_inner(*v, level)) {
                self.pending.push(PendingInstance {
                    span: r.span,
                    interface: r.interface,
                    ty: Self::residual_ty(&r.residual),
                });
                continue;
            }
            owned.push(r);
        }

        // The monomorphism restriction: a non-function group keeps its
        // constrained variables monomorphic, handing them to the outer
        // scope along with their obligations.
        if !is_function && !unsigned.is_empty() {
            let mut lowered = Vec::new();
            owned.retain(|r| {
                let ty = Self::residual_ty(&r.residual);
                let vars = free_vars(self.sub, ty);
                if vars.iter().any(|v| quantified.contains(v)) {
                    lowered.extend(vars);
                    self.pending.push(PendingInstance {
                        span: r.span,
                        interface: r.interface.clone(),
                        ty,
                    });
                    false
                } else {
                    true
                }
            });
            for v in lowered {
                self.sub.lower_level(v, level);
                quantified.remove(&v);
            }
        }

        let mut scheme_constraints: Vec<(TypeVarId, String)> = Vec::new();
        let mut ambiguous: Vec<Reduced> = Vec::new();
        for r in owned {
            match r.residual {
                Residual::Var(v) if quantified.contains(&v) => {
                    scheme_constraints.push((v, r.interface));
                }
                Residual::Var(_) => ambiguous.push(r),
                Residual::Relation(ty) => {
                    let vars = free_vars(self.sub, ty);
                    if vars
                        .iter()
                        .all(|v| quantified.contains(v) || !self.is_inner(*v, level))
                    {
                        scheme_constraints.push((ty, r.interface));
                    } else {
                        ambiguous.push(r);
                    }
                }
                Residual::Stuck(ty) => {
                    let kind = TypeErrorKind::NoInstance {
                        ty: Printer::new().ty(self.sub, ty),
                        interface: r.interface,
                    };
                    self.error(r.span, kind);
                }
            }
        }
        self.default_or_report(ambiguous);

        let mut unsigned_ftvs = member_ftvs.into_iter();
        members
            .iter()
            .map(|m| match &m.declared {
                Some(d) => Scheme {
                    vars: d.rigid_vars.clone(),
                    constraints: d.given.clone(),
                    ty: m.header_ty,
                },
                None => {
                    let ftv = unsigned_ftvs.next().unwrap_or_default();
                    let vars: Vec<TypeVarId> = ftv
                        .into_iter()
                        .filter(|v| quantified.contains(v) && self.sub.is_unbound(*v))
                        .collect();
                    let mut constraints = Vec::new();
                    for (ty, interface) in &scheme_constraints {
                        let mentioned = free_vars(self.sub, *ty);
                        if !mentioned.iter().any(|v| vars.contains(v)) {
                            continue;
                        }
                        if mentioned
                            .iter()
                            .any(|v| quantified.contains(v) && !vars.contains(v))
                        {
                            let ty = Printer::new().ty(self.sub, *ty);
                            self.error(
                                m.span,
                                TypeErrorKind::AmbiguousConstraint {
                                    interface: interface.clone(),
                                    ty,
                                },
                            );
                        } else {
                            constraints.push((*ty, interface.clone()));
                        }
                    }
                    Scheme {
                        vars,
                        constraints,
                        ty: m.header_ty,
                    }
                }
            })
            .collect()
    }

    fn is_inner(&mut self, v: TypeVarId, level: u32) -> bool {
        self.sub.is_unbound(v) && self.sub.var_level(v).is_some_and(|l| l > level)
    }

    /// Defaults each ambiguous variable if it can be, otherwise reports it.
    fn default_or_report(&mut self, ambiguous: Vec<Reduced>) {
        let blocked: HashSet<_> = ambiguous
            .iter()
            .filter(|r| matches!(r.residual, Residual::Relation(_)))
            .flat_map(|r| free_vars(self.sub, Self::residual_ty(&r.residual)))
            .collect();
        let mut by_var: Vec<(TypeVarId, Span, Vec<String>)> = Vec::new();
        for r in ambiguous {
            if let Residual::Relation(ty) = r.residual {
                let ty = Printer::new().ty(self.sub, ty);
                self.error(
                    r.span,
                    TypeErrorKind::AmbiguousConstraint {
                        interface: r.interface,
                        ty,
                    },
                );
                continue;
            }
            if blocked.contains(&self.sub.find(Self::residual_ty(&r.residual))) {
                continue;
            }
            let v = self.sub.find(Self::residual_ty(&r.residual));
            match by_var.iter_mut().find(|(var, _, _)| *var == v) {
                Some((_, _, ifaces)) => ifaces.push(r.interface),
                None => by_var.push((v, r.span, vec![r.interface])),
            }
        }
        for (v, span, mut interfaces) in by_var {
            // An earlier default in this loop may already have solved it.
            if !self.sub.is_unbound(v) {
                let leftover: Vec<PendingInstance> = interfaces
                    .into_iter()
                    .map(|interface| PendingInstance {
                        span,
                        interface,
                        ty: v,
                    })
                    .collect();
                self.pending.extend(leftover);
                let reduced = self.reduce_pending();
                self.default_or_report(reduced);
                continue;
            }
            if let Some(default) = self.pick_default(&interfaces) {
                let default_ty = self
                    .sub
                    .fresh_bound(Structure::App(Ref::Builtin(default.to_string()), vec![]));
                self.unify_or_report(span, v, default_ty);
                continue;
            }
            interfaces.sort();
            interfaces.dedup();
            let ty = Printer::new().ty(self.sub, v);
            self.error(
                span,
                TypeErrorKind::AmbiguousConstraint {
                    interface: interfaces.join(", "),
                    ty,
                },
            );
        }
    }

    fn pick_default(&mut self, interfaces: &[String]) -> Option<&'static str> {
        // Haskell's rule: at least one numeric class, and every class a
        // standard one.
        const NUMERIC: &[&str] = &[
            "Num",
            "Real",
            "Integral",
            "Fractional",
            "Floating",
            "RealFrac",
            "RealFloat",
        ];
        const STANDARD: &[&str] = &["Eq", "Ord", "Show", "Enum"];
        if !interfaces.iter().any(|i| NUMERIC.contains(&i.as_str()))
            || !interfaces
                .iter()
                .all(|i| NUMERIC.contains(&i.as_str()) || STANDARD.contains(&i.as_str()))
        {
            return None;
        }
        ["Int", "Float"].into_iter().find(|candidate| {
            let ty = self
                .sub
                .fresh_bound(Structure::App(Ref::Builtin(candidate.to_string()), vec![]));
            interfaces.iter().all(|i| {
                matches!(reduce(self.sub, self.table, &self.given, i, ty), Ok(r) if r.is_empty())
            })
        })
    }

    /// The end of the module: every obligation still pending is owned by
    /// nothing further out, so it is defaulted or reported.
    fn close_module(&mut self) {
        let reduced = self.reduce_pending();
        let mut ambiguous = Vec::new();
        for r in reduced {
            match r.residual {
                Residual::Var(_) | Residual::Relation(_) => ambiguous.push(r),
                Residual::Stuck(ty) => {
                    let kind = TypeErrorKind::NoInstance {
                        ty: Printer::new().ty(self.sub, ty),
                        interface: r.interface,
                    };
                    self.error(r.span, kind);
                }
            }
        }
        self.default_or_report(ambiguous);
    }

    fn report_holes(&mut self) {
        for (span, ty) in std::mem::take(&mut self.holes) {
            let expected = Printer::new().ty(self.sub, ty);
            self.error(span, TypeErrorKind::Hole { expected });
        }
    }
}

/// Every free (unbound) variable reachable from `ty`, in first-occurrence
/// order, deduplicated. Rigid variables are not included.
pub fn free_vars(sub: &mut Substitution, ty: TypeVarId) -> Vec<TypeVarId> {
    fn go(
        sub: &mut Substitution,
        ty: TypeVarId,
        seen: &mut HashSet<TypeVarId>,
        out: &mut Vec<TypeVarId>,
    ) {
        let root = sub.find(ty);
        match sub.resolve_structure(root) {
            Some(structure) => {
                for child in structure_children(&structure) {
                    go(sub, child, seen, out);
                }
            }
            None => {
                if sub.is_unbound(root) && seen.insert(root) {
                    out.push(root);
                }
            }
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    go(sub, ty, &mut seen, &mut out);
    out
}

/// A fresh copy of `scheme.ty`, with a fresh variable (at the current
/// level) for each quantified one, plus the scheme's constraints rewritten
/// onto those fresh variables.
pub fn instantiate(
    sub: &mut Substitution,
    scheme: &Scheme,
) -> (TypeVarId, Vec<(TypeVarId, String)>) {
    if scheme.vars.is_empty() {
        return (scheme.ty, scheme.constraints.clone());
    }
    let mut mapping = HashMap::new();
    for &v in &scheme.vars {
        let root = sub.find(v);
        let fresh = sub.fresh_unbound();
        mapping.insert(root, fresh);
    }
    let fresh_ty = copy_structure(sub, scheme.ty, &mapping);
    let fresh_constraints = scheme
        .constraints
        .iter()
        .map(|(v, interface)| {
            let root = sub.find(*v);
            (copy_structure(sub, root, &mapping), interface.clone())
        })
        .collect();
    (fresh_ty, fresh_constraints)
}

fn copy_structure(
    sub: &mut Substitution,
    ty: TypeVarId,
    mapping: &HashMap<TypeVarId, TypeVarId>,
) -> TypeVarId {
    let root = sub.find(ty);
    if let Some(&fresh) = mapping.get(&root) {
        return fresh;
    }
    let Some(structure) = sub.resolve_structure(root) else {
        // Not quantified: an outer variable or a rigid one, shared as-is.
        return root;
    };
    let copied = match structure {
        Structure::App(r, args) => Structure::App(
            r,
            args.iter()
                .map(|a| copy_structure(sub, *a, mapping))
                .collect(),
        ),
        Structure::Ctor(r, args) => Structure::Ctor(
            r,
            args.iter()
                .map(|a| copy_structure(sub, *a, mapping))
                .collect(),
        ),
        Structure::Fn(a, b) => {
            let a = copy_structure(sub, a, mapping);
            let b = copy_structure(sub, b, mapping);
            Structure::Fn(a, b)
        }
        Structure::Predicate(elems) => Structure::Predicate(
            elems
                .iter()
                .map(|e| copy_structure(sub, *e, mapping))
                .collect(),
        ),
        Structure::Tuple(elems) => Structure::Tuple(
            elems
                .iter()
                .map(|e| copy_structure(sub, *e, mapping))
                .collect(),
        ),
        Structure::Record(fields, ext) => Structure::Record(
            fields
                .iter()
                .map(|(k, v)| (k.clone(), copy_structure(sub, *v, mapping)))
                .collect(),
            ext.map(|e| copy_structure(sub, e, mapping)),
        ),
        Structure::Unit => Structure::Unit,
        Structure::VarApp(f, args) => {
            let f = copy_structure(sub, f, mapping);
            Structure::VarApp(
                f,
                args.iter()
                    .map(|a| copy_structure(sub, *a, mapping))
                    .collect(),
            )
        }
    };
    sub.fresh_bound(copied)
}
