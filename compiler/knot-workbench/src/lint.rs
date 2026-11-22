//! Source normalization for redundant groups around value names.
use knot_checker::program::{EvidenceSite, ResolvedReference, TypedProgram};
use knot_syntax::{ParseState, Span};

/// Keep the token span separate from the expression span. Reconnecting a
/// grouped name must leave its delimiters, comments and surrounding tokens intact.
pub(crate) fn grouped_name_span(text: &str, name: &str, start: u32) -> Option<Span> {
    grouped_token_span(text, start, |parser| {
        let token = parser.lower_ident().ok()?;
        (token.node == name).then_some(token.span)
    })
}

pub(crate) fn grouped_token_span(
    text: &str,
    start: u32,
    token: impl FnOnce(&mut ParseState<'_>) -> Option<Span>,
) -> Option<Span> {
    let mut state = ParseState::new(text);
    let mut groups = 0;
    while state.peek() == Some(b'(') {
        state.bump();
        state.skip_trivia().ok()?;
        groups += 1;
    }
    if groups == 0 {
        return None;
    }
    let span = token(&mut state)?;
    for _ in 0..groups {
        state.expect_byte(b')').ok()?;
    }
    state
        .is_eof()
        .then(|| Span::new(start + span.start, start + span.end))
}

pub fn canonical_source(source: &str, program: &TypedProgram) -> String {
    let mut edits = Vec::new();
    for (site, target) in &program.references {
        let EvidenceSite::Expr(id) = site else {
            continue;
        };
        let name = match target {
            ResolvedReference::Binding(id) => &program.bindings[id].name,
            ResolvedReference::Global(global) => &global.name,
            ResolvedReference::Method { name, .. } => name,
        };
        let span = program.nodes[id].span;
        let range = span.start as usize..span.end as usize;
        let group = &source[range.clone()];
        if grouped_name_span(group, name, span.start).is_none() {
            continue;
        }
        // Leave comments and multi-line layout in place.
        if group.contains(['\n', '\r']) || group.contains("--") || group.contains("{-") {
            continue;
        }
        // Inline layout blocks align their later lines to a column on this line.
        // A formatter that moves that column must also reindent the block.
        let tail = source[range.end..].lines().next().unwrap_or("");
        let mut offset = 0;
        if tail
            .split_inclusive(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|part| {
                let word = part.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
                let after_word = offset + word.len();
                offset += part.len();
                matches!(word, "let" | "of" | "do") && !tail[after_word..].trim().is_empty()
            })
        {
            continue;
        }
        edits.push((range, name));
    }
    edits.sort_by_key(|(range, _)| range.start);
    let mut result = String::with_capacity(source.len());
    let mut cursor = 0;
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '\'' || !c.is_ascii();
    for (range, name) in edits {
        result.push_str(&source[cursor..range.start]);
        if result.chars().next_back().is_some_and(word) {
            result.push(' ');
        }
        result.push_str(name);
        cursor = range.end;
        if source[cursor..].chars().next().is_some_and(word) {
            result.push(' ');
        }
    }
    result.push_str(&source[cursor..]);
    result
}
