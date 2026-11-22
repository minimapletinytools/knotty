//! Structural unification over `Structure` — `App`/`Fn`/`Tuple`/`Unit`/
//! `Record` (row-polymorphic, by field gathering), constructor variables
//! (`VarApp`/`Ctor`), the occurs check, rigid-variable handling, and level
//! maintenance (see `var.rs`). Interface obligations are not this module's
//! concern; it only answers "can these two types be made equal".

use std::collections::BTreeMap;

use crate::error::UnifyError;
use crate::ty::Structure;
use crate::var::{Substitution, TypeVarId};

pub fn unify(sub: &mut Substitution, a: TypeVarId, b: TypeVarId) -> Result<(), UnifyError> {
    let (a, b) = (sub.find(a), sub.find(b));
    if a == b {
        return Ok(());
    }
    if sub.is_rigid(a) || sub.is_rigid(b) {
        return unify_rigid(sub, a, b);
    }
    match (sub.resolve_structure(a), sub.resolve_structure(b)) {
        (None, None) => {
            sub.union_unbound(a, b);
            Ok(())
        }
        (None, Some(sb)) => bind(sub, a, sb),
        (Some(sa), None) => bind(sub, b, sa),
        (Some(sa), Some(sb)) => unify_structure(sub, a, b, sa, sb),
    }
}

/// At least one side is rigid and `a != b`. A rigid variable only ever
/// unifies with an unbound flexible variable, and only one that lives at
/// least as deep as the rigid's own signature — linking an outer variable
/// to it would let the signature's type variable escape its scope.
fn unify_rigid(sub: &mut Substitution, a: TypeVarId, b: TypeVarId) -> Result<(), UnifyError> {
    let (rigid, other) = if sub.is_rigid(a) { (a, b) } else { (b, a) };
    if sub.is_rigid(other) || sub.resolve_structure(other).is_some() {
        return Err(UnifyError::Mismatch {
            expected: a,
            actual: b,
        });
    }
    let rigid_level = sub.var_level(rigid).unwrap_or(0);
    let other_level = sub.var_level(other).unwrap_or(0);
    if other_level < rigid_level {
        return Err(UnifyError::RigidEscape { rigid, other });
    }
    sub.link_to(other, rigid);
    Ok(())
}

/// Binds unbound `var` to `structure`, after one walk over the structure
/// that does three jobs: the occurs check, lowering every unbound variable
/// inside to at most `var`'s level, and rejecting a rigid variable from a
/// deeper scope than `var` (it would escape its signature).
fn bind(sub: &mut Substitution, var: TypeVarId, structure: Structure) -> Result<(), UnifyError> {
    let level = sub.var_level(var).unwrap_or(0);
    for child in structure_children(&structure) {
        adjust(sub, var, level, child)?;
    }
    sub.bind(var, structure);
    Ok(())
}

fn adjust(
    sub: &mut Substitution,
    var: TypeVarId,
    level: u32,
    ty: TypeVarId,
) -> Result<(), UnifyError> {
    let root = sub.find(ty);
    if root == var {
        return Err(UnifyError::Occurs { var, in_ty: ty });
    }
    if sub.is_unbound(root) {
        sub.lower_level(root, level);
        return Ok(());
    }
    if sub.is_rigid(root) {
        if sub.var_level(root).unwrap_or(0) > level {
            return Err(UnifyError::RigidEscape {
                rigid: root,
                other: var,
            });
        }
        return Ok(());
    }
    if let Some(structure) = sub.resolve_structure(root) {
        for child in structure_children(&structure) {
            adjust(sub, var, level, child)?;
        }
    }
    Ok(())
}

/// Every type variable a structure directly refers to.
pub(crate) fn structure_children(structure: &Structure) -> Vec<TypeVarId> {
    match structure {
        Structure::App(_, args)
        | Structure::Ctor(_, args)
        | Structure::Tuple(args)
        | Structure::Predicate(args) => args.clone(),
        Structure::Fn(a, b) => vec![*a, *b],
        Structure::Record(fields, ext) => {
            let mut children: Vec<TypeVarId> = fields.values().copied().collect();
            children.extend(*ext);
            children
        }
        Structure::Unit => Vec::new(),
        Structure::VarApp(f, args) => {
            let mut children = vec![*f];
            children.extend(args.iter().copied());
            children
        }
    }
}

