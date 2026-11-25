//! Constraint generation over `CExpr`. Each function returns the
//! expression's inferred type wrapped in a `TypedExpr`; relating two types
//! is always done by pushing a `Constraint::Equal`, never by calling
//! `unify` directly.
//!
//! Operators have no special typing rules: `a + b` looks up the scheme of
//! whatever `+` resolved to (the prelude's, or a user-defined operator's)
//! and applies it, exactly like calling a function named `+`. Unary minus
//! is an application of the prelude's `negate`.
//!
//! `Let` delegates to `constrain::decl::constrain_let_bindings`. `Do`
//! is checked against `(>>=)` without rewriting its graph-facing shape.
//!
//! Annotation values aren't constrained yet: `Annotated` looks straight
//! through to its target.

use knot_canonical::ast::{CDoStmt, CExpr, CPattern, Ref};
use knot_syntax::span::{Span, Spanned};

use crate::ast::{TDoStmt, TExpr, Typed, TypedExpr};
use crate::constrain::pattern::constrain_pattern;
use crate::constrain::{Constraint, LocalBinding, LocalScope};
use crate::ty::Structure;
use crate::var::{Substitution, TypeVarId};

fn app0(sub: &mut Substitution, name: &str) -> TypeVarId {
    sub.fresh_bound(Structure::App(Ref::Builtin(name.to_string()), vec![]))
}

fn app1(sub: &mut Substitution, name: &str, arg: TypeVarId) -> TypeVarId {
    sub.fresh_bound(Structure::App(Ref::Builtin(name.to_string()), vec![arg]))
}

/// A fresh variable constrained to be the named prelude value's type.
fn lookup_builtin(
    sub: &mut Substitution,
    name: &str,
    span: Span,
    constraints: &mut Vec<Constraint>,
) -> TypeVarId {
    let ty = sub.fresh_unbound();
    constraints.push(Constraint::Lookup {
        span,
        reference: Ref::Builtin(name.to_string()),
        expected: ty,
    });
    ty
}

/// Applies a looked-up function type to `args`, returning the result type.
fn apply(
    sub: &mut Substitution,
    fn_ty: TypeVarId,
    args: &[TypeVarId],
    span: Span,
    constraints: &mut Vec<Constraint>,
) -> TypeVarId {
    let result = sub.fresh_unbound();
    let expected = args
        .iter()
        .rev()
        .fold(result, |acc, &a| sub.fresh_bound(Structure::Fn(a, acc)));
    constraints.push(Constraint::Equal {
        span,
        expected: fn_ty,
        actual: expected,
    });
    result
}

/// Resolves a `Var`/`Ctor` reference. `Ref::Local` covers three cases (see
/// `LocalBinding`): a lambda/case/do param or a group's own self/mutual
/// reference is `Monomorphic` — resolves immediately, no constraint needed;
/// a `let`-bound name that's been `promote_to_generalizable`d resolves via
/// its own `Constraint::LookupLocal` instead, exactly mirroring `Lookup`
/// but keyed by `TypeVarId`. A `Ref::TopLevel` that happens to already be in
/// `scope` is that same self/mutual-reference case (`constrain::decl`) —
/// always `Monomorphic` while it's there, since its group isn't solved yet.
/// Everything else defers via `Lookup` (see `constrain/mod.rs`).
/// `Ref::Unresolved` is neither — `knot-canonical` already recorded the
/// real error for it, so this returns a bare, unconstrained fresh variable
/// rather than compounding that error with a confusing downstream one.
fn constrain_name_ref(
    sub: &mut Substitution,
    scope: &LocalScope,
    reference: &Ref,
    span: knot_syntax::span::Span,
    constraints: &mut Vec<Constraint>,
) -> TypeVarId {
    match reference {
        Ref::Local(name) => match scope.try_lookup(name) {
            // Canonicalization guarantees the name is bound somewhere; if
            // constraint generation hasn't bound it, an earlier error is
            // already being reported, so leave the reference unconstrained.
            None => sub.fresh_unbound(),
            Some(LocalBinding::Monomorphic(ty)) => ty,
            Some(LocalBinding::Generalizable(key)) => {
                let ty = sub.fresh_unbound();
                constraints.push(Constraint::LookupLocal {
                    span,
                    key,
                    expected: ty,
                });
                ty
            }
        },
        Ref::Unresolved(_) => sub.fresh_unbound(),
        Ref::TopLevel(name) if scope.try_lookup(name).is_some() => {
            scope.try_lookup(name).map(LocalBinding::header_ty).unwrap()
        }
        _ => {
            let ty = sub.fresh_unbound();
            constraints.push(Constraint::Lookup {
                span,
                reference: reference.clone(),
                expected: ty,
            });
            ty
        }
    }
}

