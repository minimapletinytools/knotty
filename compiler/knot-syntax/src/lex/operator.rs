//! Operator symbols: a maximal run of operator characters, e.g. `+`, `<>`,
//! `|>`, or a user-defined `+++` (spec §4.2, §7.4).
//!
//! The character set leaves out the symbols that already mean something
//! else: `.` (field access and qualified names), `@` (annotations), `\`
//! (lambdas), `$` (Knot has no `$`, so `f $ x` stays a parse error), and
//! brackets, commas, quotes and backticks. A few runs are reserved syntax
//! rather than operators. `--` always starts a comment, even in the middle
//! of a run, so a symbol stops before it.

use crate::state::ParseState;

pub const OPERATOR_CHARS: &[u8] = b"!#%&*+-/:<=>?^|~";

/// Symbol runs that are syntax, never an operator name.
pub const RESERVED_OPERATORS: &[&str] = &["=", "|", "->", "<-", "::", "=>"];

pub fn is_operator_char(byte: u8) -> bool {
    OPERATOR_CHARS.contains(&byte)
}

impl<'a> ParseState<'a> {
    /// The operator symbol starting at the cursor, without consuming it.
    /// `None` if there is none, or if the run is reserved syntax.
    pub fn peek_operator_symbol(&self) -> Option<&'a str> {
        let src = self.src;
        let start = self.pos.offset as usize;
        let mut end = start;
        while end < src.len() && is_operator_char(src[end]) {
            if src[end] == b'-' && src.get(end + 1) == Some(&b'-') {
                break;
            }
            end += 1;
        }
        if end == start {
            return None;
        }
        let symbol = std::str::from_utf8(&src[start..end]).ok()?;
        if RESERVED_OPERATORS.contains(&symbol) {
            return None;
        }
        Some(symbol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peek(src: &str) -> Option<&str> {
        ParseState::new(src).peek_operator_symbol()
    }

    #[test]
    fn takes_the_longest_run() {
        assert_eq!(peek("+++ x"), Some("+++"));
        assert_eq!(peek(">>= f"), Some(">>="));
        assert_eq!(peek("<*>x"), Some("<*>"));
        assert_eq!(peek("== b"), Some("=="));
    }

    #[test]
    fn reserved_runs_are_not_operators() {
        for s in ["= x", "| x", "-> x", "<- x", ":: T", "=> a"] {
            assert_eq!(peek(s), None, "{s}");
        }
        assert_eq!(peek("|| x"), Some("||"));
        assert_eq!(peek(": xs"), Some(":"));
    }

    #[test]
    fn stops_before_a_comment() {
        assert_eq!(peek("+-- note"), Some("+"));
        assert_eq!(peek("-- note"), None);
    }

    #[test]
    fn excluded_characters_end_or_prevent_a_symbol() {
        assert_eq!(peek("$ x"), None);
        assert_eq!(peek(". g"), None);
        assert_eq!(peek("@x"), None);
        assert_eq!(peek("+. x"), Some("+"));
    }
}
