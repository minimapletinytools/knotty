//! `unify()` only ever sees `TypeVarId`s, never a span — `solve.rs` wraps a
//! bare `UnifyError` into a spanned `TypeError`, rendering the types
//! involved to text at that point (a `TypeVarId` means nothing once the
//! `Substitution` it indexes into is gone).

use knot_syntax::span::Span;

use crate::var::TypeVarId;

#[derive(Debug, Clone, PartialEq)]
pub enum UnifyError {
    Mismatch {
        expected: TypeVarId,
        actual: TypeVarId,
    },
    /// `var` occurs within `in_ty`'s own structure — binding would build an
    /// infinite type.
    Occurs { var: TypeVarId, in_ty: TypeVarId },
    /// A signature's rigid type variable would leak into an outer scope.
    RigidEscape { rigid: TypeVarId, other: TypeVarId },
}

/// Collected, not fatal — every error across a whole module is reported.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub span: Span,
    pub kind: TypeErrorKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeErrorKind {
    InterfaceArity {
        interface: String,
        expected: usize,
        actual: usize,
    },
    /// Inference produced an obligation with no dictionary binding in its
    /// lexical scope. Such evidence must never reach executable lowering.
    UnresolvedDictionary { interface: String, ty: String },
    /// Two types that had to be equal aren't.
    Mismatch { expected: String, actual: String },
    /// Unification would build an infinite type (`f x = x x`).
    InfiniteType { ty: String },
    /// A signature's type variable was forced to unify with something from
    /// outside the signature's own scope.
    EscapedTypeVariable { name: String },
    /// A reference with no scheme — defensive; `knot-canonical` rejects
    /// genuinely unbound names first.
    UnboundValue(String),
    /// An interface obligation on a type variable that nothing ever
    /// determines and that defaulting doesn't apply to (`show empty`).
    AmbiguousConstraint { interface: String, ty: String },
    /// A rigid (signature-bound) type variable was asked to satisfy an
    /// interface its signature never granted.
    NoInstanceForRigid { interface: String },
    /// No instance provides `interface` for this type.
    NoInstance { interface: String, ty: String },
    /// Two instances for the same `(interface, type)` pair.
    DuplicateInstance { interface: String },
    /// A superclass dictionary cannot be constructed from the instance context.
    MissingSuperclassInstance {
        interface: String,
        superclass: String,
    },
    /// An instance target that can't be given an instance at all (a type
    /// variable, a function type, a record, tuple, or unit).
    InstanceTargetNotNominal { interface: String },
    /// An instance head/context outside the supported v0 form.
    UnsupportedInstanceHead { reason: String },
    /// An instance defines a method its interface doesn't have.
    UnknownInstanceMethod { interface: String, method: String },
    /// An instance leaves out one of its interface's methods.
    MissingInstanceMethod { interface: String, method: String },
    /// Mutually defined defaults require an explicit implementation of at
    /// least one of these methods (currently Show's show/showsPrec pair).
    MissingMinimalInstanceMethods {
        interface: String,
        alternatives: Vec<String>,
    },
    /// An instance defines the same method twice.
    DuplicateInstanceMethod { interface: String, method: String },
    /// A `deriving` clause this checker can't derive for the type's shape.
    CannotDeriveInterface {
        interface: String,
        type_name: String,
        reason: String,
    },
    /// A typed hole (`_` in expression position) — always an error, reported
    /// with the type the surrounding code expects there.
    Hole { expected: String },
    /// A type used at the wrong kind (`Maybe -> Int`, `Int Int`, a row
    /// variable used as a type, `instance Functor Int`).
    KindMismatch {
        expected: String,
        actual: String,
        context: String,
    },
}
