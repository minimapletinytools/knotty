use crate::ast::Name;

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// `Int`, `List a`, `Map k v`, `Maybe a`, ...
    Named(Name, Vec<Type>),
    Var(String),
    /// `f a`, `f a b` — a type application headed by a lowercase type
    /// *variable* rather than a fixed name, e.g. `Collection f => f a -> f
    /// b` (spec §10.5). Named to match `knot-checker::ty::Structure::
    /// VarApp`, the shape this eventually becomes once the variable is
    /// pinned to a concrete constructor — see that type's own doc comment.
    /// Only ever meaningful for a variable a signature's own constraint
    /// list gives `Collection`/`Context` (the only two interfaces with
    /// constructor-shaped methods); nothing here enforces that yet, since
    /// using an unconstrained or wrongly-constrained one simply has no way
    /// to ever resolve any obligation the body places on it.
    VarApp(String, Vec<Type>),
    Fn(Box<Type>, Box<Type>),
    /// Fixed arity, with at least two elements and no language-level maximum.
    Tuple(Vec<Type>),
    /// Fields plus an optional extension row variable, e.g. `{ r | x : Float }`.
    Record(Vec<(String, Type)>, Option<String>),
    Unit,
}

/// e.g. `Ord a` in `Ord a => a -> a -> a`.
#[derive(Debug, Clone, PartialEq)]
pub struct Constraint {
    pub interface: String,
    pub type_vars: Vec<String>,
}

/// A full `::` signature: an optional constraint list plus the type itself.
/// Constraints only ever appear as this prefix list before `=>` — they're a property
/// of the signature, not of `Type` itself, so `Type` stays unconstrained-only.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeSignature {
    pub constraints: Vec<Constraint>,
    pub ty: Type,
}
