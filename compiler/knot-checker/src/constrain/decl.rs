//! Binding-group constraint generation for a whole module and for `let`
//! expressions: dependency (SCC) splitting, then one nested
//! `Constraint::Let` per group, plus checking instance method bodies.
//!
//! **Groups.** Every binding is a node in a dependency graph — a `let`
//! pattern binding (`(a, b) = ...`) is one node binding several names.
//! A top-level binding depends on the top-level names its body mentions, a
//! `let` binding on its siblings' names. Signed top-level bindings are
//! *not* dependency targets: every reference to one, including its own
//! recursive calls, instantiates its declared signature (installed up front
//! via `Constraint::Install`), so signed bindings never share a
//! monomorphic group with anything.
//!
//! **Levels.** A group's definitions are generated one level deeper than
//! the group itself (`Substitution::enter_level`), matching the level
//! `solve.rs` solves them at — see `var.rs` for why that's what makes
//! generalization order-independent.
//!
//! **Instances.** Each method body is checked against its interface's
//! declared method signature (from the prelude) with the interface's
//! variable replaced by the instance target. The target's variables and
//! the method's own extra variables are rigid; the instance context and
//! any constraint in the method's own signature are `given` facts.

use std::collections::{HashMap, HashSet};

use knot_canonical::ast::{
    CDecl, CExpr, CFnDef, CInstanceDecl, CInterfaceDecl, CPattern, CType, CTypeSignature, Ref,
};
use knot_syntax::span::{Span, Spanned};

use crate::ast::{TExpr, TPattern, Typed, TypedExpr, TypedPattern};
use crate::constrain::expr::constrain_expr;
use crate::constrain::pattern::constrain_pattern;
use crate::constrain::{Constraint, DeclaredScheme, LetMember, LocalScope};
use crate::interface::table::{interface, method_signature};
use crate::sig::{build, VarBinding};
use crate::solve::SchemeKey;
use crate::ty::{Scheme, Structure};
use crate::var::{Substitution, TypeVarId};

/// Every name reference in `expr` (and in the constructor patterns inside
/// it) that `pick` accepts.
fn collect_refs(expr: &CExpr, pick: &dyn Fn(&Ref) -> Option<String>, out: &mut HashSet<String>) {
    let go = |e: &CExpr, out: &mut HashSet<String>| collect_refs(e, pick, out);
    match expr {
        CExpr::Var(r) | CExpr::Ctor(r) => out.extend(pick(r)),
        CExpr::IntLit(_) | CExpr::FloatLit(_) | CExpr::StringLit(_) | CExpr::Unit | CExpr::Hole => {
        }
        CExpr::Lambda(params, body) => {
            for p in params {
                collect_pattern_refs(&p.node, pick, out);
            }
            go(&body.node, out);
        }
        CExpr::App(a, b) => {
            go(&a.node, out);
            go(&b.node, out);
        }
        CExpr::BinOp(op, a, b) => {
            out.extend(pick(op));
            go(&a.node, out);
            go(&b.node, out);
        }
        CExpr::Negate(e) | CExpr::FieldAccess(e, _) | CExpr::Annotated(_, e) => go(&e.node, out),
        CExpr::If(c, t, e) => {
            go(&c.node, out);
            go(&t.node, out);
            go(&e.node, out);
        }
        CExpr::Let(bindings, body) => {
            for (p, e) in bindings {
                collect_pattern_refs(&p.node, pick, out);
                go(&e.node, out);
            }
            go(&body.node, out);
        }
        CExpr::Case(scrutinee, arms) => {
            go(&scrutinee.node, out);
            for (p, e) in arms {
                collect_pattern_refs(&p.node, pick, out);
                go(&e.node, out);
            }
        }
        CExpr::Do(stmts, final_expr) => {
            for stmt in stmts {
                match stmt {
                    knot_canonical::ast::CDoStmt::Bind(p, e) => {
                        collect_pattern_refs(&p.node, pick, out);
                        go(&e.node, out);
                    }
                    knot_canonical::ast::CDoStmt::Expr(e) => go(&e.node, out),
                    knot_canonical::ast::CDoStmt::Let(bindings) => {
                        for (p, e) in bindings {
                            collect_pattern_refs(&p.node, pick, out);
                            go(&e.node, out);
                        }
                    }
                }
            }
            go(&final_expr.node, out);
        }
        CExpr::List(es) | CExpr::Tuple(es) => {
            for e in es {
                go(&e.node, out);
            }
        }
        CExpr::Record(fields) => {
            for (_, e) in fields {
                go(&e.node, out);
            }
        }
        CExpr::RecordUpdate(base, fields) => {
            go(&base.node, out);
            for (_, e) in fields {
                go(&e.node, out);
            }
        }
    }
}

fn collect_pattern_refs(
    pattern: &CPattern,
    pick: &dyn Fn(&Ref) -> Option<String>,
    out: &mut HashSet<String>,
) {
    match pattern {
        CPattern::Ctor(r, subs) => {
            out.extend(pick(r));
            for s in subs {
                collect_pattern_refs(&s.node, pick, out);
            }
        }
        CPattern::Tuple(subs) => {
            for s in subs {
                collect_pattern_refs(&s.node, pick, out);
            }
        }
        CPattern::Cons(head, tail) => {
            collect_pattern_refs(&head.node, pick, out);
            collect_pattern_refs(&tail.node, pick, out);
        }
        CPattern::As(inner, _) => collect_pattern_refs(&inner.node, pick, out),
        CPattern::Wildcard(_)
        | CPattern::Var(_)
        | CPattern::Literal(_)
        | CPattern::Nil
        | CPattern::Record(_)
        | CPattern::Unit => {}
    }
}

