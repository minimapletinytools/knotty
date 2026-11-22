//! The Knot AST: nothing resolved — no name resolution, and infix chains are
//! kept flat (`Expr::OpChain`) for `knot-canonical` to group by fixity —
//! spans attached throughout. See `knot-ast-parser-plan.md` §4 for the full design
//! and the reasoning behind each shape.

pub mod decl;
pub mod expr;
pub mod pattern;
pub mod ty;

/// A possibly-qualified identifier, e.g. `map` or `List.map`, stored as its full
/// dotted spelling. Simplicity over performance: no separate qualifier type for now.
pub type Name = String;