pub fn constrain_expr(
    sub: &mut Substitution,
    scope: &mut LocalScope,
    expr: &Spanned<CExpr>,
    constraints: &mut Vec<Constraint>,
) -> TypedExpr {
    let span = expr.span;
    let id = sub.fresh_node();
    let wrap = |ty, node| Typed { id, span, ty, node };
    match &expr.node {
        // An integer literal is `Num a => a`: context decides its type,
        // and if nothing does, `solve.rs` defaults it (to `Int`).
        CExpr::IntLit(n) => {
            let ty = sub.fresh_unbound();
            constraints.push(Constraint::HasInstance {
                span,
                interface: "Num".to_string(),
                ty,
            });
            wrap(ty, TExpr::IntLit(n.clone()))
        }
        // A float literal is `Fractional a => a`, as in Haskell; defaulting
        // makes an undetermined one a `Float`.
        CExpr::FloatLit(f) => {
            let ty = sub.fresh_unbound();
            constraints.push(Constraint::HasInstance {
                span,
                interface: "Fractional".to_string(),
                ty,
            });
            wrap(ty, TExpr::FloatLit(f.clone()))
        }
        CExpr::StringLit(s) => wrap(app0(sub, "String"), TExpr::StringLit(s.clone())),
        CExpr::Unit => wrap(sub.fresh_bound(Structure::Unit), TExpr::Unit),
        // A hole's own type is unconstrained by design (it could be
        // anything) -- that a hole must always be a compile error (spec
        // §15.1) is a separate, later diagnostic concern (recording and
        // always rejecting every hole span seen), not a typing constraint,
        // so it isn't handled here.
        CExpr::Hole => {
            let ty = sub.fresh_unbound();
            constraints.push(Constraint::Hole { span, ty });
            wrap(ty, TExpr::Hole)
        }
        CExpr::Var(r) => {
            let ty = constrain_name_ref(sub, scope, r, span, constraints);
            wrap(ty, TExpr::Var(r.clone()))
        }
        CExpr::Ctor(r) => {
            let ty = constrain_name_ref(sub, scope, r, span, constraints);
            wrap(ty, TExpr::Ctor(r.clone()))
        }
        CExpr::Lambda(params, body) => {
            scope.push();
            let typed_params: Vec<crate::ast::TypedPattern> = params
                .iter()
                .map(|p| constrain_pattern(sub, scope, p, constraints))
                .collect();
            let typed_body = constrain_expr(sub, scope, body, constraints);
            scope.pop();
            let ty = typed_params.iter().rev().fold(typed_body.ty, |acc, p| {
                sub.fresh_bound(Structure::Fn(p.ty, acc))
            });
            wrap(ty, TExpr::Lambda(typed_params, Box::new(typed_body)))
        }
        CExpr::App(f, arg) => {
            let typed_f = constrain_expr(sub, scope, f, constraints);
            let typed_arg = constrain_expr(sub, scope, arg, constraints);
            let result_ty = sub.fresh_unbound();
            let expected_fn_ty = sub.fresh_bound(Structure::Fn(typed_arg.ty, result_ty));
            constraints.push(Constraint::Equal {
                span,
                expected: typed_f.ty,
                actual: expected_fn_ty,
            });
            wrap(
                result_ty,
                TExpr::App(Box::new(typed_f), Box::new(typed_arg)),
            )
        }
        CExpr::If(cond, then_branch, else_branch) => {
            let typed_cond = constrain_expr(sub, scope, cond, constraints);
            let bool_ty = app0(sub, "Bool");
            constraints.push(Constraint::Equal {
                span,
                expected: typed_cond.ty,
                actual: bool_ty,
            });
            let typed_then = constrain_expr(sub, scope, then_branch, constraints);
            let typed_else = constrain_expr(sub, scope, else_branch, constraints);
            constraints.push(Constraint::Equal {
                span,
                expected: typed_then.ty,
                actual: typed_else.ty,
            });
            let ty = typed_then.ty;
            wrap(
                ty,
                TExpr::If(
                    Box::new(typed_cond),
                    Box::new(typed_then),
                    Box::new(typed_else),
                ),
            )
        }
        CExpr::Case(scrutinee, arms) => {
            let typed_scrutinee = constrain_expr(sub, scope, scrutinee, constraints);
            let result_ty = sub.fresh_unbound();
            let mut typed_arms = Vec::new();
            for (pattern, body) in arms {
                scope.push();
                let typed_pattern = constrain_pattern(sub, scope, pattern, constraints);
                constraints.push(Constraint::Equal {
                    span,
                    expected: typed_scrutinee.ty,
                    actual: typed_pattern.ty,
                });
                let typed_body = constrain_expr(sub, scope, body, constraints);
                constraints.push(Constraint::Equal {
                    span,
                    expected: result_ty,
                    actual: typed_body.ty,
                });
                scope.pop();
                typed_arms.push((typed_pattern, typed_body));
            }
            wrap(
                result_ty,
                TExpr::Case(Box::new(typed_scrutinee), typed_arms),
            )
        }
        CExpr::List(elems) => {
            let elem_ty = sub.fresh_unbound();
            let mut typed_elems = Vec::new();
            for e in elems {
                let typed_e = constrain_expr(sub, scope, e, constraints);
                constraints.push(Constraint::Equal {
                    span,
                    expected: elem_ty,
                    actual: typed_e.ty,
                });
                typed_elems.push(typed_e);
            }
            wrap(app1(sub, "List", elem_ty), TExpr::List(typed_elems))
        }
        CExpr::Tuple(elems) => {
            let typed_elems: Vec<TypedExpr> = elems
                .iter()
                .map(|e| constrain_expr(sub, scope, e, constraints))
                .collect();
            let elem_tys = typed_elems.iter().map(|e| e.ty).collect();
            wrap(
                sub.fresh_bound(Structure::Tuple(elem_tys)),
                TExpr::Tuple(typed_elems),
            )
        }
        // A record literal is always closed -- exactly these fields, spec
        // §7.2 -- unlike `FieldAccess`/`RecordUpdate` below, which only ever
        // need an *open* row (the value being accessed/updated might have
        // more fields than the expression cares about).
        CExpr::Record(fields) => {
            let typed_fields: Vec<(String, TypedExpr)> = fields
                .iter()
                .map(|(name, e)| (name.clone(), constrain_expr(sub, scope, e, constraints)))
                .collect();
            let field_tys = typed_fields
                .iter()
                .map(|(n, e)| (n.clone(), e.ty))
                .collect();
            wrap(
                sub.fresh_bound(Structure::Record(field_tys, None)),
                TExpr::Record(typed_fields),
            )
        }
        CExpr::RecordUpdate(base, updates) => {
            let typed_base = constrain_expr(sub, scope, base, constraints);
            let typed_updates: Vec<(String, TypedExpr)> = updates
                .iter()
                .map(|(name, e)| (name.clone(), constrain_expr(sub, scope, e, constraints)))
                .collect();
            // `base` only needs to have *at least* the updated fields, at
            // their new types -- the row-polymorphism machinery in
            // `unify::unify_record` figures out the rest, whatever it is.
            let update_tys = typed_updates
                .iter()
                .map(|(n, e)| (n.clone(), e.ty))
                .collect();
            let rest = sub.fresh_unbound();
            let required = sub.fresh_bound(Structure::Record(update_tys, Some(rest)));
            constraints.push(Constraint::Equal {
                span,
                expected: typed_base.ty,
                actual: required,
            });
            // The result has exactly `base`'s own shape -- an update can't
            // add or remove fields, only change values.
            let ty = typed_base.ty;
            wrap(ty, TExpr::RecordUpdate(Box::new(typed_base), typed_updates))
        }
        CExpr::FieldAccess(base, field) => {
            let typed_base = constrain_expr(sub, scope, base, constraints);
            let field_ty = sub.fresh_unbound();
            let rest = sub.fresh_unbound();
            let mut required_fields = std::collections::BTreeMap::new();
            required_fields.insert(field.clone(), field_ty);
            let required = sub.fresh_bound(Structure::Record(required_fields, Some(rest)));
            constraints.push(Constraint::Equal {
                span,
                expected: typed_base.ty,
                actual: required,
            });
            wrap(
                field_ty,
                TExpr::FieldAccess(Box::new(typed_base), field.clone()),
            )
        }
        // Annotation *values* aren't constrained here at all -- see this
        // module's doc comment. Just look straight through to the target,
        // keeping the original CAnnotations unelaborated.
        CExpr::Annotated(annotations, target) => {
            let typed_target = constrain_expr(sub, scope, target, constraints);
            let ty = typed_target.ty;
            wrap(
                ty,
                TExpr::Annotated(annotations.clone(), Box::new(typed_target)),
            )
        }
        CExpr::BinOp(op, l, r) => {
            let typed_l = constrain_expr(sub, scope, l, constraints);
            let typed_r = constrain_expr(sub, scope, r, constraints);
            let op_ty = constrain_name_ref(sub, scope, op, span, constraints);
            let ty = apply(sub, op_ty, &[typed_l.ty, typed_r.ty], span, constraints);
            wrap(
                ty,
                TExpr::BinOp(op.clone(), Box::new(typed_l), Box::new(typed_r), op_ty),
            )
        }
        CExpr::Negate(e) => {
            let typed_e = constrain_expr(sub, scope, e, constraints);
            let op_ty = lookup_builtin(sub, "negate", span, constraints);
            let ty = apply(sub, op_ty, &[typed_e.ty], span, constraints);
            wrap(ty, TExpr::Negate(Box::new(typed_e), op_ty))
        }
        CExpr::Let(bindings, body) => {
            let (mut typed_let, let_con) = crate::constrain::decl::constrain_let_bindings(
                sub,
                scope,
                bindings,
                |sub, scope, cs| constrain_expr(sub, scope, body, cs),
            );
            constraints.push(let_con);
            typed_let.id = id;
            typed_let.span = span;
            typed_let
        }
        CExpr::Do(stmts, final_expr) => {
            constrain_do(sub, scope, stmts, final_expr, span, constraints)
        }
    }
}