/// The names a pattern binds.
fn pattern_names(pattern: &CPattern, out: &mut Vec<String>) {
    match pattern {
        CPattern::Var(n) => out.push(n.clone()),
        CPattern::As(inner, n) => {
            pattern_names(&inner.node, out);
            out.push(n.clone());
        }
        CPattern::Ctor(_, subs) | CPattern::Tuple(subs) => {
            for s in subs {
                pattern_names(&s.node, out);
            }
        }
        CPattern::Cons(h, t) => {
            pattern_names(&h.node, out);
            pattern_names(&t.node, out);
        }
        CPattern::Record(fields) => out.extend(fields.iter().cloned()),
        CPattern::Wildcard(_) | CPattern::Literal(_) | CPattern::Nil | CPattern::Unit => {}
    }
}

/// Tarjan's SCC algorithm over nodes `0..n`, returned in dependency order:
/// a component's dependencies always come before it.
pub fn scc_order(n: usize, edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    struct State<'e> {
        edges: &'e [Vec<usize>],
        counter: usize,
        index: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        sccs: Vec<Vec<usize>>,
    }
    fn connect(v: usize, st: &mut State) {
        st.index[v] = Some(st.counter);
        st.lowlink[v] = st.counter;
        st.counter += 1;
        st.stack.push(v);
        st.on_stack[v] = true;
        for &w in &st.edges[v].clone() {
            match st.index[w] {
                None => {
                    connect(w, st);
                    st.lowlink[v] = st.lowlink[v].min(st.lowlink[w]);
                }
                Some(iw) if st.on_stack[w] => st.lowlink[v] = st.lowlink[v].min(iw),
                Some(_) => {}
            }
        }
        if Some(st.lowlink[v]) == st.index[v] {
            let mut component = Vec::new();
            while let Some(w) = st.stack.pop() {
                st.on_stack[w] = false;
                component.push(w);
                if w == v {
                    break;
                }
            }
            component.sort();
            st.sccs.push(component);
        }
    }
    let mut st = State {
        edges,
        counter: 0,
        index: vec![None; n],
        lowlink: vec![0; n],
        on_stack: vec![false; n],
        stack: Vec::new(),
        sccs: Vec::new(),
    };
    for v in 0..n {
        if st.index[v].is_none() {
            connect(v, &mut st);
        }
    }
    st.sccs
}

/// Builds the `Structure` a `CType` describes, turning each distinct
/// type-variable (or record row-extension) name into its own rigid
/// `Substitution` slot, shared across every occurrence of that name — see
/// module docs on why this matters for enforcing a signature honestly.
pub(crate) fn instantiate_rigid(
    sub: &mut Substitution,
    ty: &CType,
    rigids: &mut HashMap<String, TypeVarId>,
) -> TypeVarId {
    match ty {
        CType::Named(r, args) => {
            let arg_tys = args
                .iter()
                .map(|a| instantiate_rigid(sub, a, rigids))
                .collect();
            sub.fresh_bound(Structure::App(r.clone(), arg_tys))
        }
        CType::Var(name) => rigid_var(sub, rigids, name),
        // `f a` (spec §10.5) -- `f` itself becomes a rigid var exactly like
        // an ordinary type variable (`rigid_var`'s own shared `rigids` map
        // guarantees the same TypeVarId across every mention of `f` within
        // this one signature, so `Foldable f => f a -> Int`'s two `f`s
        // agree), then wrapped around its own recursively-instantiated
        // arguments into a `Structure::VarApp` -- the same shape `unify.rs`
        // and `check_instance`'s rigid-defers-to-`given` case already know
        // how to handle, since it's the same one the hand-written
        // `Functor`/`Applicative`/`Monad`/`Foldable` prelude schemes have
        // always produced.
        CType::VarApp(name, args) => {
            let f_var = rigid_var(sub, rigids, name);
            let arg_tys = args
                .iter()
                .map(|a| instantiate_rigid(sub, a, rigids))
                .collect();
            sub.fresh_bound(Structure::VarApp(f_var, arg_tys))
        }
        CType::Fn(a, b) => {
            let a_ty = instantiate_rigid(sub, a, rigids);
            let b_ty = instantiate_rigid(sub, b, rigids);
            sub.fresh_bound(Structure::Fn(a_ty, b_ty))
        }
        CType::Tuple(ts) => {
            let tys = ts
                .iter()
                .map(|t| instantiate_rigid(sub, t, rigids))
                .collect();
            sub.fresh_bound(Structure::Tuple(tys))
        }
        CType::Record(fields, ext) => {
            let field_tys = fields
                .iter()
                .map(|(name, t)| (name.clone(), instantiate_rigid(sub, t, rigids)))
                .collect();
            let ext_ty = ext.as_ref().map(|name| rigid_var(sub, rigids, name));
            sub.fresh_bound(Structure::Record(field_tys, ext_ty))
        }
        CType::Unit => sub.fresh_bound(Structure::Unit),
    }
}