fn unify_all(
    sub: &mut Substitution,
    xs: Vec<TypeVarId>,
    ys: Vec<TypeVarId>,
) -> Result<(), UnifyError> {
    for (x, y) in xs.into_iter().zip(ys) {
        unify(sub, x, y)?;
    }
    Ok(())
}

fn unify_structure(
    sub: &mut Substitution,
    a: TypeVarId,
    b: TypeVarId,
    sa: Structure,
    sb: Structure,
) -> Result<(), UnifyError> {
    let mismatch = UnifyError::Mismatch {
        expected: a,
        actual: b,
    };
    match (sa, sb) {
        (Structure::Unit, Structure::Unit) => Ok(()),
        (Structure::App(ra, xs), Structure::App(rb, ys))
        | (Structure::Ctor(ra, xs), Structure::Ctor(rb, ys))
        // Named higher-kind arguments in signatures are represented by App;
        // inferred constructor-variable heads use Ctor. Kind checking has
        // already established the application position's kind.
        | (Structure::App(ra, xs), Structure::Ctor(rb, ys))
        | (Structure::Ctor(ra, xs), Structure::App(rb, ys))
            if ra == rb && xs.len() == ys.len() =>
        {
            unify_all(sub, xs, ys)
        }
        (Structure::Fn(a1, r1), Structure::Fn(a2, r2)) => {
            unify(sub, a1, a2)?;
            unify(sub, r1, r2)
        }
        (Structure::Tuple(xs), Structure::Tuple(ys)) | (Structure::Predicate(xs), Structure::Predicate(ys)) if xs.len() == ys.len() => {
            unify_all(sub, xs, ys)
        }
        (Structure::Record(fields1, ext1), Structure::Record(fields2, ext2)) => {
            unify_record(sub, a, b, fields1, ext1, fields2, ext2)
        }
        (Structure::VarApp(fa, xs), Structure::VarApp(fb, ys)) if xs.len() == ys.len() => {
            unify(sub, fa, fb)?;
            unify_all(sub, xs, ys)
        }
        // A constructor variable meeting a concrete application: the
        // variable stands for the head partially applied to the leading
        // arguments, and the trailing ones line up with the `VarApp`'s own.
        (Structure::VarApp(f, xs), Structure::App(r, ys))
        | (Structure::App(r, ys), Structure::VarApp(f, xs))
            if ys.len() >= xs.len() =>
        {
            let split = ys.len() - xs.len();
            let leading = ys[..split].to_vec();
            let trailing = ys[split..].to_vec();
            let ctor = sub.fresh_bound(Structure::Ctor(r, leading));
            unify(sub, f, ctor)?;
            unify_all(sub, xs, trailing)
        }
        _ => Err(mismatch),
    }
}

