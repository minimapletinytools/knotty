//! Dictionary resolution for a solved module: for each interface
//! obligation a typed body carries (recovered through
//! `solve::ObligationMap`), which instance answers it. Not yet wired into
//! the diagnostic `check_module_report` API. Executable output uses
//! `program`'s binding-scoped traversal and the same shared resolver.
//!
//! Uses the same evidence resolver as constraint solving and superclass
//! validation. This older per-type-variable side table remains useful for
//! inspection; lowering should consume `check::check_program` instead.
use std::collections::{HashMap, HashSet};

use knot_syntax::span::Span;

use crate::ast::{ObligationResolution, TDoStmt, TExpr, TypedExpr};
use crate::constrain::LetMember;
use crate::error::TypeError;
use crate::interface::instance::{reduce_error_kind, resolve, InstanceTable};
use crate::solve::ObligationMap;
use crate::var::{Substitution, TypeVarId};

/// Resolves every interface obligation reachable from `members`' bodies,
/// keyed by `(interface, root type variable)`. `obligations` is the solver's
/// side table mapping each lookup to its instantiated obligations.
pub fn elaborate_module(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    obligations: &ObligationMap,
    members: &[LetMember],
) -> (
    HashMap<(String, TypeVarId), ObligationResolution>,
    Vec<TypeError>,
) {
    let mut resolved = HashMap::new();
    let mut errors = Vec::new();
    for m in members {
        walk_expr(
            sub,
            table,
            given,
            obligations,
            &m.elaborated_body,
            &mut resolved,
            &mut errors,
        );
    }
    (resolved, errors)
}

/// Preserve the complete proof, including residuals, using the shared resolver.
#[allow(clippy::too_many_arguments)]
fn resolve_one(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    interface: &str,
    ty: TypeVarId,
    span: Span,
    resolved: &mut HashMap<(String, TypeVarId), ObligationResolution>,
    errors: &mut Vec<TypeError>,
) {
    let key = (interface.to_string(), sub.find(ty));
    if resolved.contains_key(&key) {
        return;
    }
    match resolve(sub, table, given, interface, ty) {
        Ok(evidence) => {
            resolved.insert(key, evidence);
        }
        Err(e) => errors.push(TypeError {
            span,
            kind: reduce_error_kind(sub, e),
        }),
    }
}

/// Resolves every obligation the lookup typed `key` instantiated.
#[allow(clippy::too_many_arguments)]
fn resolve_lookup(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    obligations: &ObligationMap,
    key: TypeVarId,
    span: Span,
    resolved: &mut HashMap<(String, TypeVarId), ObligationResolution>,
    errors: &mut Vec<TypeError>,
) {
    if let Some(pairs) = obligations.get(&key) {
        for (interface, ty) in pairs.clone() {
            resolve_one(sub, table, given, &interface, ty, span, resolved, errors);
        }
    }
}