fn rigid_var(
    sub: &mut Substitution,
    rigids: &mut HashMap<String, TypeVarId>,
    name: &str,
) -> TypeVarId {
    *rigids
        .entry(name.to_string())
        .or_insert_with(|| sub.fresh_rigid(name.to_string()))
}

/// Generation-time counterpart of a signature: the expected type to check a
/// signed binding's body against, its rigid variables, and its `given`
/// interface obligations (see `DeclaredScheme`).
fn build_declared(sub: &mut Substitution, sig: &CTypeSignature) -> (TypeVarId, DeclaredScheme) {
    let mut rigids = HashMap::new();
    let expected_ty = instantiate_rigid(sub, &sig.ty, &mut rigids);
    let given = sig
        .constraints
        .iter()
        .map(|c| {
            (
                {
                    let args = c
                        .type_vars
                        .iter()
                        .map(|v| rigid_var(sub, &mut rigids, v))
                        .collect();
                    crate::predicate::pack(sub, args)
                },
                c.interface.clone(),
            )
        })
        .collect();
    let mut rigid_vars: Vec<_> = rigids.into_values().collect();
    rigid_vars.sort();
    (expected_ty, DeclaredScheme { rigid_vars, given })
}

/// Same walk as `instantiate_rigid`, but for a context that needs genuinely
/// quantifiable, ordinary flexible variables instead of rigid ones — a
/// constructor's own type parameters aren't being checked against a body
/// the way a signature's are, they're just being *given* a type, so there's
/// nothing for rigidity to protect here. Kept as its own small function
/// rather than parameterizing `instantiate_rigid` over "how to make a
/// fresh var": the two are similar, but different enough in *why* they
/// exist that sharing one implementation would obscure which is which.
pub(crate) fn instantiate_flexible(
    sub: &mut Substitution,
    ty: &CType,
    vars: &mut HashMap<String, TypeVarId>,
) -> TypeVarId {
    match ty {
        CType::Named(r, args) => {
            let arg_tys = args
                .iter()
                .map(|a| instantiate_flexible(sub, a, vars))
                .collect();
            sub.fresh_bound(Structure::App(r.clone(), arg_tys))
        }
        CType::Var(name) => *vars
            .entry(name.clone())
            .or_insert_with(|| sub.fresh_unbound()),
        // Mirrors `instantiate_rigid`'s own `VarApp` case, just flexible
        // (`fresh_unbound`) instead of rigid -- e.g. a data
        // constructor field typed `f a` for a type declared `type Wrapper f
        // a = Wrap (f a)`.
        CType::VarApp(name, args) => {
            let f_var = *vars
                .entry(name.clone())
                .or_insert_with(|| sub.fresh_unbound());
            let arg_tys = args
                .iter()
                .map(|a| instantiate_flexible(sub, a, vars))
                .collect();
            sub.fresh_bound(Structure::VarApp(f_var, arg_tys))
        }
        CType::Fn(a, b) => {
            let a_ty = instantiate_flexible(sub, a, vars);
            let b_ty = instantiate_flexible(sub, b, vars);
            sub.fresh_bound(Structure::Fn(a_ty, b_ty))
        }
        CType::Tuple(ts) => {
            let tys = ts
                .iter()
                .map(|t| instantiate_flexible(sub, t, vars))
                .collect();
            sub.fresh_bound(Structure::Tuple(tys))
        }
        CType::Record(fields, ext) => {
            let field_tys = fields
                .iter()
                .map(|(name, t)| (name.clone(), instantiate_flexible(sub, t, vars)))
                .collect();
            let ext_ty = ext.as_ref().map(|name| {
                *vars
                    .entry(name.clone())
                    .or_insert_with(|| sub.fresh_unbound())
            });
            sub.fresh_bound(Structure::Record(field_tys, ext_ty))
        }
        CType::Unit => sub.fresh_bound(Structure::Unit),
    }
}

/// Seeds `env` with a `Scheme` for every constructor of every `CDecl::
/// TypeDecl` in `decls` — e.g. `type Shape = Circle Float` installs
/// `Circle :: Float -> Shape` under `SchemeKey::TopLevel("Circle")`,
/// exactly the shape `prelude.rs`'s own `seed_constructors` hand-writes for
/// built-ins like `Just`/`Ok`, just derived from the real declaration
/// instead of hardcoded — nothing else in this crate did this at all before
/// (a real, previously-undocumented gap: without it, *any* use of a
/// user-defined constructor as a value or in a pattern is an `UnboundValue`
/// error, since `Ref::TopLevel`-headed `Lookup`/pattern-`Lookup`
/// constraints have nowhere to resolve against).
///
/// Needs no unification or generalization at all: a constructor's own type
/// is completely determined by its declaration (this language has no
/// syntax for constraining an ADT's own type parameters, unlike a
/// function's signature), so this installs a finished `Scheme` directly
/// rather than routing through `Constraint`/`solve.rs` the way `constrain_
/// module`'s ordinary bindings do. Every one of the type's own declared
/// parameters gets its own quantified variable, even one a particular
/// variant's fields never mention (`type Result e a = Ok a | Err e`'s `Ok`
/// still needs to be quantified over `e`, or its constructed value's own
/// type wouldn't actually be `Result e a` in full).
///
/// This function seeds local constructors. Checked module interfaces seed
/// imported constructor schemes separately, with fresh consumer type variables.
pub fn seed_user_constructors(
    sub: &mut Substitution,
    env: &mut crate::solve::SchemeEnv,
    decls: &[Spanned<CDecl>],
) {
    for d in decls {
        let CDecl::TypeDecl(type_name, params, variants, _deriving) = &d.node else {
            continue;
        };
        for (ctor_name, field_types) in variants {
            let mut vars = HashMap::new();
            let field_tys: Vec<TypeVarId> = field_types
                .iter()
                .map(|t| instantiate_flexible(sub, t, &mut vars))
                .collect();
            for p in params {
                vars.entry(p.clone()).or_insert_with(|| sub.fresh_unbound());
            }
            let param_vars: Vec<TypeVarId> = params.iter().map(|p| vars[p]).collect();
            let result_ty =
                sub.fresh_bound(Structure::App(Ref::TopLevel(type_name.clone()), param_vars));
            let ctor_ty = field_tys
                .iter()
                .rev()
                .fold(result_ty, |acc, &f| sub.fresh_bound(Structure::Fn(f, acc)));
            let mut quantified: Vec<_> = vars.into_values().collect();
            quantified.sort();
            env.insert(
                crate::solve::SchemeKey::TopLevel(ctor_name.clone()),
                crate::ty::Scheme {
                    vars: quantified,
                    constraints: vec![],
                    ty: ctor_ty,
                },
            );
        }
    }
}

