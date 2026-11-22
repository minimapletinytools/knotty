//! Lower/upper/qualified identifiers, reserved words and holes. ASCII letters
//! and Unicode Other_Symbol characters (including emoji) form identifiers.
//! Emoji-leading names are value names; uppercase ASCII starts type/module names.

use crate::error::{ErrorKind, ParseError};
use crate::span::{Span, Spanned};
use crate::state::ParseState;
use unicode_general_category::{get_general_category, GeneralCategory};

/// Reserved words that can never be used as a `lower` identifier — checked
/// only by `lower_ident`, since every entry here is itself lowercase-leading
/// and an uppercase identifier could never collide with one regardless (see
/// `upper_ident_segment`, which never consults this list at all). `True`/
/// `False` deliberately aren't here for exactly that reason: they're
/// uppercase-leading prelude *constructors* (`knot-canonical::prelude::
/// BUILTIN_CONSTRUCTORS`), not syntax keywords, so listing them achieved
/// nothing but a misleading appearance of enforcement — a user can already
/// write `type Toggle = True | False` today, reusing those names as
/// perfectly ordinary constructors, since constructor parsing never checks
/// this list either.
///
/// `div`/`mod` are deliberately *not* here — they're ordinary functions, used
/// infix only when backticked (`` a `div` b ``).
///
/// `interface` and `where` are reserved even though `interface ... where` is never
/// user-written (it only documents built-ins, per spec §10) — `where` graduated to
/// real user-facing syntax via `instance ... where` (§10), so it's reserved on
/// that basis regardless.
pub const RESERVED_WORDS: &[&str] = &[
    "module",
    "exposing",
    "import",
    "as",
    "type",
    "alias",
    "let",
    "in",
    "if",
    "then",
    "else",
    "case",
    "of",
    "do",
    "interface",
    "where",
    "instance",
    "deriving",
    "infixl",
    "infixr",
    "infix",
];

fn is_emoji_symbol(c: char) -> bool {
    !c.is_ascii() && get_general_category(c) == GeneralCategory::OtherSymbol
}

/// Value-name classification shared with qualified references and exports.
pub fn is_value_ident_start(c: char) -> bool {
    c.is_ascii_lowercase() || is_emoji_symbol(c)
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '\'' || is_emoji_symbol(c)
}

impl<'a> ParseState<'a> {
    /// Decode only the next scalar, preserving byte offsets and an ASCII fast path.
    pub(crate) fn ident_char_at(&self, ahead: usize) -> Option<char> {
        let start = self.pos.offset as usize + ahead;
        let first = *self.src.get(start)?;
        if first.is_ascii() {
            return Some(char::from(first));
        }
        let width = match first {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return None,
        };
        std::str::from_utf8(self.src.get(start..start + width)?)
            .ok()?
            .chars()
            .next()
    }

    pub(crate) fn starts_value_ident(&self) -> bool {
        self.value_ident_at(0)
    }
    pub(crate) fn value_ident_at(&self, ahead: usize) -> bool {
        self.ident_char_at(ahead).is_some_and(is_value_ident_start)
    }
    fn bump_ident_char(&mut self, c: char) {
        for _ in 0..c.len_utf8() {
            self.bump();
        }
    }

    /// Maximal segment. Joiners/variation selectors/modifiers are accepted only
    /// within an emoji sequence; standalone invisible identifier starts are rejected.
    fn ident_segment(&mut self) -> Option<Spanned<String>> {
        let start = self.pos;
        let first = self.ident_char_at(0)?;
        if !first.is_ascii_alphabetic() && !is_emoji_symbol(first) {
            return None;
        }
        self.bump_ident_char(first);
        let mut emoji = is_emoji_symbol(first);
        while let Some(c) = self.ident_char_at(0) {
            if is_ident_continue(c) {
                emoji = is_emoji_symbol(c);
                self.bump_ident_char(c);
            } else if emoji
                && (matches!(c, '\u{fe0e}' | '\u{fe0f}' | '\u{1f3fb}'..='\u{1f3ff}')
                    || (c == '\u{200d}'
                        && self
                            .ident_char_at(c.len_utf8())
                            .is_some_and(is_emoji_symbol)))
            {
                self.bump_ident_char(c);
            } else {
                break;
            }
        }
        Some(Spanned::new(
            Span::new(start.offset, self.pos.offset),
            self.text_since(start.offset).to_string(),
        ))
    }

