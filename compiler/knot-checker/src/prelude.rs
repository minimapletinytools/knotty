//! Seeds the scheme environment and instance table from the library
//! modules (`knot_canonical::prelude`: `Prelude.knot`, `Map.knot`). Every builtin value,
//! constructor, interface method, and operator gets its scheme from its
//! declared signature; every builtin instance comes from an `instance`
//! line. Nothing is restated here by hand.

use std::collections::HashMap;

use knot_canonical::prelude::{builtin_type_ref, prelude};
use knot_syntax::ast::ty::{Constraint, Type, TypeSignature};

use crate::interface::instance::InstanceTable;
use crate::sig::{build, VarBinding};
use crate::solve::{SchemeEnv, SchemeKey};
use crate::ty::Scheme;
use crate::var::{Substitution, TypeVarId};

/// Start a check with independent mutable state. The bundled prelude is fixed
/// for this binary, so construct its initial arena/schemes/instances once and
/// copy the schemes and arena together: scheme IDs refer to slots in that arena.
/// Hosts, imported modules and user declarations are added only to the clone.
pub(crate) fn initial_state() -> &'static (Substitution, SchemeEnv, InstanceTable) {
    static INITIAL: std::sync::OnceLock<(Substitution, SchemeEnv, InstanceTable)> =
        std::sync::OnceLock::new();
    INITIAL.get_or_init(|| {
        let mut sub = Substitution::new();
        let (env, table) = seed(&mut sub);
        (sub, env, table)
    })
}

pub fn seed(sub: &mut Substitution) -> (SchemeEnv, InstanceTable) {
    let mut env = SchemeEnv::new();
    let mut table = InstanceTable::new();
    let p = prelude();

    for ctor in &p.ctors {
        let scheme = scheme_from(sub, &ctor.sig, &[]);
        env.insert(SchemeKey::Builtin(ctor.name.clone()), scheme);
    }
    for iface in &p.interfaces {
        let class_constraint = Constraint {
            interface: iface.name.clone(),
            type_vars: iface.vars.clone(),
        };
        for (method, sig) in &iface.methods {
            let scheme = scheme_from(sub, sig, std::slice::from_ref(&class_constraint));
            env.insert(SchemeKey::Builtin(method.clone()), scheme);
        }
    }
    for value in &p.values {
        let scheme = scheme_from(sub, &value.sig, &[]);
        let key = if value.module.is_empty() {
            SchemeKey::Builtin(value.name.clone())
        } else {
            SchemeKey::Imported(value.module.clone(), value.name.clone())
        };
        env.insert(key, scheme);
    }
    for inst in &p.instances {
        let targets = inst.targets.iter().map(canonical_type).collect::<Vec<_>>();
        let context = inst
            .context
            .iter()
            .map(|c| knot_canonical::ast::CConstraint {
                interface: c.interface.clone(),
                type_vars: c.type_vars.clone(),
            })
            .collect::<Vec<_>>();
        if targets.len() == 1 && context.iter().all(|c| c.type_vars.len() == 1) {
            let knot_canonical::ast::CType::Named(head, _) = &targets[0] else {
                panic!("unary prelude instance must be nominal");
            };
            table.insert_builtin(
                &inst.interface,
                head.clone(),
                crate::interface::instance::instance_requires(&targets[0], &context),
            );
        } else {
            let head = crate::interface::instance::dictionary_head(&inst.interface, &targets);
            let name = match head {
                knot_canonical::ast::Ref::TopLevel(n) => n,
                _ => unreachable!(),
            };
            table.insert_relation(
                &inst.interface,
                knot_canonical::ast::Ref::Builtin(name),
                targets,
                context,
                crate::interface::instance::InstanceKind::BuiltIn,
            );
        }
    }
    (env, table)
}

/// A fully generalized scheme for one prelude signature: every type
/// variable quantified, `extra` constraints added to the signature's own.
fn scheme_from(sub: &mut Substitution, sig: &TypeSignature, extra: &[Constraint]) -> Scheme {
    let mut vars: HashMap<String, VarBinding> = HashMap::new();
    let mut order: Vec<TypeVarId> = Vec::new();
    let ty = build(sub, &sig.ty, &mut vars, &mut |sub, _| {
        let v = sub.fresh_unbound();
        order.push(v);
        v
    });
    let constraints = sig
        .constraints
        .iter()
        .chain(extra)
        .filter_map(|c| {
            crate::sig::constraint_args(sub, &vars, &c.type_vars).map(|v| (v, c.interface.clone()))
        })
        .collect();
    Scheme {
        vars: order,
        constraints,
        ty,
    }
}

/// Prelude patterns only use variables and nominal applications.
fn canonical_type(ty: &Type) -> knot_canonical::ast::CType {
    use knot_canonical::ast::CType;
    match ty {
        Type::Var(v) => CType::Var(v.clone()),
        Type::Named(n, args) => CType::Named(
            builtin_type_ref(n),
            args.iter().map(canonical_type).collect(),
        ),
        _ => panic!("unsupported prelude instance pattern"),
    }
}