/// Walks every `TExpr` node reachable from `typed`, resolving whatever
/// obligations it carries -- either directly on the node (`BinOp`/
/// `Negate`) or via `obligations`, keyed by the node's own `ty` (`Var`/
/// `Ctor`). `TPattern` is never walked: per `ast.rs`'s own doc comment, a
/// data constructor's scheme never has `constraints` in this language, so a
/// pattern can never carry an obligation to resolve in the first place.
fn walk_expr(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    obligations: &ObligationMap,
    typed: &TypedExpr,
    resolved: &mut HashMap<(String, TypeVarId), ObligationResolution>,
    errors: &mut Vec<TypeError>,
) {
    let mut recurse = |e: &TypedExpr, resolved: &mut _, errors: &mut _| {
        walk_expr(sub, table, given, obligations, e, resolved, errors)
    };
    match &typed.node {
        TExpr::IntLit(_) | TExpr::FloatLit(_) => {
            let interface = if matches!(typed.node, TExpr::IntLit(_)) {
                "Num"
            } else {
                "Fractional"
            };
            resolve_one(
                sub, table, given, interface, typed.ty, typed.span, resolved, errors,
            );
        }
        TExpr::StringLit(_) | TExpr::Unit | TExpr::Hole => {}
        TExpr::Var(_) | TExpr::Ctor(_) => {
            resolve_lookup(
                sub,
                table,
                given,
                obligations,
                typed.ty,
                typed.span,
                resolved,
                errors,
            );
        }
        TExpr::Lambda(_, body) => recurse(body, resolved, errors),
        TExpr::App(f, arg) => {
            recurse(f, resolved, errors);
            recurse(arg, resolved, errors);
        }
        TExpr::BinOp(_, l, r, op_ty) => {
            recurse(l, resolved, errors);
            recurse(r, resolved, errors);
            resolve_lookup(
                sub,
                table,
                given,
                obligations,
                *op_ty,
                typed.span,
                resolved,
                errors,
            );
        }
        TExpr::Negate(inner, op_ty) => {
            recurse(inner, resolved, errors);
            resolve_lookup(
                sub,
                table,
                given,
                obligations,
                *op_ty,
                typed.span,
                resolved,
                errors,
            );
        }
        TExpr::If(c, t, e) => {
            recurse(c, resolved, errors);
            recurse(t, resolved, errors);
            recurse(e, resolved, errors);
        }
        TExpr::Let(bindings, body) => {
            for (_, rhs) in bindings {
                recurse(rhs, resolved, errors);
            }
            recurse(body, resolved, errors);
        }
        TExpr::Case(scrutinee, arms) => {
            recurse(scrutinee, resolved, errors);
            for (_, body) in arms {
                recurse(body, resolved, errors);
            }
        }
        TExpr::Do(stmts, body) => {
            for stmt in stmts {
                match stmt {
                    TDoStmt::Bind(_, e, bind_ty) | TDoStmt::Expr(e, bind_ty) => {
                        walk_expr(sub, table, given, obligations, e, resolved, errors);
                        resolve_lookup(
                            sub,
                            table,
                            given,
                            obligations,
                            *bind_ty,
                            e.span,
                            resolved,
                            errors,
                        );
                    }
                    TDoStmt::Let(bindings) => {
                        for (_, e) in bindings {
                            walk_expr(sub, table, given, obligations, e, resolved, errors);
                        }
                    }
                }
            }
            walk_expr(sub, table, given, obligations, body, resolved, errors);
        }
        TExpr::List(elems) | TExpr::Tuple(elems) => {
            for el in elems {
                recurse(el, resolved, errors);
            }
        }
        TExpr::Record(fields) => {
            for (_, e) in fields {
                recurse(e, resolved, errors);
            }
        }
        TExpr::RecordUpdate(base, updates) => {
            recurse(base, resolved, errors);
            for (_, e) in updates {
                recurse(e, resolved, errors);
            }
        }
        TExpr::FieldAccess(base, _) => recurse(base, resolved, errors),
        TExpr::Annotated(_, target) => recurse(target, resolved, errors),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::Dictionary;
    use crate::error::TypeErrorKind;
    use crate::interface::instance::Evidence;
    use knot_canonical::ast::{CDecl, Ref};
    use knot_syntax::span::Spanned;

    fn decls(src: &str) -> Vec<Spanned<CDecl>> {
        let mut state = knot_syntax::ParseState::new(src);
        let raw = state.parse_decls().unwrap();
        assert!(state.is_eof(), "leftover input: {src}");
        knot_canonical::canonicalize_decls(&raw).unwrap_or_else(|errs| panic!("{errs:?}"))
    }

    /// The first obligation the `BinOp` inside a binding's body recorded.
    fn binop_obligation(typed: &TypedExpr, obligations: &ObligationMap) -> (String, TypeVarId) {
        match &typed.node {
            TExpr::Lambda(_, body) => binop_obligation(body, obligations),
            TExpr::BinOp(_, _, _, op_ty) => obligations[op_ty][0].clone(),
            other => panic!("expected a Lambda wrapping a BinOp, got {other:?}"),
        }
    }

    struct Solved {
        sub: Substitution,
        table: InstanceTable,
        result: crate::solve::SolveResult,
        members: Vec<crate::constrain::LetMember>,
    }

    /// The real pipeline up to solving, against the full prelude.
    fn solved(src: &str) -> Solved {
        let cs = decls(src);
        let mut sub = Substitution::new();
        let (mut env, table) = crate::prelude::seed(&mut sub);
        crate::constrain::decl::seed_user_constructors(&mut sub, &mut env, &cs);
        let (tree, members) = crate::constrain::decl::constrain_module(&mut sub, &cs);
        let result = crate::solve::solve_module(&mut sub, &mut env, &table, &tree);
        Solved {
            sub,
            table,
            result,
            members,
        }
    }

    // -- elaborate_module --

    #[test]
    fn a_concrete_signatures_binop_obligation_resolves_directly() {
        let mut st = solved("check :: Int -> Int -> Bool\ncheck x y = x == y\n");
        assert!(st.result.errors.is_empty(), "{:?}", st.result.errors);
        let (resolved, elab_errors) = elaborate_module(
            &mut st.sub,
            &st.table,
            &st.result.given,
            &st.result.obligations,
            &st.members,
        );
        assert!(elab_errors.is_empty(), "{elab_errors:?}");
        let check = st.members.iter().find(|m| m.name == "check").unwrap();
        let (interface, ty) = binop_obligation(&check.elaborated_body, &st.result.obligations);
        let key = (interface, st.sub.find(ty));
        assert_eq!(
            resolved.get(&key),
            Some(&Evidence::Instance {
                dictionary: Dictionary {
                    interface: "Eq".to_string(),
                    head: Ref::Builtin("Int".to_string()),
                },
                args: vec![]
            })
        );
    }

    #[test]
    fn a_polymorphic_bindings_own_body_obligation_is_still_abstract() {
        let mut st = solved("useEq x y = x == y\n");
        assert!(st.result.errors.is_empty(), "{:?}", st.result.errors);
        let (resolved, elab_errors) = elaborate_module(
            &mut st.sub,
            &st.table,
            &st.result.given,
            &st.result.obligations,
            &st.members,
        );
        assert!(elab_errors.is_empty(), "{elab_errors:?}");
        let use_eq = st.members.iter().find(|m| m.name == "useEq").unwrap();
        let (interface, ty) = binop_obligation(&use_eq.elaborated_body, &st.result.obligations);
        let key = (interface, st.sub.find(ty));
        assert_eq!(
            resolved.get(&key),
            Some(&Evidence::Residual {
                interface: "Eq".to_string(),
                obligation: crate::interface::instance::Residual::Var(key.1)
            })
        );
    }

    fn reference_obligation(st: &mut Solved, name: &str) -> (String, TypeVarId) {
        let member = st.members.iter().find(|m| m.name == name).unwrap();
        let f_ref_ty = match &member.elaborated_body.node {
            TExpr::App(inner, _) => match &inner.node {
                TExpr::App(f_ref, _) => f_ref.ty,
                other => panic!("expected App(Var(f), _), got {other:?}"),
            },
            other => panic!("expected App(f _, _), got {other:?}"),
        };
        let pairs = st
            .result
            .obligations
            .get(&f_ref_ty)
            .expect("recorded obligation");
        assert_eq!(pairs.len(), 1);
        let (interface, ty) = pairs[0].clone();
        (interface, st.sub.find(ty))
    }

    #[test]
    fn a_call_sites_lookup_obligation_resolves_via_the_side_table() {
        let mut st = solved("f x y = x == y\nresult = f 1 2\n");
        assert!(st.result.errors.is_empty(), "{:?}", st.result.errors);
        let (resolved, elab_errors) = elaborate_module(
            &mut st.sub,
            &st.table,
            &st.result.given,
            &st.result.obligations,
            &st.members,
        );
        assert!(elab_errors.is_empty(), "{elab_errors:?}");
        let key = reference_obligation(&mut st, "result");
        assert_eq!(
            resolved.get(&key),
            Some(&Evidence::Instance {
                dictionary: Dictionary {
                    interface: "Eq".to_string(),
                    head: Ref::Builtin("Int".to_string()),
                },
                args: vec![]
            })
        );
    }

    #[test]
    fn a_valid_tuple_equality_elaborates_as_structural() {
        let mut st = solved("f x y = x == y\nresult = f (1, 3) (2, 4)\n");
        assert!(st.result.errors.is_empty(), "{:?}", st.result.errors);
        let (resolved, elab_errors) = elaborate_module(
            &mut st.sub,
            &st.table,
            &st.result.given,
            &st.result.obligations,
            &st.members,
        );
        assert!(elab_errors.is_empty(), "{elab_errors:?}");
        let key = reference_obligation(&mut st, "result");
        assert!(
            matches!(resolved.get(&key), Some(Evidence::Structural { shape: crate::interface::instance::StructuralShape::Tuple(2), args, .. }) if args.len() == 2)
        );
    }

    #[test]
    fn a_genuinely_missing_instance_is_a_real_error() {
        let mut st = solved("bad = LT + EQ\n");
        assert!(st.result.errors.iter().any(
            |e| matches!(&e.kind, TypeErrorKind::NoInstance { interface, .. } if interface == "Num")
        ));
        let (_resolved, elab_errors) = elaborate_module(
            &mut st.sub,
            &st.table,
            &st.result.given,
            &st.result.obligations,
            &st.members,
        );
        assert!(elab_errors.iter().any(
            |e| matches!(&e.kind, TypeErrorKind::NoInstance { interface, .. } if interface == "Num")
        ));
    }
}