/// One binding to constrain: a top-level `CFnDef`, or one `let` binding.
struct RawBinding<'a> {
    lhs: Lhs<'a>,
    /// Every name this binding introduces.
    names: Vec<String>,
    span: Span,
    signature: Option<&'a Spanned<CTypeSignature>>,
    params: &'a [Spanned<CPattern>],
    body: &'a Spanned<CExpr>,
}

enum Lhs<'a> {
    Name(String),
    Pattern(&'a Spanned<CPattern>),
}

/// A signed binding's prebuilt header: its rigid signature type.
type Declared = HashMap<usize, (TypeVarId, DeclaredScheme)>;

/// Everything `constrain_bindings` produces besides the caller's own `R`.
struct ChainOutput {
    constraint: Constraint,
    members: Vec<LetMember>,
    /// Source binding index survives SCC ordering for the graph-facing AST.
    elaborated: Vec<(usize, TypedPattern, TypedExpr)>,
}

/// Splits `bindings` into SCCs and builds one nested `Constraint::Let` per
/// group. `finish` produces whatever comes after every group (nothing for
/// a module, the `in` body for a `let`) and runs while every group's names
/// are visible.
fn constrain_bindings<R>(
    sub: &mut Substitution,
    scope: &mut LocalScope,
    bindings: &[RawBinding],
    declared: &Declared,
    top_level: bool,
    finish: impl FnOnce(&mut Substitution, &mut LocalScope) -> (R, Constraint),
) -> (R, ChainOutput) {
    let mut owner: HashMap<&str, usize> = HashMap::new();
    for (i, b) in bindings.iter().enumerate() {
        if declared.contains_key(&i) {
            continue;
        }
        for n in &b.names {
            owner.insert(n.as_str(), i);
        }
    }
    let pick: fn(&Ref) -> Option<String> = if top_level {
        |r| match r {
            Ref::TopLevel(n) => Some(n.clone()),
            _ => None,
        }
    } else {
        |r| match r {
            Ref::Local(n) => Some(n.clone()),
            _ => None,
        }
    };
    let edges: Vec<Vec<usize>> = bindings
        .iter()
        .map(|b| {
            let mut refs = HashSet::new();
            for p in b.params {
                collect_pattern_refs(&p.node, &pick, &mut refs);
            }
            collect_refs(&b.body.node, &pick, &mut refs);
            let mut deps: Vec<usize> = refs
                .iter()
                .filter_map(|n| owner.get(n.as_str()).copied())
                .collect();
            deps.sort();
            deps.dedup();
            deps
        })
        .collect();
    let sccs = scc_order(bindings.len(), &edges);
    constrain_group_chain(sub, scope, bindings, declared, &sccs, top_level, finish)
}

