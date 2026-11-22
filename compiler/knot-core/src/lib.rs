//! Executable, type-erased Knot. Arena edges keep the runtime and core printer
//! independent of the native call stack. The typed AST remains Tangle's input.
mod arena;
pub mod free_inputs;
pub use arena::Expressions;
mod local_names;
pub use local_names::LocalNames;
mod globals;
pub use globals::Globals;
pub mod equivalence;
mod generate;
pub mod lower;
pub use knot_checker::host::{HostDeclarations, HostSignature, HostType};
pub use knot_checker::interface::instance::StructuralShape;
pub use knot_checker::program::GlobalName;
pub use knot_checker::program::NodeRef;
pub use knot_syntax::decimal::DecimalLiteral;
pub use knot_syntax::span::Span;
pub use lower::lower;
use num_bigint::BigInt;
use std::collections::BTreeMap;

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Primary,
    Synthetic,
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub node: NodeRef,
    pub span: Span,
    pub role: Role,
}
impl Origin {
    pub fn synthetic(self) -> Self {
        Self {
            role: Role::Synthetic,
            ..self
        }
    }
}
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct ExprId(pub usize);
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub struct LocalId(pub usize);
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
pub enum Global {
    Value(GlobalName),
    Instance(String, GlobalName),
    Default(String, String),
    Structural(String, StructuralShape),
}
pub fn prelude(name: &str) -> GlobalName {
    GlobalName {
        module: vec!["Prelude".into()],
        name: name.into(),
    }
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Field {
    Named(String),
    Index(usize),
    Method(String, String),
    Super(String),
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum Literal {
    Float(f64),
    Int(i64),
    Integer(BigInt),
    String(String),
    Unit,
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum Pattern {
    Wildcard,
    Bind(LocalId),
    As(LocalId, Box<Pattern>),
    Lit(Literal),
    Ctor(GlobalName, Vec<Pattern>),
    Tuple(Vec<Pattern>),
    Record(Vec<(String, LocalId)>),
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum Kind {
    /// A compact exact Ratio Integer argument to Fractional.fromRational.
    Rational(DecimalLiteral),
    Lit(Literal),
    Local(LocalId),
    Global(Global),
    Prim(String),
    /// A fully qualified host call with its checked signature retained for linking.
    Host(GlobalName, HostSignature),
    Ctor(GlobalName, usize),
    Lam(LocalId, ExprId),
    App(ExprId, Vec<ExprId>),
    Let(Vec<(LocalId, ExprId)>, ExprId),
    Case(ExprId, Vec<(Pattern, ExprId)>),
    Tuple(Vec<ExprId>),
    Record(Vec<(String, ExprId)>),
    RecordUpdate(ExprId, Vec<(String, ExprId)>),
    Select(ExprId, Field),
    Dict(Vec<(Field, ExprId)>),
    /// Explicit boundary of the first runtime slice; never silently approximated.
    Unsupported(String),
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct Expr {
    pub origin: Origin,
    pub kind: Kind,
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Default, Clone)]
pub struct Program {
    pub expressions: Expressions,
    pub globals: Globals,
    pub local_names: LocalNames,
    pub next_local: usize,
}
impl Program {
    pub fn push(&mut self, origin: Origin, kind: Kind) -> ExprId {
        let id = ExprId(self.expressions.len());
        self.expressions.push(Expr { origin, kind });
        id
    }
    /// Stable arena listing: IDs are local to this core build; source revision
    /// nonces are intentionally omitted so snapshots can be compared.
    pub fn print(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        for (name, id) in &self.globals {
            writeln!(out, "{name:?} = %{}", id.0).unwrap();
        }
        for (i, expr) in self.expressions.iter().enumerate() {
            writeln!(
                out,
                "%{i} @{}..{} {:?} = {:?}",
                expr.origin.span.start, expr.origin.span.end, expr.origin.role, expr.kind
            )
            .unwrap();
        }
        out
    }
}
/// Two's-complement wrapping, including arbitrarily large source literals.
pub fn wrapping_int(n: &BigInt) -> i64 {
    let (_, digits) = n.to_u64_digits();
    let low = digits.first().copied().unwrap_or(0);
    if n.sign() == num_bigint::Sign::Minus {
        low.wrapping_neg() as i64
    } else {
        low as i64
    }
}

pub mod link;
