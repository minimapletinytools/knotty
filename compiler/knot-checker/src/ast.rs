//! The typed AST: `CExpr`/`CPattern`'s shape with a type on every node,
//! produced by constraint generation alongside the constraints themselves.
//!
//! `Typed<T>` wraps a node with its `NodeId`, `Span` and `TypeVarId`;
//! `TExpr`/`TPattern` otherwise mirror `CExpr`/`CPattern` variant for
//! variant.
//!
//! **Obligations.** No node stores its interface obligations directly:
//! a `Var`/`Ctor` reference, an operator, and unary minus are all typed by
//! instantiating a scheme during solving, and `solve::ObligationMap` maps
//! the node's lookup type to that instantiation's obligations — which is
//! what `elaborate` uses to build dictionary evidence. Numeric literals
//! contribute `Num`/`Fractional` obligations keyed by their result type.
//!
//! `Do` retains sequencing and do-let statements for Tangle; its implicit
//! bind lookups carry evidence keys for later lowering. `Annotated` keeps
//! its annotations unelaborated; annotation checking is outside v0.

use knot_canonical::ast::{CAnnotation, Ref};
use knot_syntax::ast::pattern::PatternLiteral;
use knot_syntax::span::Span;

use crate::var::TypeVarId;

/// Unique within one checked module revision, including generated wrappers.
/// A source span is deliberately not an identity.
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord,
)]
pub struct NodeId(pub(crate) u32);

/// A bare `TExpr`/`TPattern` node plus the identity, span and type every node
/// needs — see module docs on why this lives outside the enums themselves.
#[derive(Debug, Clone, PartialEq)]
pub struct Typed<T> {
    pub id: NodeId,
    pub span: Span,
    pub ty: TypeVarId,
    pub node: T,
}

pub type TypedExpr = Typed<TExpr>;
pub type TypedPattern = Typed<TPattern>;

#[derive(Debug, Clone, PartialEq)]
pub enum TExpr {
    IntLit(num_bigint::BigInt),
    FloatLit(knot_syntax::decimal::DecimalLiteral),
    StringLit(String),
    Unit,
    /// See `constrain::expr`'s own doc comment: a hole's type is
    /// deliberately unconstrained, always a compile error to actually
    /// leave in a program, but that's a separate diagnostic concern from
    /// typing.
    Hole,
    Var(Ref),
    Ctor(Ref),
    Lambda(Vec<TypedPattern>, Box<TypedExpr>),
    App(Box<TypedExpr>, Box<TypedExpr>),
    /// An infix application. The `TypeVarId` is the type of the operator
    /// itself, looked up by its resolved name like any `Var` — the key
    /// into `solve::ObligationMap` for its instantiated obligations.
    BinOp(Ref, Box<TypedExpr>, Box<TypedExpr>, TypeVarId),
    /// Unary minus, typed as an application of `negate`; same key.
    Negate(Box<TypedExpr>, TypeVarId),
    If(Box<TypedExpr>, Box<TypedExpr>, Box<TypedExpr>),
    Let(Vec<(TypedPattern, TypedExpr)>, Box<TypedExpr>),
    Case(Box<TypedExpr>, Vec<(TypedPattern, TypedExpr)>),
    Do(Vec<TDoStmt>, Box<TypedExpr>),
    List(Vec<TypedExpr>),
    Tuple(Vec<TypedExpr>),
    Record(Vec<(String, TypedExpr)>),
    RecordUpdate(Box<TypedExpr>, Vec<(String, TypedExpr)>),
    FieldAccess(Box<TypedExpr>, String),
    /// Deliberately unelaborated — see module docs.
    Annotated(Vec<CAnnotation>, Box<TypedExpr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TDoStmt {
    /// The final field is the implicit `(>>=)` lookup's type/evidence key.
    Bind(TypedPattern, TypedExpr, TypeVarId),
    Expr(TypedExpr, TypeVarId),
    Let(Vec<(TypedPattern, TypedExpr)>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TPattern {
    Wildcard(Option<String>),
    Var(String),
    Literal(PatternLiteral),
    Ctor(Ref, Vec<TypedPattern>),
    Tuple(Vec<TypedPattern>),
    Cons(Box<TypedPattern>, Box<TypedPattern>),
    Nil,
    /// `{ x, y }` — each field's own fresh type variable, open-row
    /// constrained exactly like `TExpr::FieldAccess`/`RecordUpdate` already
    /// are (see `constrain::pattern`'s own `CPattern::Record` case). No
    /// obligations possible — purely structural, same as `Tuple`.
    Record(Vec<(String, TypeVarId)>),
    As(Box<TypedPattern>, String),
    Unit,
}

/// A nominal instance's identity. `Evidence::Instance` also carries its
/// context arguments. Lowering must connect this identity to the typed
/// method/default bodies and construct explicit dictionary code before
/// Twine executes the program.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Dictionary {
    pub interface: String,
    pub head: Ref,
}

/// Dictionary evidence, including explicit residual leaves for constraints
/// that must become parameters of generalized bindings before lowering.
pub type ObligationResolution = crate::interface::instance::Evidence;
