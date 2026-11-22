//! Full scenes and revision-scoped subtree replacements.
use super::{drawing, field_names, Drawing, Editor};
use serde_json::{json, Value};

pub(super) struct Base {
    snapshot: String,
    source_root: String,
    scene: std::sync::Arc<Value>,
}
impl From<&drawing::Render> for Base {
    fn from(render: &drawing::Render) -> Self {
        Self {
            snapshot: render.snapshot.id().into(),
            source_root: render.source_root_key.clone(),
            scene: render.scene.clone(),
        }
    }
}

pub(super) fn payload(
    drawing: &Drawing,
    editor: &Editor,
    compact: bool,
    base: Option<Base>,
) -> Value {
    let render = drawing.last_good();
    let mut payload = json!({"revision":drawing.revision(),"file":drawing.path().display().to_string(),
        "source":drawing.display_source(),
        "error":drawing.error(),"snapshot":render.map(|r|r.snapshot.id()),
        "sourceRootKey":render.map(|r|&r.source_root_key)});
    let Some(render) = render else {
        payload["scene"] = Value::Null;
        payload["svg"] = Value::Null;
        return payload;
    };
    payload["whips"] = json!(&render.whips);
    payload["whipsEditable"] = json!(!editor.preview_only() && drawing.error().is_none());
    let mut entries = Vec::new();
    let mut table = Table::new();
    let mut decorate =
        |node: &mut Value| annotate(node, render, editor, compact, &mut table, &mut entries);
    let patch = base.and_then(|base| {
        if drawing.svg_renderer() == drawing::SvgRenderer::Knot
            || base.source_root != render.source_root_key
            || drawing.error().is_some()
        {
            return None;
        }
        let parts = match difference(&base.scene, &render.scene) {
            None => Vec::new(),
            Some(Difference::Children(parts)) => encode(parts, &mut decorate)?,
            Some(Difference::Replace(_)) => return None,
        };
        let mut patch = json!({"base":base.snapshot});
        patch["children"] = Value::Array(parts);
        Some(patch)
    });
    if let Some(patch) = patch {
        payload["scenePatch"] = patch;
    } else {
        let mut scene = render.scene.as_ref().clone();
        decorate(&mut scene);
        payload["scene"] = scene;
        payload["svg"] = json!(&render.svg);
    }
    if compact {
        payload["editabilitySets"] = entries.into();
    }
    payload
}

// JSON numeric equality merges signed zeros. Preserve their SVG spelling and JS value.
fn identical(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) if x.is_f64() && y.is_f64() => {
            x.as_f64().map(f64::to_bits) == y.as_f64().map(f64::to_bits)
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| identical(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|((k, x), (l, y))| k == l && identical(x, y))
        }
        _ => a == b,
    }
}

enum Difference<'a> {
    Replace(&'a Value),
    Children(Vec<(usize, Difference<'a>)>),
}

fn difference<'a>(before: &Value, after: &'a Value) -> Option<Difference<'a>> {
    use Difference::*;
    let (Some(old), Some(new)) = (before.as_object(), after.as_object()) else {
        return Some(Replace(after));
    };
    if old.len() != new.len()
        || !old
            .iter()
            .all(|(k, v)| k == "children" || new.get(k).is_some_and(|w| identical(v, w)))
    {
        return Some(Replace(after));
    }
    let (Some(a), Some(b)) = (before["children"].as_array(), after["children"].as_array()) else {
        return Some(Replace(after));
    };
    if a.len() != b.len() {
        return Some(Replace(after));
    }
    let changes: Vec<_> = a
        .iter()
        .zip(b)
        .enumerate()
        .filter_map(|(i, (old, new))| difference(old, new).map(|change| (i, change)))
        .collect();
    if changes.is_empty() {
        None
    } else if changes.len() == b.len()
        && changes
            .iter()
            .all(|(_, change)| matches!(change, Replace(_)))
    {
        // No child can be retained. Send the parent once, including at the root.
        Some(Replace(after))
    } else {
        Some(Children(changes))
    }
}

fn encode(
    parts: Vec<(usize, Difference<'_>)>,
    decorate: &mut impl FnMut(&mut Value),
) -> Option<Vec<Value>> {
    parts
        .into_iter()
        .map(|(index, change)| {
            let mut part = json!({"index":index});
            match change {
                Difference::Replace(node) => {
                    part["svg"] = Value::String(super::scene_svg::render_at_depth(node, 1).ok()?);
                    let mut replacement = node.clone();
                    decorate(&mut replacement);
                    part["node"] = replacement;
                }
                Difference::Children(children) => {
                    part["children"] = Value::Array(encode(children, decorate)?)
                }
            }
            Some(part)
        })
        .collect()
}

type Table = std::collections::BTreeMap<(String, Vec<String>), usize>;
fn annotate(
    node: &mut serde_json::Value,
    render: &drawing::Render,
    editor: &Editor,
    compact: bool,
    table: &mut Table,
    entries: &mut Vec<serde_json::Value>,
) {
    let key = node["key"].as_str().unwrap_or("");
    let fields = if render.source_key(key).is_ok() {
        field_names(node)
    } else {
        vec![]
    };
    let kind = node["kind"].as_str().unwrap_or("");
    if compact {
        // Capability estimates depend only on kind and exposed field names in this editor.
        let signature = (kind.to_owned(), fields);
        let index = *table.entry(signature).or_insert_with_key(|(kind, fields)| {
            entries
                .push(editor.editability(render.snapshot.id(), key, kind, fields)["fields"].take());
            entries.len() - 1
        });
        node["editabilityIndex"] = json!(index);
    } else {
        node["editability"] =
            editor.editability(render.snapshot.id(), key, kind, &fields)["fields"].take();
    }
    if let Some(children) = node["children"].as_array_mut() {
        for child in children {
            annotate(child, render, editor, compact, table, entries);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_compare_nested_fields_exactly_and_require_a_compatible_root() {
        let leaf = json!({"key":"0.0", "v":[0.0], "children":[]});
        let group = json!({"key":"0","children":[leaf, leaf]});
        let original = json!({"props":{"width":10}, "children":[group, group]});
        let mut changed = original.clone();
        changed["children"][0]["children"][0]["v"][0] = json!(-0.0);
        let Some(Difference::Children(root)) = difference(&original, &changed) else {
            panic!("expected nested patch");
        };
        let [(0, Difference::Children(parts))] = &root[..] else {
            panic!("expected a change inside group 0");
        };
        assert!(matches!(&parts[..], [(0, Difference::Replace(_))]));
        assert!(difference(&original, &original).is_none());
        changed["children"][0]["children"] = json!([]);
        let Some(Difference::Children(parts)) = difference(&original, &changed) else {
            panic!("expected group replacement");
        };
        assert!(matches!(&parts[..], [(0, Difference::Replace(_))]));
        changed["children"][1]["children"] = json!([]);
        assert!(matches!(
            difference(&original, &changed),
            Some(Difference::Replace(_))
        ));
        changed = original.clone();
        changed["props"]["width"] = json!(11);
        assert!(matches!(
            difference(&original, &changed),
            Some(Difference::Replace(_))
        ));
        changed = original.clone();
        changed["children"].as_array_mut().unwrap().pop();
        assert!(matches!(
            difference(&original, &changed),
            Some(Difference::Replace(_))
        ));
    }
}
