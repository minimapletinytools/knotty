use crate::ast::expr::{Annotation, Expr};
use crate::ast::pattern::Pattern;
use crate::ast::ty::{Constraint, Type, TypeSignature};
use crate::span::Spanned;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assoc {
    Left,
    Right,
    /// `infix`: `a ~= b ~= c` is an error rather than grouping either way.
    None,
}

/// How an infix operator groups: precedence 0 (loosest) to 9 (tightest),
/// plus associativity. Application binds tighter than any operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixity {
    pub assoc: Assoc,
    pub precedence: u8,
}

impl Fixity {
    /// An operator with no fixity declaration is `infixl 9` (spec §7.4).
    pub const DEFAULT: Fixity = Fixity {
        assoc: Assoc::Left,
        precedence: 9,
    };
}

/// `infixl 6 (+++)`, `` infixl 7 `div`, `mod` ``: one fixity for one or more
/// operators, each named by its symbol or its backticked identifier.
#[derive(Debug, Clone, PartialEq)]
pub struct FixityDecl {
    pub fixity: Fixity,
    pub operators: Vec<Spanned<String>>,
}

/// A type signature (`name :: Type`) and its definition (`name arg = expr`) are two
/// source lines but *one* node: the declaration parser looks ahead after an optional
/// signature line and merges it with the following definition for the same name.
/// Stacked/block annotations sit above the signature line but conceptually annotate
/// the merged binding, so they attach here — there's no separate `TypeSig` variant.
#[derive(Debug, Clone, PartialEq)]
pub struct FnDef {
    pub name: String,
    pub signature: Option<Spanned<TypeSignature>>,
    pub params: Vec<Spanned<Pattern>>,
    pub body: Spanned<Expr>,
    pub annotations: Vec<Annotation>,
}

/// Haskell-style: `instance Eq Shape where (==) a b = ...`. Methods reuse the same
/// one-equation-per-name rule as any other function (spec §2.3) — no special-casing.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceDecl {
    pub interface: String,
    /// e.g. `Eq a =>` for `instance Eq a => Eq (Maybe a)`.
    pub constraints: Vec<Constraint>,
    /// One atom per interface parameter: `Shape`, `(Maybe a)`, or both
    /// `(Maybe t)` and `t` in a binary instance.
    pub targets: Vec<Type>,
    /// Each method's `signature` is always `None` — inherited from the interface.
    pub methods: Vec<FnDef>,
}

/// `interface Eq a => Ord a where ...`: one of the closed set of
/// interfaces. Only a library module (the prelude) may declare one. Each
/// method has a signature; a method may also have a default body, used by
/// any instance that doesn't define it (spec §10.2).
#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceDecl {
    pub name: String,
    /// The interface's own type variables (`a` in `Eq a`, `s t` in `Constriction s t`).
    pub vars: Vec<String>,
    /// `Eq a =>` for `interface Eq a => Ord a`.
    pub superclasses: Vec<Constraint>,
    pub methods: Vec<(String, Spanned<TypeSignature>)>,
    /// Default bodies. Each `signature` is `None`: it's the method's.
    pub defaults: Vec<FnDef>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decl {
    Fn(FnDef),
    TypeAlias(String, Vec<String>, Type),
    /// ADT: name, type params, variants (each a constructor name + its
    /// argument types), and derived interfaces (`deriving (Eq, Ord, Show)`,
    /// empty if no `deriving` clause at all) — `type alias` has no
    /// equivalent, since a closed/open record or tuple alias already gets
    /// automatic structural `Eq`/`Ord`/`Show` a different way (spec §10.4).
    TypeDecl(String, Vec<String>, Vec<(String, Vec<Type>)>, Vec<String>),
    Instance(InstanceDecl),
    Fixity(FixityDecl),
    Interface(InterfaceDecl),
    /// `primIntAdd :: Int -> Int -> Int` with no definition: a value the
    /// runtime provides. Only a library module may declare one.
    Primitive(String, Spanned<TypeSignature>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Exposing {
    All,
    Some(Vec<ExposedItem>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExposedItem {
    Value(String),
    /// `Foo` — the type but not its constructors.
    TypeOnly(String),
    /// `Foo(..)` — the type and all its constructors.
    TypeWithVariants(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub module: Vec<String>,
    pub alias: Option<String>,
    pub exposing: Option<Exposing>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub name: Vec<String>,
    pub exposing: Exposing,
    pub imports: Vec<Import>,
    pub decls: Vec<Spanned<Decl>>,
}