fn constrain_group_chain<R>(
    sub: &mut Substitution,
    scope: &mut LocalScope,
    bindings: &[RawBinding],
    declared: &Declared,
    sccs: &[Vec<usize>],
    top_level: bool,
    finish: impl FnOnce(&mut Substitution, &mut LocalScope) -> (R, Constraint),
) -> (R, ChainOutput) {
    let mut groups = Vec::with_capacity(sccs.len());
    let mut all_members = Vec::new();
    let mut all_elaborated = Vec::new();
    for group in sccs {
        let level = sub.level();
        sub.enter_level();
        scope.push();

        // Headers first, so self and mutual references inside the group resolve.
        let mut header_constraints = Vec::new();
        let mut headers: Vec<(TypeVarId, Option<TypedPattern>)> = Vec::new();
        for &i in group {
            let b = &bindings[i];
            match &b.lhs {
                Lhs::Name(name) => {
                    let ty = match declared.get(&i) {
                        Some((ty, _)) => *ty,
                        None => {
                            let ty = sub.fresh_unbound();
                            scope.bind(name, ty);
                            ty
                        }
                    };
                    headers.push((ty, None));
                }
                Lhs::Pattern(p) => {
                    let typed = constrain_pattern(sub, scope, p, &mut header_constraints);
                    headers.push((typed.ty, Some(typed)));
                }
            }
        }

        let mut members = Vec::new();
        let mut elaborated = Vec::new();
        let mut pattern_tys = Vec::new();
        for (&i, (header_ty, typed_pattern)) in group.iter().zip(headers) {
            let b = &bindings[i];
            scope.push();
            let mut body_constraints = Vec::new();
            let typed_params: Vec<TypedPattern> = b
                .params
                .iter()
                .map(|p| constrain_pattern(sub, scope, p, &mut body_constraints))
                .collect();
            let typed_body = constrain_expr(sub, scope, b.body, &mut body_constraints);
            scope.pop();
            let inferred = typed_params.iter().rev().fold(typed_body.ty, |acc, p| {
                sub.fresh_bound(Structure::Fn(p.ty, acc))
            });
            header_constraints.push(Constraint::Equal {
                span: b.span,
                expected: header_ty,
                actual: inferred,
            });
            header_constraints.extend(body_constraints);
            let value = if typed_params.is_empty() {
                typed_body
            } else {
                Typed {
                    id: sub.fresh_node(),
                    span: b.span,
                    ty: header_ty,
                    node: TExpr::Lambda(typed_params, Box::new(typed_body)),
                }
            };
            match (&b.lhs, typed_pattern) {
                (Lhs::Name(name), _) => {
                    elaborated.push((
                        i,
                        Typed {
                            id: sub.fresh_node(),
                            span: b.span,
                            ty: header_ty,
                            node: TPattern::Var(name.clone()),
                        },
                        value.clone(),
                    ));
                    members.push(LetMember {
                        name: name.clone(),
                        span: b.span,
                        header_ty,
                        declared: declared.get(&i).map(|(_, d)| d.clone()),
                        elaborated_body: value,
                    });
                }
                (Lhs::Pattern(_), Some(pattern)) => {
                    pattern_tys.push(header_ty);
                    elaborated.push((i, pattern, value));
                }
                (Lhs::Pattern(_), None) => unreachable!("pattern headers carry their pattern"),
            }
        }
        sub.exit_level();

        // Top-level names are visible to later groups only through `Lookup`
        // (each use instantiates the generalized scheme); `let` names stay in
        // scope but switch to `LookupLocal` for the same reason.
        if top_level {
            scope.pop();
        } else {
            for m in &members {
                scope.promote_to_generalizable(&m.name);
            }
        }
        all_members.extend(members.iter().cloned());
        all_elaborated.extend(elaborated);
        groups.push((level, members, pattern_tys, header_constraints));
    }
    let (r, mut constraint) = finish(sub, scope);
    // Close lexical frames and build the same nested constraints without one
    // native call frame (or copying the entire remaining output) per SCC.
    for (level, members, pattern_tys, header_constraints) in groups.into_iter().rev() {
        if !top_level {
            scope.pop();
        }
        constraint = Constraint::Let {
            level,
            top_level,
            generalize: pattern_tys.is_empty(),
            members,
            pattern_tys,
            header_con: Box::new(Constraint::And(header_constraints)),
            body_con: Box::new(constraint),
        };
    }
    (
        r,
        ChainOutput {
            constraint,
            members: all_members,
            elaborated: all_elaborated,
        },
    )
}

/// Checks every method of `inst` against its interface's declared method
/// signature — see module docs. Unknown, duplicate, and missing methods
/// are reported separately (`interface::instance::check_instance_methods`);
/// an unknown one is simply not checked here.
fn constrain_instance(
    sub: &mut Substitution,
    inst: &CInstanceDecl,
) -> (Constraint, Option<ConstrainedInstance>) {
    let Some(iface) = interface(&inst.interface) else {
        return (Constraint::True, None);
    };
    let level = sub.level();
    sub.enter_level();

    if inst.targets.len() != iface.vars.len() {
        sub.exit_level();
        return (Constraint::True, None);
    }
    let mut rigids = HashMap::new();
    let mut class_bindings = HashMap::new();
    let mut target_args = Vec::new();
    for (position, (var, ty)) in iface.vars.iter().zip(&inst.targets).enumerate() {
        let binding = if crate::interface::table::parameter_arity(&inst.interface, position) == 0 {
            VarBinding::Ty(instantiate_rigid(sub, ty, &mut rigids))
        } else {
            let CType::Named(r, args) = ty else {
                sub.exit_level();
                return (Constraint::True, None);
            };
            let leading = args
                .iter()
                .map(|a| instantiate_rigid(sub, a, &mut rigids))
                .collect();
            VarBinding::Ctor(r.clone(), leading)
        };
        let arg = match &binding {
            VarBinding::Ty(ty) => *ty,
            VarBinding::Ctor(head, args) => {
                sub.fresh_bound(Structure::Ctor(head.clone(), args.clone()))
            }
        };
        target_args.push(arg);
        class_bindings.insert(var.clone(), binding);
    }
    let target = crate::predicate::pack(sub, target_args);
    let context = crate::interface::instance::ordered_context(&inst.targets, &inst.constraints)
        .into_iter()
        .map(|c| {
            let args = c
                .type_vars
                .iter()
                .map(|v| rigid_var(sub, &mut rigids, v))
                .collect();
            (crate::predicate::pack(sub, args), c.interface)
        })
        .collect::<Vec<_>>();
    let mut method_constraints = Vec::new();
    let mut methods = Vec::new();
    for method in &inst.methods {
        let Some(sig) = method_signature(&iface.name, &method.name) else {
            continue;
        };
        let mut vars = class_bindings.clone();
        let mut method_vars = Vec::new();
        let expected = build(sub, &sig.ty, &mut vars, &mut |sub, name| {
            let ty = sub.fresh_rigid(name.to_string());
            method_vars.push(ty);
            ty
        });
        let constraints: Vec<_> = sig
            .constraints
            .iter()
            .filter_map(|c| {
                crate::sig::constraint_args(sub, &vars, &c.type_vars)
                    .map(|ty| (ty, c.interface.clone()))
            })
            .collect();
        let (body_constraint, body) = constrain_method_body_against(sub, method, expected);
        let mut given = context.clone();
        given.extend(constraints.clone());
        method_constraints.push(Constraint::Given {
            level,
            given,
            body: Box::new(body_constraint),
        });
        methods.push(ConstrainedMethod {
            name: method.name.clone(),
            scheme: Scheme {
                vars: method_vars,
                constraints,
                ty: expected,
            },
            body,
        });
    }
    sub.exit_level();
    (
        Constraint::And(method_constraints),
        Some(ConstrainedInstance {
            target,
            context,
            methods,
        }),
    )
}

