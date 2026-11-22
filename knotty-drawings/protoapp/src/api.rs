use super::*;
use knot_workbench::traversal::PreparedContext;
use serde_json::Value;

pub(super) fn dispatch(
    method: &str,
    path: &str,
    query: &str,
    body: &str,
    drawing: &mut Drawing,
    editor: &mut Editor,
    sparse: bool,
    prepared: impl FnOnce(&str, &str) -> Option<PreparedContext>,
) -> Value {
    match (method, path) {
        ("GET", "/api/tangle") => {
            drawing.refresh();
            let name = query_value(query, "definition");
            let graph = graph_view::response(
                drawing,
                editor,
                name.as_deref(),
                query_value(query, "embed").as_deref() == Some("1"),
            );
            match graph {
                Ok(response) => response,
                Err(reason) => json!({"ok":false,"reason":reason}),
            }
        }
        ("POST", "/api/tangle") => {
            let result = serde_json::from_str::<edit::GraphEdit>(body)
                .map_err(|e| e.to_string())
                .and_then(|edit| editor.save_graph(drawing, edit));
            match result {
                Ok(()) => json!({"ok":true}),
                Err(reason) => json!({"ok":false,"reason":reason}),
            }
        }
        ("GET", "/api/revision") => {
            drawing.refresh();
            json!({
                "revision": drawing.revision(),
                "error": drawing.error(),
            })
        }
        ("GET", "/api/scene") => {
            drawing.refresh();
            scene_response::payload(drawing, editor, false, None)
        }
        ("GET", "/api/editability") => {
            drawing.refresh();
            let key = query_value(query, "key").unwrap_or_default();
            let snapshot = query_value(query, "snapshot").unwrap_or_default();
            match drawing.check_snapshot(&snapshot) {
                Ok(render) => {
                    let fields = if render.source_key(&key).is_ok() {
                        find_node(&render.scene, &key)
                            .map(field_names)
                            .unwrap_or_default()
                    } else {
                        vec![]
                    };
                    editor.editability(
                        &snapshot,
                        &key,
                        find_node(&render.scene, &key)
                            .and_then(|n| n["kind"].as_str())
                            .unwrap_or(""),
                        &fields,
                    )
                }
                Err(reason) => {
                    json!({ "snapshot": snapshot, "key": key, "fields": {}, "reason": reason })
                }
            }
        }
        ("POST", "/api/paint" | "/api/preview") => {
            match serde_json::from_str::<PreviewRequest>(body) {
                Ok(preview) => {
                    let path = match &preview {
                        PreviewRequest::BeginGeometry { key, snapshot, .. } => {
                            prepared(snapshot, key)
                        }
                        _ => None,
                    };
                    editor.preview_response(drawing, preview, path, sparse)
                }
                Err(e) => json!({"ok":false,"reason":format!("bad preview request: {e}")}),
            }
        }
        ("POST", "/api/undo") => match serde_json::from_str::<serde_json::Value>(body) {
            Ok(value) => match value["snapshot"].as_str() {
                Some(snapshot) => editor.undo(drawing, snapshot),
                None => json!({"ok":false,"reason":"missing snapshot"}),
            },
            Err(e) => json!({"ok":false,"reason":e.to_string()}),
        },
        ("POST", "/api/create/circle" | "/api/create/circle/check") => {
            let check_only = path.ends_with("/check");
            match serde_json::from_str::<CreateCircleRequest>(body) {
                Ok(edit) => editor.create_circle(drawing, &edit, check_only),
                Err(e) => json!({"ok":false,"reason":format!("bad creation request: {e}")}),
            }
        }
        ("POST", "/api/create/shape" | "/api/create/shape/check") => {
            let check_only = path.ends_with("/check");
            match serde_json::from_str::<CreateShapeRequest>(body) {
                Ok(edit) => editor.create_shape(drawing, &edit, check_only),
                Err(e) => json!({"ok":false,"reason":format!("bad creation request: {e}")}),
            }
        }
        ("POST", "/api/whip") => match serde_json::from_str::<whips::WhipEdit>(body) {
            Ok(edit) => editor.apply_whip(drawing, &edit),
            Err(e) => json!({"ok":false,"reason":format!("bad whip request: {e}")}),
        },
        ("POST", "/api/edit") => match serde_json::from_str::<EditRequest>(body) {
            Ok(edit) => editor.apply(drawing, &edit),
            Err(e) => json!({ "ok": false, "reason": format!("bad edit request: {e}") }),
        },
        _ => json!({"ok":false,"reason":"Unknown drawing request"}),
    }
}