/// Check sequencing using the type of `(>>=)`, retaining the original block.
/// The recursive constraint construction gives each statement the same scope
/// and generalization boundary that lowering to binds/lets will have.
fn constrain_do(
    sub: &mut Substitution,
    scope: &mut LocalScope,
    stmts: &[CDoStmt],
    final_expr: &Spanned<CExpr>,
    span: Span,
    constraints: &mut Vec<Constraint>,
) -> TypedExpr {
    let Some((stmt, rest)) = stmts.split_first() else {
        let body = constrain_expr(sub, scope, final_expr, constraints);
        return Typed {
            id: sub.fresh_node(),
            span,
            ty: body.ty,
            node: TExpr::Do(Vec::new(), Box::new(body)),
        };
    };
    let (first, tail, ty) = match stmt {
        CDoStmt::Let(bindings) => {
            let (typed, con) = crate::constrain::decl::constrain_let_bindings(
                sub,
                scope,
                bindings,
                |sub, scope, cs| constrain_do(sub, scope, rest, final_expr, span, cs),
            );
            constraints.push(con);
            let TExpr::Let(bindings, tail) = typed.node else {
                unreachable!()
            };
            (TDoStmt::Let(bindings), *tail, typed.ty)
        }
        CDoStmt::Bind(_, action) | CDoStmt::Expr(action) => {
            // An action cannot see its own bind's names.
            let value = constrain_expr(sub, scope, action, constraints);
            let bind_ty = lookup_builtin(sub, ">>=", action.span, constraints);
            scope.push();
            let pattern = match stmt {
                CDoStmt::Bind(p, _) => p.clone(),
                _ => Spanned::new(action.span, CPattern::Wildcard(None)),
            };
            let pattern = constrain_pattern(sub, scope, &pattern, constraints);
            let tail = constrain_do(sub, scope, rest, final_expr, span, constraints);
            scope.pop();
            let continuation = sub.fresh_bound(Structure::Fn(pattern.ty, tail.ty));
            let ty = apply(
                sub,
                bind_ty,
                &[value.ty, continuation],
                action.span,
                constraints,
            );
            let first = match stmt {
                CDoStmt::Bind(_, _) => TDoStmt::Bind(pattern, value, bind_ty),
                _ => TDoStmt::Expr(value, bind_ty),
            };
            (first, tail, ty)
        }
    };
    let TExpr::Do(mut rest, body) = tail.node else {
        unreachable!()
    };
    rest.insert(0, first);
    Typed {
        id: sub.fresh_node(),
        span,
        ty,
        node: TExpr::Do(rest, body),
    }
}