/// An interface's default method bodies, each checked against its method's
/// declared type with the interface's own variable rigid and the interface
/// itself given (`(/=) x y = not (x == y)` may use `Eq a`), as Haskell
/// checks a class's default methods.
fn constrain_defaults(
    sub: &mut Substitution,
    iface: &CInterfaceDecl,
) -> (Constraint, Vec<ConstrainedMethod>) {
    let level = sub.level();
    sub.enter_level();
    let mut method_constraints = Vec::new();
    let mut methods = Vec::new();
    for default in &iface.defaults {
        let Some(sig) = method_signature(&iface.name, &default.name) else {
            continue;
        };
        let mut method_vars = Vec::new();
        let mut vars = HashMap::new();
        for name in &iface.vars {
            let ty = sub.fresh_rigid(name.clone());
            method_vars.push(ty);
            vars.insert(name.clone(), VarBinding::Ty(ty));
        }
        let class_args = crate::predicate::pack(sub, method_vars.clone());
        let mut given = vec![(class_args, iface.name.clone())];
        let expected = build(sub, &sig.ty, &mut vars, &mut |sub, name| {
            let ty = sub.fresh_rigid(name.to_string());
            method_vars.push(ty);
            ty
        });
        for c in &sig.constraints {
            if let Some(v) = crate::sig::constraint_args(sub, &vars, &c.type_vars) {
                given.push((v, c.interface.clone()));
            }
        }
        let (body_constraint, body) = constrain_method_body_against(sub, default, expected);
        method_constraints.push(Constraint::Given {
            level,
            given: given.clone(),
            body: Box::new(body_constraint),
        });
        methods.push(ConstrainedMethod {
            name: default.name.clone(),
            scheme: Scheme {
                vars: method_vars,
                constraints: given,
                ty: expected,
            },
            body,
        });
    }
    sub.exit_level();
    (Constraint::And(method_constraints), methods)
}

/// Constrains one method's params and body against `expected_ty`, the same
/// way a signed binding's body is checked against its signature.
fn constrain_method_body_against(
    sub: &mut Substitution,
    method: &CFnDef,
    expected_ty: TypeVarId,
) -> (Constraint, TypedExpr) {
    let mut scope = LocalScope::new();
    scope.push();
    let mut body_constraints = Vec::new();
    let typed_params: Vec<TypedPattern> = method
        .params
        .iter()
        .map(|p| constrain_pattern(sub, &mut scope, p, &mut body_constraints))
        .collect();
    let typed_body = constrain_expr(sub, &mut scope, &method.body, &mut body_constraints);
    scope.pop();
    let inferred = typed_params.iter().rev().fold(typed_body.ty, |acc, p| {
        sub.fresh_bound(Structure::Fn(p.ty, acc))
    });
    let mut constraints = vec![Constraint::Equal {
        span: method.body.span,
        expected: expected_ty,
        actual: inferred,
    }];
    constraints.extend(body_constraints);
    let body = if typed_params.is_empty() {
        typed_body
    } else {
        Typed {
            id: sub.fresh_node(),
            span: method.body.span,
            ty: expected_ty,
            node: TExpr::Lambda(typed_params, Box::new(typed_body)),
        }
    };
    (Constraint::And(constraints), body)
}

/// Retained bodies from the same constraint-generation pass used for checking.
pub struct ConstrainedMethod {
    pub name: String,
    pub scheme: Scheme,
    pub body: TypedExpr,
}

pub struct ConstrainedInstance {
    pub target: TypeVarId,
    pub context: Vec<(TypeVarId, String)>,
    pub methods: Vec<ConstrainedMethod>,
}

pub struct ConstrainedModule {
    pub constraint: Constraint,
    pub members: Vec<LetMember>,
    /// Indices into the canonical declaration list keep declarations and
    /// their retained bodies associated without matching by source spans.
    pub instances: Vec<(usize, ConstrainedInstance)>,
    pub defaults: Vec<(usize, Vec<ConstrainedMethod>)>,
}

