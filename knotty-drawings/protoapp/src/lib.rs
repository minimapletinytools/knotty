mod drawing;
mod drawing_value;
mod edit;
#[cfg(not(target_os = "wasi"))]
mod geometry_worker;
mod render_profile;
mod scene_response;
mod scene_svg;
mod scene_value;
#[cfg(not(target_os = "wasi"))]
mod storybook;
mod whips;

use drawing::{Drawing, SvgRenderer};
use edit::{CreateCircleRequest, EditRequest, Editor, PreviewRequest};
use serde_json::json;
use std::path::{Path, PathBuf};
mod api;
#[cfg(not(target_os = "wasi"))]
mod server;
#[cfg(not(target_os = "wasi"))]
use server::json_response;
#[cfg(not(target_os = "wasi"))]
pub use server::start;
#[cfg(target_os = "wasi")]
mod browser;

fn query_value(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == name).then(|| v.replace("%2E", ".").replace("%2e", "."))
    })
}

/// Finds the scene node with the given key ("" is the root, "2.0" a grandchild).
fn find_node<'a>(scene: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    let mut node = scene;
    if key.is_empty() {
        return Some(node);
    }
    for part in key.split('.') {
        let index: usize = part.parse().ok()?;
        node = node["children"].get(index)?;
    }
    Some(node)
}

fn field_names(node: &serde_json::Value) -> Vec<String> {
    node["props"]
        .as_object()
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_node_follows_child_indices() {
        let scene = json!({ "key": "", "children": [
            { "key": "0", "children": [] },
            { "key": "1", "children": [ { "key": "1.0", "children": [] } ] },
        ]});
        assert_eq!(find_node(&scene, "").unwrap()["key"], "");
        assert_eq!(find_node(&scene, "1.0").unwrap()["key"], "1.0");
        assert!(find_node(&scene, "2").is_none());
        assert!(find_node(&scene, "x").is_none());
    }
}