#[cfg(test)]
mod tests {
    use crate::check::check_source;

    fn type_of(src: &str) -> String {
        let report = check_source(src);
        assert!(report.errors.is_empty(), "{src}: {:?}", report.errors);
        report
            .types
            .into_iter()
            .find(|(n, _)| n == "it")
            .expect("binding `it`")
            .1
    }

    fn rejected(src: &str) -> Vec<String> {
        let errors: Vec<String> = check_source(src)
            .errors
            .into_iter()
            .map(|e| format!("{:?}", e.kind))
            .collect();
        assert!(!errors.is_empty(), "{src} should be rejected");
        errors
    }

    #[test]
    fn literals() {
        assert_eq!(type_of("it = 1\n"), "Int");
        assert_eq!(type_of("it = 1.5\n"), "Float");
        assert_eq!(type_of("it = \"s\"\n"), "String");
        assert_eq!(type_of("it = ()\n"), "()");
        assert_eq!(type_of("it :: Float\nit = 1\n"), "Float");
    }

    #[test]
    fn holes_are_errors_reporting_the_expected_type() {
        let errors = rejected("it :: Int -> Int\nit x = x + _\n");
        assert!(
            errors.iter().any(|e| e == "Hole { expected: \"Int\" }"),
            "{errors:?}"
        );
    }

