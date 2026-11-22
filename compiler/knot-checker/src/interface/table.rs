//! The closed interface set, read from the prelude (`knot_canonical::
//! prelude`, `Prelude.knot`): which interfaces exist, their superclasses,
//! their own type variable, their methods' declared signatures, and which
//! methods have defaults. Nothing
//! here is restated by hand — instance-method checking, operator typing,
//! and the prelude's value schemes all read the same parsed signatures.

use knot_canonical::prelude::{prelude, BuiltinInterface};
use knot_syntax::ast::ty::{Type, TypeSignature};

pub fn interface(name: &str) -> Option<&'static BuiltinInterface> {
    prelude().interface(name)
}

pub fn is_known_interface(name: &str) -> bool {
    interface(name).is_some()
}

pub fn superclasses(name: &str) -> &'static [String] {
    interface(name)
        .map(|i| i.superclasses.as_slice())
        .unwrap_or(&[])
}

/// The declared signature of `interface`'s `method`.
pub fn method_signature(interface_name: &str, method: &str) -> Option<&'static TypeSignature> {
    interface(interface_name)?
        .methods
        .iter()
        .find(|(name, _)| name == method)
        .map(|(_, sig)| sig)
}

/// How many type arguments the interface's own variable takes in its
/// methods' signatures: 0 for `Eq a` (instances are types), 1 for
/// `Functor f` (instances are one-argument type constructors).
pub fn interface_arity(name: &str) -> usize {
    parameter_arity(name, 0)
}

pub fn parameter_arity(name: &str, position: usize) -> usize {
    let Some(iface) = interface(name) else {
        return 0;
    };
    let Some(var) = iface.vars.get(position) else {
        return 0;
    };
    fn arity_in(ty: &Type, var: &str) -> Option<usize> {
        match ty {
            Type::VarApp(v, args) if v == var => Some(args.len()),
            Type::VarApp(_, args) | Type::Named(_, args) | Type::Tuple(args) => {
                args.iter().find_map(|a| arity_in(a, var))
            }
            Type::Fn(a, b) => arity_in(a, var).or_else(|| arity_in(b, var)),
            Type::Record(fields, _) => fields.iter().find_map(|(_, t)| arity_in(t, var)),
            Type::Var(_) | Type::Unit => None,
        }
    }
    iface
        .methods
        .iter()
        .find_map(|(_, sig)| arity_in(&sig.ty, var))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spec_interface_is_known() {
        for name in [
            "Eq",
            "Ord",
            "Show",
            "Semigroup",
            "Monoid",
            "Num",
            "Fractional",
            "Integral",
            "Functor",
            "Applicative",
            "Monad",
            "Foldable",
            "Traversable",
        ] {
            assert!(is_known_interface(name), "{name} should be known");
        }
        assert!(!is_known_interface("Frobnicable"));
    }

    #[test]
    fn superclasses_match_the_spec() {
        assert_eq!(superclasses("Ord"), &["Eq"]);
        assert_eq!(superclasses("Monoid"), &["Semigroup"]);
        assert_eq!(superclasses("Fractional"), &["Num"]);
        assert_eq!(superclasses("Integral"), &["Real", "Enum"]);
        assert_eq!(superclasses("Applicative"), &["Functor"]);
        assert_eq!(superclasses("Monad"), &["Applicative"]);
        // As in Haskell, Foldable doesn't require Functor.
        assert!(superclasses("Foldable").is_empty());
        assert_eq!(superclasses("Traversable"), &["Functor", "Foldable"]);
        assert!(superclasses("Eq").is_empty());
    }

    #[test]
    fn methods_are_found_by_name() {
        assert!(method_signature("Eq", "==").is_some());
        assert!(method_signature("Num", "signum").is_some());
        assert!(method_signature("Monad", ">>=").is_some());
        assert!(method_signature("Eq", "bogus").is_none());
        assert!(method_signature("Foldable", "filter").is_none());
    }

    #[test]
    fn constructor_interfaces_have_arity_one() {
        assert_eq!(interface_arity("Eq"), 0);
        assert_eq!(interface_arity("Monoid"), 0);
        assert_eq!(interface_arity("Functor"), 1);
        assert_eq!(interface_arity("Traversable"), 1);
    }
}
