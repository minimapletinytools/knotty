//! A bundled, checked source file drives the gallery independently of the open drawing.
use knot_canonical::ast::CDecl;
use knot_workbench::{project::ProjectSnapshot, tangle};
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path, sync::OnceLock};

#[cfg(not(target_os = "wasi"))]
pub fn catalog() -> &'static Value {
    static CATALOG: OnceLock<Value> = OnceLock::new();
    CATALOG.get_or_init(|| build().unwrap_or_else(|reason| json!({ "error": reason })))
}

const FILE: &str = "tangle-stories.knot";
const STORIES: &str = include_str!("../../examples/tangle-stories.knot");

fn load(source: String) -> Result<ProjectSnapshot, String> {
    ProjectSnapshot::load(
        Path::new(FILE),
        &[],
        &Default::default(),
        Some(source),
        &Default::default(),
    )
}

/// Byte span of each story's declaration in the bundled module.
fn spans() -> &'static Result<HashMap<String, (usize, usize)>, String> {
    static SPANS: OnceLock<Result<HashMap<String, (usize, usize)>, String>> = OnceLock::new();
    SPANS.get_or_init(|| {
        let snapshot = load(STORIES.into())?;
        Ok(snapshot
            .graph
            .entry()
            .program
            .canonical
            .decls
            .iter()
            .filter_map(|decl| match &decl.node {
                CDecl::Fn(f) => Some((
                    f.name.clone(),
                    (decl.span.start as usize, decl.span.end as usize),
                )),
                _ => None,
            })
            .collect())
    })
}

/// Replace one story's declaration with edited text, check the whole module and
/// project the story again. Nothing is saved: a reload restores the original.
/// Errors inside the edited text carry a line and column relative to it.
pub fn edit(definition: &str, source: &str) -> Value {
    let result = (|| -> Result<Value, (String, Option<(usize, usize)>)> {
        let spans = spans().as_ref().map_err(|e| (e.clone(), None))?;
        let &(start, end) = spans
            .get(definition)
            .ok_or_else(|| (format!("Unknown story {definition}."), None))?;
        let module = format!("{}{source}{}", &STORIES[..start], &STORIES[end..]);
        let first = STORIES[..start].matches('\n').count() + 1;
        let lines = source.matches('\n').count() + 1;
        // The rest of the module checked before, so an error outside the edit
        // (say, a parser that gives up later) still belongs to it.
        let located = |reason: String| {
            let at = position(&reason).map(|(line, column)| {
                if line < first {
                    (1, 1)
                } else if line >= first + lines {
                    (lines, source.lines().last().map_or(1, |l| l.len() + 1))
                } else {
                    (line + 1 - first, column)
                }
            });
            (reason, at)
        };
        let snapshot = load(module).map_err(located)?;
        let graph = tangle::project(&snapshot, definition).map_err(located)?;
        Ok(json!({ "ok": true, "graph": graph }))
    })();
    result.unwrap_or_else(|(reason, at)| match at {
        Some((line, column)) => json!({ "ok": false, "reason": reason, "line": line, "column": column }),
        None => json!({ "ok": false, "reason": reason }),
    })
}

/// Request body for [`edit`].
pub fn edit_request(body: &str) -> Value {
    #[derive(serde::Deserialize)]
    struct Edit {
        definition: String,
        source: String,
    }
    match serde_json::from_str::<Edit>(body) {
        Ok(e) => edit(&e.definition, &e.source),
        Err(e) => json!({ "ok": false, "reason": format!("bad story request: {e}") }),
    }
}

/// "tangle-stories.knot:10:3: …" or "tangle-stories.knot: 10:3: …"
fn position(reason: &str) -> Option<(usize, usize)> {
    let rest = &reason[reason.find(FILE)? + FILE.len()..];
    let mut parts = rest.strip_prefix(':')?.trim_start().splitn(3, ':');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[cfg(not(target_os = "wasi"))]
fn build() -> Result<Value, String> {
    use knot_canonical::ast::CExpr;
    let snapshot = load(STORIES.into())?;
    let mut stories = Vec::new();
    for decl in &snapshot.graph.entry().program.canonical.decls {
        if let CDecl::Fn(f) = &decl.node {
            if let Some(annotation) = f.annotations.iter().find(|a| a.key == "story") {
                let CExpr::StringLit(group) = &annotation.value.node else {
                    return Err(format!("Expected a story group on {}", f.name));
                };
                stories
                    .push(json!({ "group": group, "graph": tangle::project(&snapshot, &f.name)? }));
            }
        }
    }
    Ok(json!({ "stories": stories }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn story(name: &str) -> String {
        catalog()["stories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["graph"]["definition"] == name)
            .unwrap()["graph"]["source"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn edited_stories_project_again_and_errors_point_into_the_edit() {
        let original = story("operators");
        let unchanged = edit("operators", &original);
        assert_eq!(unchanged["graph"]["source"], original.as_str());
        let changed = edit("operators", &original.replace("b * 3", "b * 7"));
        assert_eq!(changed["ok"], true, "{changed}");
        assert!(changed["graph"]["source"].as_str().unwrap().contains("b * 7"));
        assert_ne!(changed["graph"], unchanged["graph"]);
        let broken = edit("operators", &original.replace("b * 3", "b * \"x\""));
        assert_eq!(broken["ok"], false);
        let line = broken["line"].as_u64().unwrap() as usize;
        assert!(original.lines().nth(line - 1).unwrap().contains("b * 3"), "{broken}");
        let parse = edit("operators", &original.replace("b * 3", "b * ("));
        assert_eq!(parse["ok"], false);
        assert!(parse["line"].is_u64(), "{parse}");
        assert_eq!(edit("missing", "x = 1")["ok"], false);
    }
}