    #[test]
    fn lambda_application_and_if() {
        assert_eq!(type_of("it = \\x y -> (y, x)\n"), "a -> b -> (b, a)");
        assert_eq!(type_of("it = (\\x -> x) True\n"), "Bool");
        assert_eq!(type_of("it c = if c then 1 else 2\n"), "Num a => Bool -> a");
        rejected("it = if 1 then 2 else 3\n");
        rejected("it c = if c then 1 else \"s\"\n");
    }

    #[test]
    fn case_unifies_scrutinee_with_patterns_and_arms_with_each_other() {
        assert_eq!(
            type_of("it m = case m of\n  Just x -> x\n  Nothing -> 0\n"),
            "Num a => Maybe a -> a"
        );
        rejected("it m = case m of\n  Just x -> x\n  Nothing -> \"s\"\n  _ -> True\n");
    }

    #[test]
    fn lists_and_tuples() {
        assert_eq!(type_of("it = [1, 2]\n"), "List Int");
        assert_eq!(type_of("it = []\n"), "List a");
        assert_eq!(type_of("it = (1, \"s\", True)\n"), "(Int, String, Bool)");
        rejected("it = [1, \"s\"]\n");
    }

    #[test]
    fn records() {
        assert_eq!(
            type_of("it = { x = 1, y = True }\n"),
            "{ x : Int, y : Bool }"
        );
        assert_eq!(type_of("it r = r.x\n"), "{ b | x : a } -> a");
        assert_eq!(
            type_of("it r = { r | x = 1 }\n"),
            "Num a => { b | x : a } -> { b | x : a }"
        );
        assert_eq!(type_of("it = { x = 1, y = 2 }.y\n"), "Int");
        rejected("it = { x = 1 }.y\n");
    }

