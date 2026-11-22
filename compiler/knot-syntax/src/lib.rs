//! `knot-syntax` — parses Knot source text into an AST: spans attached, nothing
//! resolved (no name resolution, no type checking), and infix chains left flat
//! for `knot-canonical` to group by fixity. See `knot-ast-parser-plan.md` at the repo root for the full design and
//! `language-spec-notes.md` for the language being parsed.

pub mod ast;
pub mod decimal;
pub mod error;
pub mod lex;
pub mod line_index;
pub mod parse;
pub mod span;
pub mod state;
pub mod validate;

pub use error::{ErrorKind, ParseError};
pub use span::{Cursor, Span, Spanned};
pub use state::ParseState;

/// Parses a complete Knot module: header, imports, declarations, then the
/// post-parse structural checks (duplicate top-level bindings —
/// see `validate`). Bails on the first error, matching Elm's own v0 behavior,
/// rather than attempting multi-error recovery.
pub fn parse(source: &str) -> Result<ast::decl::Module, ParseError> {
    parse_with(source, validate::validate_module)
}

/// Like `parse`, for a library module (the prelude): it may also declare
/// interfaces, primitives, and opaque types.
pub fn parse_library(source: &str) -> Result<ast::decl::Module, ParseError> {
    parse_with(source, validate::validate_library_module)
}

fn parse_with(
    source: &str,
    validate: fn(&ast::decl::Module) -> Vec<validate::ValidationError>,
) -> Result<ast::decl::Module, ParseError> {
    let mut state = ParseState::new(source);
    let module = state.parse_module()?;
    if let Some(first) = validate(&module).into_iter().next() {
        return Err(ParseError::new(
            ErrorKind::Custom(first.message),
            first.span,
        ));
    }
    Ok(module)
}