    /// A value identifier: `x`, `myVar2`, `🧶FloatAdd`. Errors if it's a reserved word.
    pub fn lower_ident(&mut self) -> Result<Spanned<String>, ParseError> {
        let start = self.pos;
        if !self.starts_value_ident() {
            return Err(ParseError::new(
                ErrorKind::Expected("a lowercase or emoji identifier"),
                Span::new(start.offset, start.offset),
            ));
        }
        let seg = self
            .ident_segment()
            .expect("checked value-name start above");
        if RESERVED_WORDS.contains(&seg.node.as_str()) {
            return Err(ParseError::new(
                ErrorKind::Custom(format!(
                    "`{}` is a reserved word, not an identifier",
                    seg.node
                )),
                seg.span,
            ));
        }
        Ok(seg)
    }

    /// A single uppercase-leading identifier segment, with no qualification: `Foo`,
    /// `List`. Use `qualified_upper_start` for names that may be dotted.
    pub fn upper_ident_segment(&mut self) -> Result<Spanned<String>, ParseError> {
        let start = self.pos;
        if !matches!(self.peek(), Some(b) if b.is_ascii_uppercase()) {
            return Err(ParseError::new(
                ErrorKind::Expected("an uppercase identifier"),
                Span::new(start.offset, start.offset),
            ));
        }
        Ok(self.ident_segment().expect("checked uppercase start above"))
    }

    /// A qualified name headed by an uppercase segment: `List`, `List.map`,
    /// `Shape.Circle`. Greedily consumes `.Upper` segments (module-path
    /// qualification); if the name ends in `.lower`, that final segment is consumed
    /// too (a qualified value/function reference) and qualification stops there —
    /// anything dotted on *after* that is a separate concern (field access) for the
    /// expression grammar, not this name. The `.` must have no surrounding
    /// whitespace, which falls out naturally here since this function never calls
    /// `skip_trivia` between segments.
    pub fn qualified_upper_start(&mut self) -> Result<Spanned<String>, ParseError> {
        let start = self.pos;
        let mut text = self.upper_ident_segment()?.node;
        loop {
            if self.peek() != Some(b'.') {
                break;
            }
            match self.peek_at(1) {
                Some(b) if b.is_ascii_uppercase() => {
                    self.bump(); // '.'
                    let seg = self.upper_ident_segment()?;
                    text.push('.');
                    text.push_str(&seg.node);
                }
                _ if self.value_ident_at(1) => {
                    self.bump(); // '.'
                    let seg = self.lower_ident()?;
                    text.push('.');
                    text.push_str(&seg.node);
                    break;
                }
                _ => break,
            }
        }
        Ok(Spanned::new(Span::new(start.offset, self.pos.offset), text))
    }

    /// `_` (anonymous hole) or `_name` (named hole). This layer doesn't enforce
    /// *where* a named hole may appear (pattern/binding-discard only, per spec
    /// §15) — that's the caller's job, since it depends on grammatical position.
    pub fn hole(&mut self) -> Result<Spanned<Option<String>>, ParseError> {
        let start = self.pos;
        if self.peek() != Some(b'_') {
            return Err(ParseError::new(
                ErrorKind::Expected("`_`"),
                Span::new(start.offset, start.offset),
            ));
        }
        self.bump();
        let name = self.ident_segment().map(|seg| seg.node);
        Ok(Spanned::new(Span::new(start.offset, self.pos.offset), name))
    }

    /// True if `keyword` (e.g. `"as"`, `"let"`, `"then"`) appears at the current
    /// position as a *complete* identifier, not a prefix of a longer one — so
    /// `peek_keyword("as")` doesn't fire in the middle of `asymptote`. Doesn't skip
    /// leading trivia or consume anything; callers that need either call
    /// `skip_trivia`/`expect_keyword` themselves.
    pub fn peek_keyword(&self, keyword: &str) -> bool {
        let bytes = keyword.as_bytes();
        let start = self.pos.offset as usize;
        let end = start + bytes.len();
        if end > self.src.len() || &self.src[start..end] != bytes {
            return false;
        }
        !self
            .ident_char_at(bytes.len())
            .is_some_and(is_ident_continue)
    }