/// The whole-module entry point. Signed bindings' declared schemes are
/// installed first; every `CDecl::Fn` is then grouped and constrained;
/// every instance's methods are checked alongside. Returns the constraint
/// tree and every top-level binding as a `LetMember`.
pub fn constrain_module(
    sub: &mut Substitution,
    decls: &[Spanned<CDecl>],
) -> (Constraint, Vec<LetMember>) {
    let output = constrain_program(sub, decls);
    (output.constraint, output.members)
}

pub fn constrain_program(sub: &mut Substitution, decls: &[Spanned<CDecl>]) -> ConstrainedModule {
    let fndefs: Vec<(&CFnDef, Span)> = decls
        .iter()
        .filter_map(|d| match &d.node {
            CDecl::Fn(f) => Some((f, d.span)),
            _ => None,
        })
        .collect();
    let bindings: Vec<RawBinding> = fndefs
        .iter()
        .map(|(f, span)| RawBinding {
            lhs: Lhs::Name(f.name.clone()),
            names: vec![f.name.clone()],
            span: *span,
            signature: f.signature.as_ref(),
            params: &f.params,
            body: &f.body,
        })
        .collect();

    let mut declared: Declared = HashMap::new();
    let mut installs = Vec::new();
    sub.enter_level();
    for (i, b) in bindings.iter().enumerate() {
        if let Some(sig) = b.signature {
            let (ty, d) = build_declared(sub, &sig.node);
            installs.push((
                SchemeKey::TopLevel(b.names[0].clone()),
                Scheme {
                    vars: d.rigid_vars.clone(),
                    constraints: d.given.clone(),
                    ty,
                },
            ));
            declared.insert(i, (ty, d));
        }
    }
    sub.exit_level();

    let mut scope = LocalScope::new();
    let ((), out) = constrain_bindings(sub, &mut scope, &bindings, &declared, true, |_, _| {
        ((), Constraint::True)
    });

    let mut all = vec![Constraint::Install(installs), out.constraint];
    let mut instances = Vec::new();
    let mut defaults = Vec::new();
    for (index, d) in decls.iter().enumerate() {
        match &d.node {
            CDecl::Instance(inst) => {
                let (constraint, retained) = constrain_instance(sub, inst);
                all.push(constraint);
                if let Some(retained) = retained {
                    instances.push((index, retained));
                }
            }
            CDecl::Interface(iface) => {
                let (constraint, methods) = constrain_defaults(sub, iface);
                all.push(constraint);
                defaults.push((index, methods));
            }
            _ => {}
        }
    }
    ConstrainedModule {
        constraint: Constraint::And(all),
        members: out.members,
        instances,
        defaults,
    }
}

