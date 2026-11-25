//! Where an element comes from in the entry source, for "Copy for AI". Each
//! probe is a small property edit that is prepared and validated but never
//! published; the literals it would rewrite name the lines behind that field.
use super::*;
use std::collections::BTreeMap;

/// The fields worth probing: geometry and the common style and text values.
fn probe_fields(kind: &str) -> Vec<&'static str> {
    let mut fields: Vec<&'static str> = geometry::fields(kind)
        .unwrap_or(&[])
        .iter()
        .copied()
        .filter(|f| !matches!(*f, "points" | "commands"))
        .collect();
    fields.extend(["fill.paint", "stroke.paint", "stroke.width", "content"]);
    fields.retain(|f| supported(kind, f));
    fields
}

/// A nearby value of the same type: different enough to need a source change.
fn nudge(value: &Value) -> Option<Value> {
    Some(match value {
        Value::Number(n) => json!(n.as_f64()? + 1.0),
        Value::String(s) => json!(format!("{s}·")),
        Value::Array(rgb) if rgb.len() == 3 => {
            let red = rgb[0].as_u64()?;
            json!([if red == 255 { 254 } else { red + 1 }, rgb[1], rgb[2]])
        }
        Value::Null => json!([0, 0, 0]),
        _ => return None,
    })
}

impl Editor {
    pub fn locate(&mut self, drawing: &mut Drawing, snapshot: &str, key: &str) -> Value {
        let result = (|| -> Result<Value, String> {
            drawing.refresh();
            let render = drawing.check_snapshot(snapshot)?;
            let node = crate::find_node(&render.scene, key).ok_or("Unknown scene element.")?;
            let kind = node["kind"].as_str().unwrap_or("");
            let source_key = render.source_key(key)?;
            let source = &render.snapshot.sources()[render.snapshot.entry_path()];
            if self.engine.is_none() {
                self.engine = Some(new_engine().map_err(|e| e.to_string())?);
            }
            let engine = self.engine.as_ref().unwrap();
            // line -> fields whose edit rewrites a literal on that line
            let mut lines: BTreeMap<usize, Vec<&str>> = BTreeMap::new();
            for field in probe_fields(kind) {
                let Some(value) = node["props"].get(field).and_then(nudge) else {
                    continue;
                };
                let Ok(request) = decode_field(field, &value)
                    .and_then(|value| property_request(&source_key, kind, field, value))
                else {
                    continue;
                };
                let Ok(prepared) = engine.prepare(
                    &render.snapshot,
                    "program",
                    &request,
                    drawing.roots(),
                    EditLimits::default(),
                ) else {
                    continue;
                };
                for patch in prepared.patches() {
                    let (start, end) = (patch.span.start as usize, patch.span.end as usize);
                    // Only patches that point into the entry file can be shown.
                    if source.get(start..end) != Some(patch.before.as_str()) {
                        continue;
                    }
                    let line = source[..start].matches('\n').count() + 1;
                    let fields = lines.entry(line).or_default();
                    if !fields.contains(&field) {
                        fields.push(field);
                    }
                }
            }
            let text: Vec<&str> = source.lines().collect();
            let lines: Vec<Value> = lines
                .into_iter()
                .map(|(line, fields)| {
                    json!({"line": line, "text": text.get(line - 1).copied().unwrap_or(""), "fields": fields})
                })
                .collect();
            Ok(json!({"ok": true, "file": drawing.path(), "lines": lines}))
        })();
        result.unwrap_or_else(|reason| json!({"ok": false, "reason": reason}))
    }
}
