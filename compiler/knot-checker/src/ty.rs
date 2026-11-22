//! Type shapes (`Structure`) and generalized schemes (`Scheme`).

use std::collections::BTreeMap;

use knot_canonical::ast::Ref;

use crate::var::TypeVarId;

#[derive(Debug, Clone, PartialEq)]
pub enum Structure {
    /// A named type applied to its arguments: `Int`, `List a`, `Map k v`,
    /// builtin or user-defined.
    App(Ref, Vec<TypeVarId>),
    Fn(TypeVarId, TypeVarId),
    Tuple(Vec<TypeVarId>),
    /// Ordered arguments of a multi-parameter interface obligation. This is
    /// an internal dictionary type, never a source-level tuple/value type.
    /// Unary obligations retain their sole argument directly.
    Predicate(Vec<TypeVarId>),
    /// Fields plus an optional row-extension variable (spec §5.4); `None`
    /// is a closed record.
    Record(BTreeMap<String, TypeVarId>, Option<TypeVarId>),
    Unit,
    /// A type constructor, partially applied to its leading arguments —
    /// what a constructor variable (`f` in `f a`) is bound to once
    /// unification meets a concrete type: `List` for `List a`, `Result e`
    /// for `Result e a`. Only ever the binding of a `VarApp` head.
    Ctor(Ref, Vec<TypeVarId>),
    /// An application headed by a constructor *variable*: `f a` in
    /// `map :: Functor f => (a -> b) -> f a -> f b`. The head is an
    /// ordinary type variable; once it's bound to a `Ctor`, the whole
    /// thing means that constructor applied to these arguments (see
    /// `Substitution::head_normal`).
    VarApp(TypeVarId, Vec<TypeVarId>),
}

/// A quantified type: `forall vars. constraints => ty`. Instantiating it
/// (`solve::instantiate`) replaces every `vars` entry with a fresh variable
/// and substitutes all arguments of each constraint. A constraint's arena node
/// is its sole type argument, or `Structure::Predicate` for multiple arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Scheme {
    pub vars: Vec<TypeVarId>,
    pub constraints: Vec<(TypeVarId, String)>,
    pub ty: TypeVarId,
}

impl Scheme {
    /// No quantified variables and no constraints.
    pub fn monomorphic(ty: TypeVarId) -> Self {
        Scheme {
            vars: Vec::new(),
            constraints: Vec::new(),
            ty,
        }
    }
}