/// Row-polymorphic record unification: unify the fields both sides have,
/// then make each side's extension variable absorb the fields only the
/// other side has. Recursing through `unify` (not `Substitution::bind`)
/// lets an extension that is already bound to a record merge naturally.
fn unify_record(
    sub: &mut Substitution,
    a: TypeVarId,
    b: TypeVarId,
    fields1: BTreeMap<String, TypeVarId>,
    ext1: Option<TypeVarId>,
    fields2: BTreeMap<String, TypeVarId>,
    ext2: Option<TypeVarId>,
) -> Result<(), UnifyError> {
    let mut only1 = BTreeMap::new();
    let mut only2 = fields2;
    for (name, ty1) in fields1 {
        match only2.remove(&name) {
            Some(ty2) => unify(sub, ty1, ty2)?,
            None => {
                only1.insert(name, ty1);
            }
        }
    }
    let mismatch = UnifyError::Mismatch {
        expected: a,
        actual: b,
    };

    match (only1.is_empty(), only2.is_empty()) {
        (true, true) => unify_extensions(sub, ext1, ext2),
        (true, false) => absorb(sub, ext1, only2, ext2, mismatch),
        (false, true) => absorb(sub, ext2, only1, ext1, mismatch),
        (false, false) => {
            let (Some(e1), Some(e2)) = (ext1, ext2) else {
                return Err(mismatch);
            };
            // Same unknown remainder on both sides, but different explicit
            // fields: `{ r | x }` vs `{ r | y }` would need `r` to contain
            // `y` and `x` to be absent from it at once.
            if sub.find(e1) == sub.find(e2) {
                return Err(mismatch);
            }
            // The shared remainder starts at the deepest level; binding the
            // two extensions below lowers it to wherever it really lives.
            let rest = sub.fresh_unbound_at(u32::MAX);
            let needs1 = sub.fresh_bound(Structure::Record(only2, Some(rest)));
            let needs2 = sub.fresh_bound(Structure::Record(only1, Some(rest)));
            unify(sub, e1, needs1)?;
            unify(sub, e2, needs2)
        }
    }
}

/// One side (`absent_ext`'s owner) lacks `extra_fields`: its extension must
/// be exactly those fields plus whatever remains of the other side.
fn absorb(
    sub: &mut Substitution,
    absent_ext: Option<TypeVarId>,
    extra_fields: BTreeMap<String, TypeVarId>,
    donor_ext: Option<TypeVarId>,
    mismatch: UnifyError,
) -> Result<(), UnifyError> {
    match absent_ext {
        None => Err(mismatch),
        Some(e) => {
            let needed = sub.fresh_bound(Structure::Record(extra_fields, donor_ext));
            unify(sub, e, needed)
        }
    }
}

