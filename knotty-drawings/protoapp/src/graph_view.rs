use crate::{Drawing, Editor};
use knot_canonical::ast::CDecl;
use serde_json::{json, Value};

pub(super) fn response(
    drawing: &Drawing,
    editor: &Editor,
    name: Option<&str>,
    embedded: bool,
) -> Result<Value, String> {
    if let Some(error) = drawing.error() {
        return Err(error.into());
    }
    let render = drawing.last_good().ok_or("No checked drawing.")?;
    let snapshot = &render.snapshot;
    let program = &snapshot.graph.entry().program;
    let can_undo = editor.can_undo(drawing);
    if embedded {
        let names: Vec<_> = program
            .canonical
            .decls
            .iter()
            .filter_map(|decl| {
                let CDecl::Fn(f) = &decl.node else {
                    return None;
                };
                snapshot.editable_source(snapshot.entry_path(), decl.span)?;
                Some(f.name.as_str())
            })
            .collect();
        // External source edits may remove the selected definition.
        let selected = name.filter(|n| names.contains(n)).unwrap_or("program");
        let graph = knot_workbench::tangle::project(snapshot, selected)?;
        return Ok(json!({ "ok":true, "graph":graph, "definitions":names, "canUndo":can_undo }));
    }
    let default = if program.definition("cafe_table").is_some() {
        "cafe_table"
    } else {
        "program"
    };
    let selected = name.unwrap_or(default);
    let graph = knot_workbench::tangle::project(snapshot, selected)?;
    let (preview, error) = match drawing.definition_preview(render, selected) {
        Ok(svg) => (svg, None),
        Err(reason) => (None, Some(reason)),
    };
    Ok(
        json!({ "ok":true, "graph":graph, "preview":preview, "previewError":error, "canUndo":can_undo }),
    )
}