/// A `let` expression's bindings plus its body (built by `constrain_body`
/// with every binding in scope). Returns the elaborated `TExpr::Let` and its
/// constraint tree.
pub fn constrain_let_bindings(
    sub: &mut Substitution,
    scope: &mut LocalScope,
    bindings: &[(Spanned<CPattern>, Spanned<CExpr>)],
    constrain_body: impl FnOnce(&mut Substitution, &mut LocalScope, &mut Vec<Constraint>) -> TypedExpr,
) -> (TypedExpr, Constraint) {
    let raw: Vec<RawBinding> = bindings
        .iter()
        .map(|(p, e)| {
            let mut names = Vec::new();
            pattern_names(&p.node, &mut names);
            let lhs = match &p.node {
                CPattern::Var(name) => Lhs::Name(name.clone()),
                _ => Lhs::Pattern(p),
            };
            RawBinding {
                lhs,
                names,
                span: p.span,
                signature: None,
                params: &[],
                body: e,
            }
        })
        .collect();
    let (typed_body, mut out) =
        constrain_bindings(sub, scope, &raw, &HashMap::new(), false, |sub, scope| {
            let mut constraints = Vec::new();
            let body = constrain_body(sub, scope, &mut constraints);
            (body, Constraint::And(constraints))
        });
    out.elaborated.sort_by_key(|(index, _, _)| *index);
    let elaborated = Typed {
        id: sub.fresh_node(),
        span: typed_body.span,
        ty: typed_body.ty,
        node: TExpr::Let(
            out.elaborated
                .into_iter()
                .map(|(_, pattern, value)| (pattern, value))
                .collect(),
            Box::new(typed_body),
        ),
    };
    (elaborated, out.constraint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::check_source;

    fn decls(src: &str) -> Vec<Spanned<CDecl>> {
        let mut state = knot_syntax::ParseState::new(src);
        let raw = state.parse_decls().unwrap();
        assert!(state.is_eof(), "leftover input: {src}");
        knot_canonical::canonicalize_decls(&raw).unwrap_or_else(|errs| panic!("{errs:?}"))
    }

    /// The module's binding-group chain, outermost first, as member names.
    fn groups(src: &str) -> Vec<Vec<String>> {
        let mut sub = Substitution::new();
        let (tree, _) = constrain_module(&mut sub, &decls(src));
        let Constraint::And(parts) = tree else {
            panic!("expected And")
        };
        let mut out = Vec::new();
        let mut c = &parts[1];
        while let Constraint::Let {
            members,
            body_con,
            level,
            ..
        } = c
        {
            assert_eq!(*level, 0, "top-level groups sit at level 0");
            out.push(members.iter().map(|m| m.name.clone()).collect());
            c = body_con;
        }
        out
    }

    fn type_of(src: &str, name: &str) -> String {
        let report = check_source(src);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        report.types.into_iter().find(|(n, _)| n == name).unwrap().1
    }

    #[test]
    fn dependencies_become_outer_groups_and_mutual_recursion_shares_one() {
        assert_eq!(
            groups("useHelper y = helper y\nhelper x = x\n"),
            vec![vec!["helper".to_string()], vec!["useHelper".to_string()]]
        );
        let g = groups(
            "isOdd n = if n == 0 then False else isEven (n - 1)\n\
             isEven n = if n == 0 then True else isOdd (n - 1)\n",
        );
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].len(), 2);
    }

    #[test]
    fn signed_bindings_are_not_dependency_targets() {
        // `f` is signed, so `g` referring to it doesn't put them in one
        // group, even though they're mutually recursive.
        let g = groups("f :: Int -> Int\nf x = g x\ng y = f y\n");
        assert_eq!(g.len(), 2, "{g:?}");
    }

    #[test]
    fn mutually_recursive_functions_generalize_together() {
        let src = "isOdd n = if n == 0 then False else isEven (n - 1)\n\
                   isEven n = if n == 0 then True else isOdd (n - 1)\n";
        assert_eq!(type_of(src, "isOdd"), "(Eq a, Num a) => a -> Bool");
        assert_eq!(type_of(src, "isEven"), "(Eq a, Num a) => a -> Bool");
    }

    #[test]
    fn a_signed_binding_may_recurse_polymorphically() {
        let src = "type Nested a = Flat a | Nest (Nested (List a))\n\
                   depth :: Nested a -> Int\n\
                   depth n = case n of\n  Flat _ -> 0\n  Nest inner -> 1 + depth inner\n";
        assert_eq!(type_of(src, "depth"), "Nested a -> Int");
    }

    #[test]
    fn long_dependency_chains_preserve_local_polymorphism() {
        let mut source = String::from("z =\n  let\n");
        for i in (1..=800).rev() {
            source.push_str(&format!("    identity{i} = identity{}\n", i - 1));
        }
        source.push_str("    identity0 x = x\n  in (identity800 True, identity800 \"ok\")\n");
        assert_eq!(type_of(&source, "z"), "(Bool, String)");
    }

    #[test]
    fn let_bindings_may_refer_forward_and_recurse_mutually() {
        let src = "z =\n  let\n    f = g 1\n    g = \\x -> x + 1\n  in\n    f\n";
        assert_eq!(type_of(src, "z"), "Int");
        let src = "z =\n  let\n    ev = \\n -> if n == 0 then True else od (n - 1)\n    od = \\n -> if n == 0 then False else ev (n - 1)\n  in\n    ev 10\n";
        assert_eq!(type_of(src, "z"), "Bool");
    }

    #[test]
    fn let_pattern_bindings_see_and_are_seen_by_their_siblings() {
        let src = "z =\n  let\n    (a, b) = (1, 2)\n    f = a + b\n  in\n    f\n";
        assert_eq!(type_of(src, "z"), "Int");
        let src = "z =\n  let\n    f = 1\n    (a, b) = (f, \"s\")\n  in\n    b\n";
        assert_eq!(type_of(src, "z"), "String");
    }

    #[test]
    fn let_pattern_bindings_are_monomorphic() {
        let src = "z =\n  let\n    (a, b) = (\\x -> x, 1)\n  in\n    (a 1, a True)\n";
        assert!(!check_source(src).errors.is_empty());
    }

    #[test]
    fn a_let_inside_a_lambda_never_generalizes_the_lambdas_parameter() {
        let report = check_source("bad = (\\x -> let y = x in y + 1) \"hello\"\n");
        assert!(!report.errors.is_empty());
        assert_eq!(
            type_of("f = \\x -> let g = \\y -> (x, y) in (g 1, g True)\n", "f"),
            "Num b => a -> ((a, b), (a, Bool))"
        );
    }

    #[test]
    fn user_constructors_are_usable_and_polymorphic() {
        let src = "type MyResult e a = MyOk a | MyErr e\n\
                   useAtInt = case MyErr \"boom\" of\n  MyOk x -> x\n  MyErr _ -> 0\n\
                   useAtBool = case MyErr \"boom\" of\n  MyOk x -> x\n  MyErr _ -> True\n";
        assert_eq!(type_of(src, "useAtInt"), "Int");
        assert_eq!(type_of(src, "useAtBool"), "Bool");
        assert_eq!(type_of("type Box a = Box a\nf = Box\n", "f"), "a -> Box a");
    }

    #[test]
    fn extensible_record_aliases_apply_to_concrete_records() {
        let src = "type alias Foo = { name : String }\n\
                   type alias Selectable a = { a | isSelected : Bool }\n\
                   makeSelectableFoo :: Selectable Foo\n\
                   makeSelectableFoo =\n  { name = \"Bar\", isSelected = True }\n";
        assert_eq!(
            type_of(src, "makeSelectableFoo"),
            "{ isSelected : Bool, name : String }"
        );
        let bad = "type alias Foo = { name : String }\n\
                   type alias Selectable a = { a | isSelected : Bool }\n\
                   getFooName :: Foo -> String\n\
                   getFooName foo = foo.name\n\
                   makeSelectableFoo :: Selectable Foo\n\
                   makeSelectableFoo =\n  { name = \"Bar\", isSelected = True }\n\
                   getMyFooName = getFooName makeSelectableFoo\n";
        assert_eq!(check_source(bad).errors.len(), 1);
    }
}
