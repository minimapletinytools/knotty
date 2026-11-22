use crate::ast::pattern::Pattern;
use crate::ast::Name;
use crate::span::Spanned;

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    IntLit(num_bigint::BigInt),
    FloatLit(crate::decimal::DecimalLiteral),
    StringLit(String),
    Unit,
    /// Lowercase, possibly qualified: `x`, `List.map`.
    Var(Name),
    /// Uppercase, possibly qualified: `Circle`, `Shape.Circle` — also covers
    /// `True`/`False`, which are just 0-arity constructors (no separate `BoolLit`,
    /// consistent with how `Pattern::Ctor` already treats them).
    Ctor(Name),
    /// `_` only — named holes (`_name`) are pattern/binding-only, never an
    /// expression placeholder (`f a _b c` is invalid; see language-spec-notes.md §15).
    Hole,
    Lambda(Vec<Spanned<Pattern>>, Box<Spanned<Expr>>),
    App(Box<Spanned<Expr>>, Box<Spanned<Expr>>),
    /// `a + b * c`, `` x `div` 2 ``: the operands and operators of an infix
    /// chain, in source order and *not* yet grouped. Grouping needs each
    /// operator's fixity, which can be declared anywhere in the module (or,
    /// later, imported), so `knot-canonical` resolves the chain once every
    /// name is known — the same split Haskell and Elm make. Each operator is
    /// its name: the symbol (`+`) or the backticked identifier (`div`,
    /// `Map.member`). Never empty.
    OpChain(Box<Spanned<Expr>>, Vec<(Spanned<Name>, Spanned<Expr>)>),
    /// A bare operator in parens (`(+)`, `(+++)`): a reference to that
    /// operator's own function, the same value `+` names infix (spec §7.6).
    /// `(+) a b` is ordinary application of it.
    OpRef(Name),
    /// Unary negation — whitespace-disambiguated from subtraction, see spec §7.5.
    Negate(Box<Spanned<Expr>>),
    If(Box<Spanned<Expr>>, Box<Spanned<Expr>>, Box<Spanned<Expr>>),
    Let(Vec<(Spanned<Pattern>, Spanned<Expr>)>, Box<Spanned<Expr>>),
    Case(Box<Spanned<Expr>>, Vec<(Spanned<Pattern>, Spanned<Expr>)>),
    Do(Vec<DoStmt>, Box<Spanned<Expr>>),
    List(Vec<Spanned<Expr>>),
    /// Fixed arity, with at least two elements and no language-level maximum.
    Tuple(Vec<Spanned<Expr>>),
    Record(Vec<(String, Spanned<Expr>)>),
    RecordUpdate(Box<Spanned<Expr>>, Vec<(String, Spanned<Expr>)>),
    FieldAccess(Box<Spanned<Expr>>, String),
    /// Prefix `@ann` attached to the closest-*following* atom only — never to a
    /// wider application or operator expression (spec §13.3's binding rule).
    Annotated(Vec<Annotation>, Box<Spanned<Expr>>),
}

/// A single `do`-block statement, retained for the graph-facing AST.
#[derive(Debug, Clone, PartialEq)]
pub enum DoStmt {
    Bind(Spanned<Pattern>, Spanned<Expr>),
    Expr(Spanned<Expr>),
    Let(Vec<(Spanned<Pattern>, Spanned<Expr>)>),
}

/// `@name(args)` desugars into this shape at parse time (single arg -> bare value,
/// multiple args -> tuple, per spec §13 desugaring rule) — one representation, not two.
#[derive(Debug, Clone, PartialEq)]
pub struct Annotation {
    pub key: String,
    pub value: Spanned<Expr>,
}
