//! Unicode text policy shared by primitives. Categories follow Data.Char's
//! category-based predicates; casing uses context-free full Unicode mappings.
use std::fmt::{self, Write};
use unicode_general_category::{get_general_category as category, GeneralCategory as G};

pub(crate) fn space(c: char) -> bool {
    matches!(c, '\t'..='\r') || category(c) == G::SpaceSeparator
}
pub(crate) fn predicate(name: &str, s: &str) -> bool {
    let mut chars = s.chars();
    let Some(c) = chars.next() else {
        return false;
    };
    if chars.next().is_some() {
        return false;
    }
    let g = category(c);
    let alpha = matches!(
        g,
        G::UppercaseLetter
            | G::LowercaseLetter
            | G::TitlecaseLetter
            | G::ModifierLetter
            | G::OtherLetter
    );
    match name {
        "primStringIsSpace" => space(c),
        "primStringIsAlpha" => alpha,
        "primStringIsAlphaNum" => {
            alpha || matches!(g, G::DecimalNumber | G::LetterNumber | G::OtherNumber)
        }
        "primStringIsUpper" => matches!(g, G::UppercaseLetter | G::TitlecaseLetter),
        "primStringIsLower" => g == G::LowercaseLetter,
        _ => unreachable!("known text predicate"),
    }
}
fn render(s: &str, out: &mut impl Write) -> fmt::Result {
    const CONTROL: [&str; 32] = [
        "NUL", "SOH", "STX", "ETX", "EOT", "ENQ", "ACK", "a", "b", "t", "n", "v", "f", "r", "SO",
        "SI", "DLE", "DC1", "DC2", "DC3", "DC4", "NAK", "SYN", "ETB", "CAN", "EM", "SUB", "ESC",
        "FS", "GS", "RS", "US",
    ];
    out.write_char('"')?;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\x00'..='\x1f' => {
                out.write_char('\\')?;
                out.write_str(CONTROL[c as usize])?;
                if c == '\x0e' && chars.peek() == Some(&'H') {
                    out.write_str("\\&")?;
                }
            }
            '\x7f' => out.write_str("\\DEL")?,
            _ if c <= '~' => out.write_char(c)?,
            _ => {
                write!(out, "\\{}", c as u32)?;
                if chars.peek().is_some_and(char::is_ascii_digit) {
                    out.write_str("\\&")?;
                }
            }
        }
    }
    out.write_char('"')
}
struct Count(usize);
impl Write for Count {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0 = self.0.saturating_add(s.len());
        Ok(())
    }
}
pub(crate) fn show_len(s: &str) -> usize {
    let mut count = Count(0);
    render(s, &mut count).unwrap();
    count.0
}
pub(crate) fn show(s: &str) -> String {
    let mut out = String::new();
    render(s, &mut out).unwrap();
    out
}
pub(crate) fn integer(s: &str) -> Option<i64> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}
/// Full-string ASCII decimal grammar, excluding whitespace and special names.
pub(crate) fn float(s: &str) -> Option<f64> {
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e.strip_prefix(['+', '-']).unwrap_or(e))),
        None => (unsigned, None),
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if exponent.is_some_and(|e| !digits(e)) {
        return None;
    }
    let valid = match mantissa.split_once('.') {
        Some((a, b)) => digits(a) && digits(b),
        None => digits(mantissa),
    };
    if valid {
        s.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rendering_matches_ghc_golden() {
        for line in include_str!("../tests/fixtures/string-show.tsv").lines() {
            let (codepoints, expected) = line.split_once('\t').unwrap();
            let input: String = codepoints
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| char::from_u32(s.parse().unwrap()).unwrap())
                .collect();
            assert_eq!(show(&input), expected, "{codepoints}");
            assert_eq!(show_len(&input), expected.len());
        }
    }
    #[test]
    fn rendering_escapes_unicode_and_disambiguates_escapes() {
        for (input, expected) in [
            ("héllo 🦒", "\"h\\233llo \\129426\""),
            ("\"\\\n\r\t", "\"\\\"\\\\\\n\\r\\t\""),
            (
                "\0\x01\x07\x08\x0b\x0c\x7f",
                "\"\\NUL\\SOH\\a\\b\\v\\f\\DEL\"",
            ),
            ("\x0eH", "\"\\SO\\&H\""),
            ("\u{85}2", "\"\\133\\&2\""),
            ("\u{200b}\u{2028}\u{e000}", "\"\\8203\\8232\\57344\""),
        ] {
            assert_eq!(show(input), expected);
            assert_eq!(show_len(input), expected.len());
        }
    }
    #[test]
    fn category_predicates_match_data_char_edge_cases() {
        assert_eq!(unicode_general_category::UNICODE_VERSION, (16, 0, 0));
        for (c, space, alpha, alnum, upper, lower) in [
            ('\t', true, false, false, false, false),
            ('\u{a0}', true, false, false, false, false),
            ('\u{85}', false, false, false, false, false),
            ('\u{2028}', false, false, false, false, false),
            ('\u{2029}', false, false, false, false, false),
            ('\u{345}', false, false, false, false, false),
            ('ǅ', false, true, true, true, false),
            ('Ⅰ', false, false, true, false, false),
            ('²', false, false, true, false, false),
            ('é', false, true, true, false, true),
            ('中', false, true, true, false, false),
            ('٣', false, false, true, false, false),
        ] {
            for (p, want) in [
                ("Space", space),
                ("Alpha", alpha),
                ("AlphaNum", alnum),
                ("Upper", upper),
                ("Lower", lower),
            ] {
                assert_eq!(
                    predicate(&format!("primStringIs{p}"), &c.to_string()),
                    want,
                    "{c:?} {p}"
                );
            }
        }
        for p in ["Space", "Alpha", "AlphaNum", "Upper", "Lower"] {
            for s in ["", "ab", "Á"] {
                assert!(!predicate(&format!("primStringIs{p}"), s));
            }
        }
    }
    #[test]
    fn numeric_parsing_is_decimal_whole_string_and_range_checked() {
        assert_eq!(integer("-9223372036854775808"), Some(i64::MIN));
        assert_eq!(integer("9223372036854775807"), Some(i64::MAX));
        for s in [
            "9223372036854775808",
            "-9223372036854775809",
            "1.0",
            "1e2",
            "0xff",
            "１２",
        ] {
            assert_eq!(integer(s), None, "{s}");
        }
        assert_eq!(integer("+0042"), Some(42));
        for s in [
            "", " ", " 1", "1\n", "+", "--1", "1_000", "NaN", "Infinity", "inf",
        ] {
            assert_eq!(integer(s), None, "{s}");
            assert_eq!(float(s), None, "{s}");
        }
        for s in [".5", "5.", "1e", "1e+", "1e2e3", "1.2.3", "0x1p2"] {
            assert_eq!(float(s), None, "{s}");
        }
        assert_eq!(float("+01.25E-1"), Some(0.125));
        assert_eq!(float("42"), Some(42.0));
        assert!(float("1e999999999999999999999").unwrap().is_infinite());
        assert_eq!(
            float("-1e-999999999999999999999").unwrap().to_bits(),
            (-0.0f64).to_bits()
        );
    }
}