    #[test]
    fn annotations_look_through_to_their_target() {
        assert_eq!(type_of("it = @label(\"n\") 5\n"), "Int");
    }

    #[test]
    fn arithmetic_operators_need_their_interfaces() {
        assert_eq!(type_of("it x = x + 1\n"), "Num a => a -> a");
        assert_eq!(type_of("it x y = x / y\n"), "Fractional a => a -> a -> a");
        assert_eq!(type_of("it x y = div x y\n"), "Integral a => a -> a -> a");
        assert_eq!(type_of("it x y = mod x y\n"), "Integral a => a -> a -> a");
        assert_eq!(
            type_of("it x n = x ^ n\n"),
            "(Num a, Integral b) => a -> b -> a"
        );
        assert_eq!(type_of("it x = -x\n"), "Num a => a -> a");
        rejected("it = 1 + \"s\"\n");
        rejected("it = div 1.5 2.0\n");
    }

    #[test]
    fn comparison_and_logic_operators() {
        assert_eq!(type_of("it x y = x == y\n"), "Eq a => a -> a -> Bool");
        assert_eq!(type_of("it x y = x /= y\n"), "Eq a => a -> a -> Bool");
        assert_eq!(type_of("it x y = x <= y\n"), "Ord a => a -> a -> Bool");
        assert_eq!(type_of("it x y = x && y || x\n"), "Bool -> Bool -> Bool");
        rejected("it = 1 && True\n");
    }

    #[test]
    fn list_and_semigroup_operators() {
        assert_eq!(type_of("it x xs = x : xs\n"), "a -> List a -> List a");
        assert_eq!(type_of("it x y = x <> y\n"), "Semigroup a => a -> a -> a");
        rejected("it = 1 : [\"s\"]\n");
    }

    #[test]
    fn pipe_and_composition() {
        assert_eq!(type_of("it = 1 |> (\\x -> x + 1)\n"), "Int");
        assert_eq!(
            type_of("it f g = f |>> g\n"),
            "(a -> b) -> (b -> c) -> a -> c"
        );
    }

    #[test]
    fn then_sequences_two_actions() {
        // `>>` is the monadic then; composition is `|>>`.
        assert_eq!(type_of("it m k = m >> k\n"), "Monad a => a b -> a c -> a c");
    }

    #[test]
    fn monadic_operators_need_their_constructor_interfaces() {
        assert_eq!(
            type_of("it m f = m >>= f\n"),
            "Monad a => a b -> (b -> a c) -> a c"
        );
        assert_eq!(
            type_of("it f x = f <*> x\n"),
            "Applicative a => a (b -> c) -> a b -> a c"
        );
        rejected("it = 5 >>= (\\x -> 5)\n");
    }

    #[test]
    fn operator_references_have_the_operators_type() {
        assert_eq!(type_of("it = (+)\n"), "Num a => a -> a -> a");
        assert_eq!(type_of("it = (+) 1 2\n"), "Int");
    }

    #[test]
    fn let_bindings_are_polymorphic_and_see_each_other() {
        assert_eq!(
            type_of("it = let id = \\x -> x in (id 1, id True)\n"),
            "(Int, Bool)"
        );
        assert_eq!(
            type_of("it =\n  let\n    a = 1\n    b = a + 1\n  in\n    b\n"),
            "Int"
        );
    }

    #[test]
    fn do_notation_desugars_to_bind() {
        assert_eq!(
            type_of("it = do\n  x <- Just 1\n  y <- Just 2\n  pure (x + y)\n"),
            "Maybe Int"
        );
        assert_eq!(type_of("it = do\n  Just 1\n  Just True\n"), "Maybe Bool");
    }
}