#[cfg(test)]
mod tests {
    use crate::check::check_source;

    fn types_of(src: &str) -> Vec<(String, String)> {
        let report = check_source(src);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        report.types
    }

    fn type_of(src: &str, name: &str) -> String {
        types_of(src)
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("no binding {name}"))
            .1
    }

    fn rejects(src: &str, kind_prefix: &str) {
        let report = check_source(src);
        assert!(
            report
                .errors
                .iter()
                .any(|e| format!("{:?}", e.kind).starts_with(kind_prefix)),
            "expected {kind_prefix}, got {:?}",
            report.errors
        );
    }

    #[test]
    fn fresh_prelude_state_isolated_from_prior_check_mutations() {
        use super::*;
        use crate::ty::Structure;
        use knot_canonical::ast::Ref;

        let (mut first, mut env, mut table) = initial_state().clone();
        let just = SchemeKey::Builtin("Just".into());
        let original = env.get(&just).unwrap().clone();
        first.bind(
            original.vars[0],
            Structure::App(Ref::Builtin("Int".into()), vec![]),
        );
        first.set_level(7);
        let first_node = first.fresh_node();
        first.fresh_node();
        env.insert(just.clone(), Scheme::monomorphic(original.vars[0]));
        let local = Ref::TopLevel("LocalOnly".into());
        table.insert_builtin("Eq", local.clone(), vec![]);

        let (mut second, env, table) = initial_state().clone();
        assert_eq!(second.level(), 0);
        assert_eq!(second.fresh_node(), first_node);
        assert_eq!(env.get(&just), Some(&original));
        assert!(second.is_unbound(original.vars[0]));
        assert!(!table.has_instance("Eq", &local));
        assert!(table.has_instance("Eq", &Ref::Builtin("Int".into())));
        assert_eq!(
            crate::print::print_scheme(&mut second, &original),
            "a -> Maybe a"
        );
    }

    #[test]
    fn builtin_values_have_their_declared_types() {
        assert_eq!(type_of("f = compare\n", "f"), "Ord a => a -> a -> Ordering");
        assert_eq!(
            type_of("f = map\n", "f"),
            "Functor c => (a -> b) -> c a -> c b"
        );
        assert_eq!(
            type_of("f = traverse\n", "f"),
            "(Applicative b, Traversable d) => (a -> b c) -> d a -> b (d c)"
        );
        assert_eq!(type_of("f = Just\n", "f"), "a -> Maybe a");
        assert_eq!(
            type_of("f = Map.get\n", "f"),
            "Ord a => a -> Map.Map a b -> Maybe b"
        );
    }

    #[test]
    fn operators_are_typed_through_the_prelude() {
        assert_eq!(type_of("f x y = x + y\n", "f"), "Num a => a -> a -> a");
        assert_eq!(type_of("f x y = x < y\n", "f"), "Ord a => a -> a -> Bool");
        assert_eq!(type_of("f x xs = x : xs\n", "f"), "a -> List a -> List a");
        assert_eq!(type_of("f = (<>)\n", "f"), "Semigroup a => a -> a -> a");
        assert_eq!(type_of("f x = -x\n", "f"), "Num a => a -> a");
        assert_eq!(type_of("f g x = x |> g\n", "f"), "(a -> b) -> a -> b");
    }

    #[test]
    fn builtin_instances_resolve() {
        assert_eq!(type_of("f = compare LT GT == EQ\n", "f"), "Bool");
        assert_eq!(type_of("f = \"a\" <> \"b\"\n", "f"), "String");
        assert_eq!(type_of("f = [1] <> empty\n", "f"), "List Int");
        assert_eq!(type_of("f = show (Just [1.5])\n", "f"), "String");
        assert_eq!(
            type_of("f = traverse (\\x -> Just x) [1]\n", "f"),
            "Maybe (List Int)"
        );
        assert_eq!(type_of("f = fromIntegral 3 / 2.0\n", "f"), "Float");
    }

    #[test]
    fn missing_builtin_instances_are_rejected() {
        // `1.5` is `Fractional a`, `mod` needs `Integral a`, and no default
        // fits both: ambiguous, as in Haskell.
        rejects("f = mod 1.5 2.0\n", "AmbiguousConstraint");
        rejects("f = map (\\x -> x) 5\n", "NoInstance");
        rejects("f = \"a\" + \"b\"\n", "NoInstance");
    }

    #[test]
    fn do_notation_works_over_two_parameter_types() {
        assert_eq!(
            type_of(
                "f :: Result String Int\nf = do\n  x <- Ok 1\n  y <- Ok 2\n  pure (x + y)\n",
                "f"
            ),
            "Result String Int"
        );
    }
}