    /// Skips trivia, then requires and consumes `keyword` per `peek_keyword`.
    pub fn expect_keyword(&mut self, keyword: &'static str) -> Result<(), ParseError> {
        self.skip_trivia()?;
        if !self.peek_keyword(keyword) {
            return Err(ParseError::new(
                ErrorKind::Expected(keyword),
                Span::new(self.pos.offset, self.pos.offset),
            ));
        }
        for _ in 0..keyword.len() {
            self.bump();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_ident_scans_maximal_run() {
        let mut s = ParseState::new("myVar2 rest");
        let id = s.lower_ident().unwrap();
        assert_eq!(id.node, "myVar2");
        assert_eq!(s.peek(), Some(b' '));
    }

    #[test]
    fn lower_ident_rejects_reserved_word() {
        let mut s = ParseState::new("let");
        assert!(s.lower_ident().is_err());
    }

    #[test]
    fn lower_ident_does_not_over_reject_reserved_prefix() {
        // "lets" is a totally different, valid identifier from "let" -- maximal
        // munch happens before the reserved-word comparison.
        let mut s = ParseState::new("lets");
        let id = s.lower_ident().unwrap();
        assert_eq!(id.node, "lets");
    }

    #[test]
    fn lower_ident_rejects_uppercase_start() {
        let mut s = ParseState::new("Foo");
        assert!(s.lower_ident().is_err());
    }

    #[test]
    fn upper_ident_segment_rejects_lowercase_start() {
        let mut s = ParseState::new("foo");
        assert!(s.upper_ident_segment().is_err());
    }

    #[test]
    fn qualified_name_stops_with_no_dot() {
        let mut s = ParseState::new("List rest");
        let name = s.qualified_upper_start().unwrap();
        assert_eq!(name.node, "List");
        assert_eq!(s.peek(), Some(b' '));
    }

    #[test]
    fn qualified_name_ends_on_lowercase_segment() {
        let mut s = ParseState::new("List.map xs");
        let name = s.qualified_upper_start().unwrap();
        assert_eq!(name.node, "List.map");
        assert_eq!(s.peek(), Some(b' '));
    }

    #[test]
    fn qualified_name_can_chain_uppercase_segments() {
        let mut s = ParseState::new("Shape.Circle");
        let name = s.qualified_upper_start().unwrap();
        assert_eq!(name.node, "Shape.Circle");
        assert!(s.is_eof());
    }

    #[test]
    fn qualified_name_stops_before_second_dot_after_lowercase() {
        // "List.map.foo" -- only one trailing lowercase segment is part of the
        // qualified name; the second dot is left for field-access grammar.
        let mut s = ParseState::new("List.map.foo");
        let name = s.qualified_upper_start().unwrap();
        assert_eq!(name.node, "List.map");
        assert_eq!(s.peek(), Some(b'.'));
    }

    #[test]
    fn anonymous_hole() {
        let mut s = ParseState::new("_ rest");
        let hole = s.hole().unwrap();
        assert_eq!(hole.node, None);
        assert_eq!(s.peek(), Some(b' '));
    }

    #[test]
    fn named_hole() {
        let mut s = ParseState::new("_debugValue)");
        let hole = s.hole().unwrap();
        assert_eq!(hole.node, Some("debugValue".to_string()));
        assert_eq!(s.peek(), Some(b')'));
    }

    #[test]
    fn hole_followed_by_digit_is_still_anonymous() {
        // `_1` is `_` followed by a separate token, not a named hole -- named
        // holes must look like a valid identifier (start with a letter).
        let mut s = ParseState::new("_1");
        let hole = s.hole().unwrap();
        assert_eq!(hole.node, None);
        assert_eq!(s.peek(), Some(b'1'));
    }
}

#[cfg(test)]
mod emoji_tests {
    use super::*;

    #[test]
    fn emoji_names_are_maximal_value_identifiers_with_byte_spans() {
        for name in [
            "🧶FloatAdd",
            "🍅",
            "paint🧶2'",
            "👩🏽‍💻Edit",
            "❤️Color",
            "🇨🇦Shape",
            "if🧶",
        ] {
            let source = format!("{name} next");
            let mut state = ParseState::new(&source);
            let token = state.lower_ident().unwrap();
            assert_eq!(token.node, name);
            assert_eq!(token.span, Span::new(0, name.len() as u32));
            assert_eq!(state.pos.col, name.chars().count() as u32 + 1);
        }
    }

    #[test]
    fn emoji_suffixes_do_not_split_keywords_and_invisible_starts_are_rejected() {
        assert!(!ParseState::new("if🧶").peek_keyword("if"));
        assert!(ParseState::new("if 🧶").peek_keyword("if"));
        for name in ["\u{200d}name", "\u{fe0f}name", "🏽name"] {
            assert!(ParseState::new(name).lower_ident().is_err(), "{name:?}");
        }
    }
}