/// Both sides have exactly the same fields; only the remainders are left.
fn unify_extensions(
    sub: &mut Substitution,
    ext1: Option<TypeVarId>,
    ext2: Option<TypeVarId>,
) -> Result<(), UnifyError> {
    match (ext1, ext2) {
        (None, None) => Ok(()),
        (Some(e1), Some(e2)) => unify(sub, e1, e2),
        (Some(e), None) | (None, Some(e)) => {
            let empty = sub.fresh_bound(Structure::Record(BTreeMap::new(), None));
            unify(sub, e, empty)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot_canonical::ast::Ref;

    fn app0(sub: &mut Substitution, name: &str) -> TypeVarId {
        sub.fresh_bound(Structure::App(Ref::Builtin(name.to_string()), vec![]))
    }

    fn app1(sub: &mut Substitution, name: &str, arg: TypeVarId) -> TypeVarId {
        sub.fresh_bound(Structure::App(Ref::Builtin(name.to_string()), vec![arg]))
    }

    #[test]
    fn identical_builtin_apps_unify() {
        let mut sub = Substitution::new();
        let a = app0(&mut sub, "Int");
        let b = app0(&mut sub, "Int");
        assert!(unify(&mut sub, a, b).is_ok());
    }

    #[test]
    fn mismatched_builtin_apps_fail() {
        let mut sub = Substitution::new();
        let a = app0(&mut sub, "Int");
        let b = app0(&mut sub, "Float");
        assert!(matches!(
            unify(&mut sub, a, b),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn generic_apps_unify_by_unifying_their_arguments() {
        // List a ~ List Int -- a should resolve to Int.
        let mut sub = Substitution::new();
        let a = sub.fresh_unbound();
        let list_a = app1(&mut sub, "List", a);
        let int_ty = app0(&mut sub, "Int");
        let list_int = app1(&mut sub, "List", int_ty);
        assert!(unify(&mut sub, list_a, list_int).is_ok());
        assert_eq!(sub.resolve_structure(a), sub.resolve_structure(int_ty));
    }

    #[test]
    fn different_heads_fail_even_with_unifiable_args() {
        let mut sub = Substitution::new();
        let a = sub.fresh_unbound();
        let list_a = app1(&mut sub, "List", a);
        let b = sub.fresh_unbound();
        let maybe_b = app1(&mut sub, "Maybe", b);
        assert!(matches!(
            unify(&mut sub, list_a, maybe_b),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn sensitivity_unifies_by_unifying_its_argument_like_any_other_generic() {
        // Per the type-checker plan's "stub Sensitivity" note: Sensitivity a
        // ~ Sensitivity Int unifies (a := Int), with no special-casing beyond
        // the ordinary App rule -- no introspection into `a`'s own shape.
        let mut sub = Substitution::new();
        let a = sub.fresh_unbound();
        let sens_a = app1(&mut sub, "Sensitivity", a);
        let int_ty = app0(&mut sub, "Int");
        let sens_int = app1(&mut sub, "Sensitivity", int_ty);
        assert!(unify(&mut sub, sens_a, sens_int).is_ok());
        assert_eq!(sub.resolve_structure(a), sub.resolve_structure(int_ty));
    }

    #[test]
    fn sensitivity_of_different_concrete_types_do_not_unify() {
        let mut sub = Substitution::new();
        let int_ty = app0(&mut sub, "Int");
        let float_ty = app0(&mut sub, "Float");
        let sens_int = app1(&mut sub, "Sensitivity", int_ty);
        let sens_float = app1(&mut sub, "Sensitivity", float_ty);
        assert!(matches!(
            unify(&mut sub, sens_int, sens_float),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn function_types_unify_argument_and_return_independently() {
        let mut sub = Substitution::new();
        let a1 = sub.fresh_unbound();
        let r1 = sub.fresh_unbound();
        let fn1 = sub.fresh_bound(Structure::Fn(a1, r1));

        let int_ty = app0(&mut sub, "Int");
        let bool_ty = app0(&mut sub, "Bool");
        let fn2 = sub.fresh_bound(Structure::Fn(int_ty, bool_ty));

        assert!(unify(&mut sub, fn1, fn2).is_ok());
        assert_eq!(sub.resolve_structure(a1), sub.resolve_structure(int_ty));
        assert_eq!(sub.resolve_structure(r1), sub.resolve_structure(bool_ty));
    }

    #[test]
    fn mismatched_function_argument_fails() {
        let mut sub = Substitution::new();
        let int_ty = app0(&mut sub, "Int");
        let bool_ty = app0(&mut sub, "Bool");
        let string_ty = app0(&mut sub, "String");
        let fn1 = sub.fresh_bound(Structure::Fn(int_ty, bool_ty));
        let fn2 = sub.fresh_bound(Structure::Fn(string_ty, bool_ty));
        assert!(matches!(
            unify(&mut sub, fn1, fn2),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn tuples_unify_elementwise() {
        let mut sub = Substitution::new();
        let a = sub.fresh_unbound();
        let b = sub.fresh_unbound();
        let t1 = sub.fresh_bound(Structure::Tuple(vec![a, b]));

        let int_ty = app0(&mut sub, "Int");
        let bool_ty = app0(&mut sub, "Bool");
        let t2 = sub.fresh_bound(Structure::Tuple(vec![int_ty, bool_ty]));

        assert!(unify(&mut sub, t1, t2).is_ok());
        assert_eq!(sub.resolve_structure(a), sub.resolve_structure(int_ty));
        assert_eq!(sub.resolve_structure(b), sub.resolve_structure(bool_ty));
    }

    #[test]
    fn mismatched_tuple_arity_fails() {
        let mut sub = Substitution::new();
        let int_ty = app0(&mut sub, "Int");
        let bool_ty = app0(&mut sub, "Bool");
        let pair = sub.fresh_bound(Structure::Tuple(vec![int_ty, bool_ty]));
        let triple = sub.fresh_bound(Structure::Tuple(vec![int_ty, bool_ty, int_ty]));
        assert!(matches!(
            unify(&mut sub, pair, triple),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn ctor_var_pins_to_a_concrete_head_it_meets_and_unifies_arguments() {
        // f a ~ List Int should bind f := List and a := Int, mirroring an
        // ordinary generic App's own argument-unification rule, just with
        // a variable in the head position (spec §10.5, e.g.
        // `map`'s `f`).
        let mut sub = Substitution::new();
        let f = sub.fresh_unbound();
        let a = sub.fresh_unbound();
        let fa = sub.fresh_bound(Structure::VarApp(f, vec![a]));
        let int_ty = app0(&mut sub, "Int");
        let list_int = app1(&mut sub, "List", int_ty);
        assert!(unify(&mut sub, fa, list_int).is_ok());
        assert_eq!(
            sub.resolve_structure(f),
            Some(Structure::Ctor(Ref::Builtin("List".to_string()), vec![]))
        );
        assert_eq!(sub.resolve_structure(a), Some(builtin_app(&mut sub, "Int")));
    }

    #[test]
    fn ctor_var_pins_to_a_2_parameter_head_leaving_the_last_argument_free() {
        // f a ~ Result String Int (a 2-parameter constructor, unlike the
        // 1-parameter `List` above) should bind f := Result *partially
        // applied to String* and a := Int -- the exact-arity-match bug
        // this used to hit unconditionally rejected any concrete head
        // whose own argument count didn't match the VarApp's single
        // argument, which every 2-parameter constructor instance
        // (`Result e a`, `Map k v`) always failed.
        let mut sub = Substitution::new();
        let f = sub.fresh_unbound();
        let a = sub.fresh_unbound();
        let fa = sub.fresh_bound(Structure::VarApp(f, vec![a]));
        let string_ty = app0(&mut sub, "String");
        let int_ty = app0(&mut sub, "Int");
        let result_string_int = sub.fresh_bound(Structure::App(
            Ref::Builtin("Result".to_string()),
            vec![string_ty, int_ty],
        ));
        assert!(unify(&mut sub, fa, result_string_int).is_ok());
        match sub.resolve_structure(f) {
            Some(Structure::Ctor(r, leading)) => {
                assert_eq!(r, Ref::Builtin("Result".to_string()));
                assert_eq!(leading.len(), 1);
                assert_eq!(
                    sub.resolve_structure(leading[0]),
                    Some(builtin_app(&mut sub, "String"))
                );
            }
            other => panic!("expected a partially-applied Result Ctor, got {other:?}"),
        }
        assert_eq!(sub.resolve_structure(a), Some(builtin_app(&mut sub, "Int")));
    }

    fn builtin_app(sub: &mut Substitution, name: &str) -> Structure {
        let id = app0(sub, name);
        sub.resolve_structure(id).unwrap()
    }

    #[test]
    fn two_ctor_vars_unify_their_heads_and_arguments() {
        let mut sub = Substitution::new();
        let f = sub.fresh_unbound();
        let g = sub.fresh_unbound();
        let a = sub.fresh_unbound();
        let b = sub.fresh_unbound();
        let fa = sub.fresh_bound(Structure::VarApp(f, vec![a]));
        let gb = sub.fresh_bound(Structure::VarApp(g, vec![b]));
        assert!(unify(&mut sub, fa, gb).is_ok());
        assert_eq!(sub.find(f), sub.find(g));
        assert_eq!(sub.find(a), sub.find(b));
    }

    #[test]
    fn ctor_var_already_pinned_to_one_head_rejects_a_different_one() {
        let mut sub = Substitution::new();
        let f = sub.fresh_unbound();
        let a = sub.fresh_unbound();
        let fa = sub.fresh_bound(Structure::VarApp(f, vec![a]));
        let int_ty = app0(&mut sub, "Int");
        let list_int = app1(&mut sub, "List", int_ty);
        unify(&mut sub, fa, list_int).unwrap();

        let b = sub.fresh_unbound();
        let fb = sub.fresh_bound(Structure::VarApp(f, vec![b]));
        let bool_ty = app0(&mut sub, "Bool");
        let maybe_bool = app1(&mut sub, "Maybe", bool_ty);
        assert!(matches!(
            unify(&mut sub, fb, maybe_bool),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn mismatched_ctor_app_arity_fails() {
        let mut sub = Substitution::new();
        let f = sub.fresh_unbound();
        let a = sub.fresh_unbound();
        let b = sub.fresh_unbound();
        let fab = sub.fresh_bound(Structure::VarApp(f, vec![a, b]));
        let int_ty = app0(&mut sub, "Int");
        let list_int = app1(&mut sub, "List", int_ty);
        assert!(matches!(
            unify(&mut sub, fab, list_int),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn unit_unifies_with_unit() {
        let mut sub = Substitution::new();
        let u1 = sub.fresh_bound(Structure::Unit);
        let u2 = sub.fresh_bound(Structure::Unit);
        assert!(unify(&mut sub, u1, u2).is_ok());
    }

    #[test]
    fn occurs_check_rejects_an_infinite_type() {
        // a ~ List a must fail: binding a to List a would build a cycle.
        let mut sub = Substitution::new();
        let a = sub.fresh_unbound();
        let list_a = app1(&mut sub, "List", a);
        assert!(matches!(
            unify(&mut sub, a, list_a),
            Err(UnifyError::Occurs { .. })
        ));
    }

    #[test]
    fn occurs_check_catches_indirect_cycles_through_a_function_type() {
        // a ~ (a -> Int) must fail too, not just the single-level case.
        let mut sub = Substitution::new();
        let a = sub.fresh_unbound();
        let int_ty = app0(&mut sub, "Int");
        let fn_ty = sub.fresh_bound(Structure::Fn(a, int_ty));
        assert!(matches!(
            unify(&mut sub, a, fn_ty),
            Err(UnifyError::Occurs { .. })
        ));
    }

    #[test]
    fn rigid_var_unifies_with_a_fresh_flexible_var() {
        // Models ordinary body-checking against a signature: `myMax`'s rigid
        // `a` unifying with a parameter's freshly-inferred flexible type must
        // succeed, not be treated as a mismatch.
        let mut sub = Substitution::new();
        let rigid_a = sub.fresh_rigid("a".to_string());
        let param = sub.fresh_unbound();
        assert!(unify(&mut sub, rigid_a, param).is_ok());
        assert_eq!(sub.find(rigid_a), sub.find(param));
    }

    #[test]
    fn distinct_rigid_vars_never_unify() {
        let mut sub = Substitution::new();
        let a = sub.fresh_rigid("a".to_string());
        let b = sub.fresh_rigid("b".to_string());
        assert!(matches!(
            unify(&mut sub, a, b),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn rigid_var_never_unifies_with_a_concrete_type() {
        let mut sub = Substitution::new();
        let a = sub.fresh_rigid("a".to_string());
        let int_ty = app0(&mut sub, "Int");
        assert!(matches!(
            unify(&mut sub, a, int_ty),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn a_flexible_var_already_linked_to_rigid_still_rejects_a_concrete_type() {
        // Guards against a signature being a lie the checker doesn't actually
        // enforce (plan §5): once `param` has been unified with rigid `a`,
        // it must behave exactly as rigid from then on.
        let mut sub = Substitution::new();
        let rigid_a = sub.fresh_rigid("a".to_string());
        let param = sub.fresh_unbound();
        unify(&mut sub, rigid_a, param).unwrap();

        let int_ty = app0(&mut sub, "Int");
        assert!(matches!(
            unify(&mut sub, param, int_ty),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    // -- records --

    fn record(
        sub: &mut Substitution,
        fields: &[(&str, TypeVarId)],
        ext: Option<TypeVarId>,
    ) -> TypeVarId {
        let fields = fields.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        sub.fresh_bound(Structure::Record(fields, ext))
    }

    #[test]
    fn closed_records_with_identical_fields_unify_fieldwise() {
        let mut sub = Substitution::new();
        let a = sub.fresh_unbound();
        let bool_ty = app0(&mut sub, "Bool");
        let r1 = record(&mut sub, &[("x", a)], None);
        let r2 = record(&mut sub, &[("x", bool_ty)], None);
        assert!(unify(&mut sub, r1, r2).is_ok());
        assert_eq!(sub.resolve_structure(a), sub.resolve_structure(bool_ty));
    }

    #[test]
    fn closed_records_with_a_shared_field_type_mismatch_fail() {
        let mut sub = Substitution::new();
        let int_ty = app0(&mut sub, "Int");
        let bool_ty = app0(&mut sub, "Bool");
        let r1 = record(&mut sub, &[("x", int_ty)], None);
        let r2 = record(&mut sub, &[("x", bool_ty)], None);
        assert!(matches!(
            unify(&mut sub, r1, r2),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn closed_records_with_different_field_sets_fail() {
        let mut sub = Substitution::new();
        let int_ty = app0(&mut sub, "Int");
        let bool_ty = app0(&mut sub, "Bool");
        let r1 = record(&mut sub, &[("x", int_ty)], None);
        let r2 = record(&mut sub, &[("y", bool_ty)], None);
        assert!(matches!(
            unify(&mut sub, r1, r2),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn open_record_absorbs_a_closed_records_extra_field() {
        // { x : Int, y : Bool } (closed) ~ { x : Int | e } (open) should
        // succeed, with `e` resolving to the leftover `{ y : Bool }`.
        let mut sub = Substitution::new();
        let int_ty1 = app0(&mut sub, "Int");
        let int_ty2 = app0(&mut sub, "Int");
        let bool_ty = app0(&mut sub, "Bool");
        let closed = record(&mut sub, &[("x", int_ty1), ("y", bool_ty)], None);
        let e = sub.fresh_unbound();
        let open = record(&mut sub, &[("x", int_ty2)], Some(e));

        assert!(unify(&mut sub, closed, open).is_ok());
        match sub.resolve_structure(e) {
            Some(Structure::Record(fields, ext)) => {
                assert_eq!(ext, None);
                assert_eq!(fields.len(), 1);
                assert_eq!(
                    sub.resolve_structure(fields["y"]),
                    sub.resolve_structure(bool_ty)
                );
            }
            other => panic!("expected e to resolve to a closed record, got {other:?}"),
        }
    }

    #[test]
    fn open_record_with_a_field_the_closed_side_lacks_fails() {
        let mut sub = Substitution::new();
        let int_ty1 = app0(&mut sub, "Int");
        let int_ty2 = app0(&mut sub, "Int");
        let bool_ty = app0(&mut sub, "Bool");
        let closed = record(&mut sub, &[("x", int_ty1)], None);
        let e = sub.fresh_unbound();
        let open = record(&mut sub, &[("x", int_ty2), ("y", bool_ty)], Some(e));
        assert!(matches!(
            unify(&mut sub, closed, open),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn two_open_records_with_no_exclusive_fields_unify_their_extensions() {
        let mut sub = Substitution::new();
        let int_ty1 = app0(&mut sub, "Int");
        let int_ty2 = app0(&mut sub, "Int");
        let e1 = sub.fresh_unbound();
        let e2 = sub.fresh_unbound();
        let r1 = record(&mut sub, &[("x", int_ty1)], Some(e1));
        let r2 = record(&mut sub, &[("x", int_ty2)], Some(e2));
        assert!(unify(&mut sub, r1, r2).is_ok());
        assert_eq!(sub.find(e1), sub.find(e2));
    }

    #[test]
    fn two_open_records_each_absorb_the_others_exclusive_field() {
        // { x : Int | e1 } ~ { y : Bool | e2 } should succeed: each row is
        // open enough to also contain the other's field.
        let mut sub = Substitution::new();
        let int_ty = app0(&mut sub, "Int");
        let bool_ty = app0(&mut sub, "Bool");
        let e1 = sub.fresh_unbound();
        let e2 = sub.fresh_unbound();
        let r1 = record(&mut sub, &[("x", int_ty)], Some(e1));
        let r2 = record(&mut sub, &[("y", bool_ty)], Some(e2));

        assert!(unify(&mut sub, r1, r2).is_ok());

        match sub.resolve_structure(e1) {
            Some(Structure::Record(fields, _)) => {
                assert_eq!(
                    sub.resolve_structure(fields["y"]),
                    sub.resolve_structure(bool_ty)
                );
            }
            other => panic!("expected e1 to resolve to a record containing y, got {other:?}"),
        }
        match sub.resolve_structure(e2) {
            Some(Structure::Record(fields, _)) => {
                assert_eq!(
                    sub.resolve_structure(fields["x"]),
                    sub.resolve_structure(int_ty)
                );
            }
            other => panic!("expected e2 to resolve to a record containing x, got {other:?}"),
        }
    }

    #[test]
    fn rigid_row_variable_refuses_to_be_forced_closed() {
        // Models `distance :: { r | x : Float, y : Float } -> Float`'s own
        // body-check: `r` must stay opaque even when the rest of the record
        // happens to line up exactly with a closed record elsewhere -- a
        // signature's row variable can never be quietly narrowed to "exactly
        // these fields," or the polymorphism guarantee is a lie (plan §5).
        let mut sub = Substitution::new();
        let rigid_r = sub.fresh_rigid("r".to_string());
        let float_ty1 = app0(&mut sub, "Float");
        let float_ty2 = app0(&mut sub, "Float");
        let open = record(&mut sub, &[("x", float_ty1)], Some(rigid_r));
        let closed = record(&mut sub, &[("x", float_ty2)], None);
        assert!(matches!(
            unify(&mut sub, open, closed),
            Err(UnifyError::Mismatch { .. })
        ));
    }

    #[test]
    fn non_rigid_open_record_closes_to_empty_against_a_matching_closed_one() {
        let mut sub = Substitution::new();
        let e = sub.fresh_unbound();
        let float_ty1 = app0(&mut sub, "Float");
        let float_ty2 = app0(&mut sub, "Float");
        let open = record(&mut sub, &[("x", float_ty1)], Some(e));
        let closed = record(&mut sub, &[("x", float_ty2)], None);
        assert!(unify(&mut sub, open, closed).is_ok());
        assert_eq!(
            sub.resolve_structure(e),
            Some(Structure::Record(BTreeMap::new(), None))
        );
    }

    #[test]
    fn chained_open_record_unification_flattens_via_recursion() {
        // r1 = { x | e1 }, r2 = { y | e2 }: unifying them first makes e1/e2
        // each absorb the other's field plus a shared `rest`. Then unifying
        // r1 against a *third* record { z | e3 } must correctly account for
        // r1's field *and* whatever e1 already picked up (y), not just x --
        // exercising the "extension already bound" case, not only the
        // fresh-variable case the other tests cover.
        let mut sub = Substitution::new();
        let x_ty = app0(&mut sub, "Int");
        let y_ty = app0(&mut sub, "Int");
        let z_ty = app0(&mut sub, "Int");
        let e1 = sub.fresh_unbound();
        let e2 = sub.fresh_unbound();
        let r1 = record(&mut sub, &[("x", x_ty)], Some(e1));
        let r2 = record(&mut sub, &[("y", y_ty)], Some(e2));
        unify(&mut sub, r1, r2).unwrap();

        let e3 = sub.fresh_unbound();
        let r3 = record(&mut sub, &[("z", z_ty)], Some(e3));
        assert!(unify(&mut sub, r1, r3).is_ok());

        // e3 must now account for both of r1's fields beyond z: x directly,
        // and y transitively (via whatever r1's own e1 turned out to need).
        match sub.resolve_structure(e3) {
            Some(Structure::Record(fields, _)) => {
                assert!(fields.contains_key("x"), "e3 should carry x: {fields:?}");
            }
            other => panic!("expected e3 to resolve to a record, got {other:?}"),
        }
    }
}
