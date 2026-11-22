//! A bundled, checked source file drives the gallery independently of the open drawing.
use knot_canonical::ast::{CDecl, CExpr};
use knot_workbench::{project::ProjectSnapshot, tangle};
use serde_json::{json, Value};
use std::{path::Path, sync::OnceLock};

pub fn catalog() -> &'static Value {
    static CATALOG: OnceLock<Value> = OnceLock::new();
    CATALOG.get_or_init(|| build().unwrap_or_else(|reason| json!({ "error": reason })))
}

fn build() -> Result<Value, String> {
    let snapshot = ProjectSnapshot::load(
        Path::new("tangle-stories.knot"),
        &[],
        &Default::default(),
        Some(include_str!("../../examples/tangle-stories.knot").into()),
        &Default::default(),
    )?;
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
