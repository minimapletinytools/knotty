//! Property editing. Every edit is a pullback: a request on one element of the
//! output (by key) that the unravel engine turns into a validated source edit.
//! This is the app's single integration point for pullbacks and undo.
mod creation;
pub use creation::{CreateCircleRequest, CreateShapeRequest};
mod graph;
pub use graph::GraphEdit;
mod geometry;
mod preview;
mod properties;
mod locate;
mod source;
pub use source::SourceText;
use crate::drawing::Drawing;
#[cfg(not(target_os = "wasi"))]
pub use geometry::GeometryCheck;
#[cfg(not(target_os = "wasi"))]
pub use geometry::GeometryTask;
use knot_workbench::{
    traversal::{ConstructorRule, Request, SourceEditor},
    unravel::{EditError, EditLimits},
};
pub use preview::PreviewRequest;
use properties::{property_request, supported};
use serde_json::{json, Map, Value};

/// A request to set one property of one element: `field` is a dotted path such
/// as "cx" or "fill.paint"; `value` uses the scene JSON encoding of that field.
#[derive(serde::Deserialize, Debug)]
pub struct EditRequest {
    pub snapshot: String,
    pub key: String,
    pub field: String,
    pub value: Value,
}

pub struct Editor {
    /// Pretends some fields are editable so the UI's styles can be compared.
    preview: bool,
    reuse_path_queries: bool,
    stage_previews: bool,
    compact_previews: bool,
    engine: Option<SourceEditor>,
    undo: Vec<(u64, String)>,
    /// Revision published by the latest source-pane save, while it is still current.
    text_revision: Option<u64>,
    active_preview: Option<preview::PropertyPreview>,
}

const ENGINE_MISSING: &str = "This shape or field has no source pullback yet.";

/// Fields `--preview-editability` pretends are editable.
const PREVIEW_EDITABLE: &[&str] = &[
    "x",
    "y",
    "width",
    "height",
    "rx",
    "ry",
    "cx",
    "cy",
    "r",
    "x1",
    "y1",
    "x2",
    "y2",
    "content",
    "fill.paint",
    "stroke.paint",
    "stroke.width",
];

/// Drawing policy and materialization instances are Knot declarations. This host
/// registration attaches the custom additions rule before the standard cons rule.
pub fn new_engine() -> Result<SourceEditor, EditError> {
    Ok(SourceEditor::new()?.with_constructor_rules(vec![
        ConstructorRule {
            constructor: knot_core::prelude(":"),
            expression: "DrawingUnravel.unravelShapeAdditions".into(),
        },
        ConstructorRule {
            constructor: knot_core::prelude(":"),
            expression: "Prelude.unravelListCons".into(),
        },
        ConstructorRule {
            constructor: knot_core::GlobalName {
                module: vec!["Types".into()],
                name: "ShapeNode".into(),
            },
            expression: "DrawingUnravel.unravelShapeNode".into(),
        },
        ConstructorRule {
            constructor: knot_core::GlobalName {
                module: vec!["Types".into()],
                name: "Circle".into(),
            },
            expression: "DrawingUnravel.unravelCircleShape".into(),
        },
        ConstructorRule {
            constructor: knot_core::GlobalName {
                module: vec!["Types".into()],
                name: "Rect".into(),
            },
            expression: "DrawingUnravel.unravelRectShape".into(),
        },
        ConstructorRule {
            constructor: knot_core::GlobalName {
                module: vec!["Types".into()],
                name: "Ellipse".into(),
            },
            expression: "DrawingUnravel.unravelEllipseShape".into(),
        },
    ]))
}

impl Editor {
    pub fn new(preview: bool) -> Self {
        Editor {
            preview,
            compact_previews: std::env::var("KNOTTY_COMPACT_PREVIEWS").as_deref() == Ok("1"),
            stage_previews: std::env::var("KNOTTY_STAGE_PREVIEWS").as_deref() == Ok("1"),
            reuse_path_queries: !cfg!(feature = "incremental")
                && std::env::var("KNOTTY_PATH_QUERY_REUSE").as_deref() != Ok("0"),
            engine: None,
            undo: vec![],
            text_revision: None,
            active_preview: None,
        }
    }

    /// Which fields of the element at `key` can be changed, with a reason for
    /// each one that can't.
    pub fn editability(&self, snapshot: &str, key: &str, kind: &str, fields: &[String]) -> Value {
        let mut result = Map::new();
        for field in fields {
            let entry = if self.preview && PREVIEW_EDITABLE.contains(&field.as_str()) {
                json!({ "editable": true })
            } else if self.preview {
                json!({ "editable": false, "reason": "Preview: pretending this can't be unravelled." })
            } else if supported(kind, field) {
                // Shape-level capability estimate: preparation still checks
                // the actual source route, selected rules and pinned inputs.
                json!({ "editable": true })
            } else {
                json!({ "editable": false, "reason": ENGINE_MISSING })
            };
            result.insert(field.clone(), entry);
        }
        json!({ "snapshot": snapshot, "key": key, "fields": result })
    }

    pub fn preview_only(&self) -> bool {
        self.preview
    }

    pub fn apply(&mut self, drawing: &mut Drawing, edit: &EditRequest) -> Value {
        let result = self.apply_request(drawing, &edit.snapshot, |render| {
            let key = render.source_key(&edit.key)?;
            let node = crate::find_node(&render.scene, &edit.key).ok_or("unknown scene element")?;
            if node["props"].get(&edit.field).is_none() {
                return Err("unknown field".into());
            }
            let value = decode_field(&edit.field, &edit.value)?;
            let request = property_request(
                &key,
                node["kind"].as_str().unwrap_or(""),
                &edit.field,
                value,
            )?;
            Ok(("program".into(), request))
        });
        match result {
            Ok(()) => json!({"ok":true}),
            Err(reason) => json!({"ok":false,"key":edit.key,"field":edit.field,"reason":reason}),
        }
    }

    pub fn apply_whip(&mut self, drawing: &mut Drawing, edit: &crate::whips::WhipEdit) -> Value {
        let result = self.apply_request(drawing, &edit.snapshot, |render| {
            let whip = render
                .whips
                .get(edit.index)
                .ok_or("Unknown drawing control.")?;
            whip.owned_by(edit.key.as_ref())?;
            Ok((whip.binding.clone(), whip.request(edit.value)?))
        });
        match result {
            Ok(()) => json!({"ok":true}),
            Err(reason) => json!({"ok":false,"reason":reason}),
        }
    }

    fn apply_request(
        &mut self,
        drawing: &mut Drawing,
        revision: &str,
        resolve: impl FnOnce(&crate::drawing::Render) -> Result<(String, Request), String>,
    ) -> Result<(), String> {
        if self.active_preview.is_some() {
            return Err("Finish the property editor with OK or Cancel first.".into());
        }
        drawing.refresh();
        let render = drawing.check_snapshot(revision)?;
        if self.preview {
            return Err("Preview mode: edits are not applied.".into());
        }
        let (head, request) = resolve(render)?;
        if self.engine.is_none() {
            self.engine = Some(new_engine().map_err(|e| e.to_string())?);
        }
        let before = render.snapshot.sources()[render.snapshot.entry_path()].clone();
        let prepared = self
            .engine
            .as_ref()
            .unwrap()
            .prepare(
                &render.snapshot,
                &head,
                &request,
                drawing.roots(),
                EditLimits::default(),
            )
            .map_err(edit_error)?;
        let previous_revision = drawing.revision();
        drawing.save_prepared_profiled(prepared.snapshot(), &prepared)?;
        self.record_undo(previous_revision, drawing.revision(), before);
        Ok(())
    }

    fn record_undo(&mut self, previous: u64, current: u64, before: String) {
        if self.undo.last().is_some_and(|(rev, _)| *rev != previous) {
            self.undo.clear();
        }
        self.undo.push((current, before));
    }

    pub fn undo(&mut self, drawing: &mut Drawing, snapshot: &str) -> Value {
        if self.active_preview.is_some() {
            return json!({"ok":false,"reason":"Finish the property editor with OK or Cancel first."});
        }
        drawing.refresh();
        let result = (|| -> Result<(), String> {
            drawing.check_snapshot(snapshot)?;
            let (revision, source) = self.undo.last().ok_or("Nothing to undo.")?;
            if *revision != drawing.revision() {
                return Err("Drawing changed outside this edit history.".into());
            }
            drawing.save_source(snapshot, source)?;
            self.undo.pop();
            if let Some((revision, _)) = self.undo.last_mut() {
                *revision = drawing.revision();
            }
            Ok(())
        })();
        match result {
            Ok(()) => json!({"ok":true}),
            Err(reason) => json!({"ok":false,"reason":reason}),
        }
    }
}

fn edit_error(error: EditError) -> String {
    match error {
        EditError::RuleFailed => "The existing constructor or setter cannot produce this value. Fixed defaults need an explicit setter in the source.".into(),
        EditError::ValidationFailed => "This change cannot satisfy the request while preserving the source's fixed values and shared inputs. A fixed default may need an explicit setter.".into(),
        EditError::Unsupported(reason) => reason,
        other => other.to_string(),
    }
}

/// Typed app boundary. Display enum spellings are mapped explicitly; strings
/// are values, never interpolated as executable Knot source.
#[derive(Debug, PartialEq)]
pub enum FieldValue {
    Float(f64),
    Bool(bool),
    String(String),
    OptionalString(Option<String>),
    Constructor(&'static str),
    Paint(Option<[u8; 3]>),
    Structured(String),
}
pub fn decode_field(field: &str, value: &Value) -> Result<FieldValue, String> {
    let invalid = || format!("invalid value for {field}");
    match field {
        "points" | "commands" | "viewBox" | "transform" | "stroke.dashes" => {
            properties::decode_structured(field, value)
        }
        "id" => {
            if value.is_null() {
                Ok(FieldValue::OptionalString(None))
            } else {
                value
                    .as_str()
                    .map(|s| FieldValue::OptionalString(Some(s.into())))
                    .ok_or_else(invalid)
            }
        }
        "content" | "font.family" => value
            .as_str()
            .map(|s| FieldValue::String(s.into()))
            .ok_or_else(invalid),
        "visible" | "closed" => value.as_bool().map(FieldValue::Bool).ok_or_else(invalid),
        "fill.paint" | "stroke.paint" => {
            if value.is_null() {
                return Ok(FieldValue::Paint(None));
            }
            let channels = value
                .as_array()
                .filter(|a| a.len() == 3)
                .ok_or_else(invalid)?;
            let mut rgb = [0; 3];
            for (out, v) in rgb.iter_mut().zip(channels) {
                *out = v
                    .as_u64()
                    .and_then(|v| u8::try_from(v).ok())
                    .ok_or_else(invalid)?;
            }
            Ok(FieldValue::Paint(Some(rgb)))
        }
        "fill.rule" | "stroke.cap" | "stroke.join" | "font.anchor" | "font.baseline" => {
            let name = match (field, value.as_str()) {
                ("fill.rule", Some("nonzero")) => "NonZero",
                ("fill.rule", Some("evenodd")) => "EvenOdd",
                ("stroke.cap", Some("butt")) => "Butt",
                ("stroke.cap", Some("round")) => "RoundCap",
                ("stroke.cap", Some("square")) => "Square",
                ("stroke.join", Some("miter")) => "Miter",
                ("stroke.join", Some("round")) => "RoundJoin",
                ("stroke.join", Some("bevel")) => "Bevel",
                ("font.anchor", Some("start")) => "AnchorStart",
                ("font.anchor", Some("middle")) => "AnchorMiddle",
                ("font.anchor", Some("end")) => "AnchorEnd",
                ("font.baseline", Some("alphabetic")) => "BaselineAlphabetic",
                ("font.baseline", Some("middle")) => "BaselineMiddle",
                ("font.baseline", Some("central")) => "BaselineCentral",
                ("font.baseline", Some("hanging")) => "BaselineHanging",
                _ => return Err(invalid()),
            };
            Ok(FieldValue::Constructor(name))
        }
        "x" | "y" | "width" | "height" | "rx" | "ry" | "cx" | "cy" | "r" | "x1" | "y1" | "x2"
        | "y2" | "opacity" | "fill.opacity" | "stroke.width" | "stroke.opacity"
        | "stroke.miterLimit" | "stroke.dashOffset" | "font.size" => value
            .as_f64()
            .filter(|v| v.is_finite())
            .map(FieldValue::Float)
            .ok_or_else(invalid),
        _ => Err(format!("unsupported field {field}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_shapes_stay_locked() {
        let fields = vec!["cx".to_string(), "fill.paint".to_string()];
        let result = Editor::new(false).editability("test", "1", "unknown", &fields);
        assert_eq!(result["fields"]["cx"]["editable"], false);
        assert_eq!(result["fields"]["fill.paint"]["editable"], false);
    }

    #[test]
    fn preview_marks_geometry_editable() {
        let fields = vec!["cx".to_string(), "fill.rule".to_string()];
        let result = Editor::new(true).editability("test", "1", "unknown", &fields);
        assert_eq!(result["fields"]["cx"]["editable"], true);
        assert_eq!(result["fields"]["fill.rule"]["editable"], false);
    }
}

#[cfg(test)]
mod value_tests {
    use super::*;
    #[test]
    fn decode_field_distinguishes_empty_strings_optional_ids_and_enum_domains() {
        assert_eq!(
            decode_field("content", &json!("")),
            Ok(FieldValue::String("".into()))
        );
        assert!(decode_field("content", &Value::Null).is_err());
        assert_eq!(
            decode_field("id", &Value::Null),
            Ok(FieldValue::OptionalString(None))
        );
        assert_eq!(
            decode_field("stroke.cap", &json!("round")),
            Ok(FieldValue::Constructor("RoundCap"))
        );
        assert_eq!(
            decode_field("stroke.join", &json!("round")),
            Ok(FieldValue::Constructor("RoundJoin"))
        );
        assert!(decode_field("fill.paint", &json!([256, 0, 0])).is_err());
        assert!(decode_field("fill.paint", &json!([1.5, 0, 0])).is_err());
        assert_eq!(
            decode_field("fill.paint", &json!([200, 10, 10])),
            Ok(FieldValue::Paint(Some([200, 10, 10])))
        );
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    #[test]
    fn demo_edit_save_rerender_undo_and_stale_requests() {
        let library = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .unwrap();
        let dir = std::env::temp_dir().join(format!("knot-app-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = include_str!("../tests/fixtures/demo.knot").to_owned();
        let path = dir.join("Demo.knot");
        std::fs::write(&path, &source).unwrap();
        let mut drawing = Drawing::new(path.clone(), &library, None).unwrap();
        let mut editor = Editor::new(false);
        let first = drawing.last_good().unwrap().snapshot.id().to_string();
        let edit = EditRequest {
            snapshot: first.clone(),
            key: "1".into(),
            field: "cx".into(),
            value: json!(150),
        };
        let result = editor.apply(&mut drawing, &edit);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(
            crate::find_node(&drawing.last_good().unwrap().scene, "1").unwrap()["props"]["cx"],
            json!(150.0)
        );
        let after_position = std::fs::read_to_string(&path).unwrap();
        assert!(after_position.contains("circle (150.0) 130 70"));
        assert_eq!(editor.apply(&mut drawing, &edit)["ok"], false);
        let edit = EditRequest {
            snapshot: drawing.last_good().unwrap().snapshot.id().into(),
            key: "1".into(),
            field: "fill.paint".into(),
            value: json!([200, 10, 10]),
        };
        let result = editor.apply(&mut drawing, &edit);
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(
            crate::find_node(&drawing.last_good().unwrap().scene, "1").unwrap()["props"]
                ["fill.paint"],
            json!([200, 10, 10])
        );
        let id = drawing.last_good().unwrap().snapshot.id().to_string();
        assert_eq!(editor.undo(&mut drawing, &id)["ok"], true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after_position);
        let id = drawing.last_good().unwrap().snapshot.id().to_string();
        assert_eq!(editor.undo(&mut drawing, &id)["ok"], true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        let mut external = source.clone();
        external.push_str("\n-- another editor\n");
        std::fs::write(&path, external).unwrap();
        assert_eq!(editor.undo(&mut drawing, &id)["ok"], false);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn mapped_circle_edit_reaches_the_selected_input() {
        let library = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .unwrap();
        let dir = std::env::temp_dir().join(format!("knot-app-map-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        let source="module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nprogram = svg 100 100 (map (\\x -> circle x 20 5 |> set_fill tomato) [10, 30])\n";
        std::fs::write(&path, source).unwrap();
        let mut drawing = Drawing::new(path.clone(), &library, None).unwrap();
        let mut editor = Editor::new(false);
        let edit = EditRequest {
            snapshot: drawing.last_good().unwrap().snapshot.id().into(),
            key: "1".into(),
            field: "cx".into(),
            value: json!(35),
        };
        let result = editor.apply(&mut drawing, &edit);
        assert_eq!(result["ok"], true, "{result}");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("[10, (35.0)]"));
        assert_eq!(
            crate::find_node(&drawing.last_good().unwrap().scene, "0").unwrap()["props"]["cx"],
            json!(10.0)
        );
        assert_eq!(
            crate::find_node(&drawing.last_good().unwrap().scene, "1").unwrap()["props"]["cx"],
            json!(35.0)
        );
        let id = drawing.last_good().unwrap().snapshot.id().to_string();
        assert_eq!(editor.undo(&mut drawing, &id)["ok"], true);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn repeated_paint_edits_survive_source_materialization_and_undo() {
        let library = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .unwrap();
        let dir =
            std::env::temp_dir().join(format!("knot-app-repeat-paint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        let source="module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nimport Types exposing (..)\nprogram = svg 100 100 [circle 20 30 10 |> set_fill tomato |> set_stroke dark_red]\n";
        std::fs::write(&path, source).unwrap();
        let mut drawing = Drawing::new(path.clone(), &library, None).unwrap();
        let mut editor = Editor::new(false);
        let mut before = vec![];
        for field in ["stroke.paint", "fill.paint"] {
            for value in [json!([0, 0, 0]), json!([53, 45, 44]), json!([53, 45, 200])] {
                before.push(std::fs::read_to_string(&path).unwrap());
                let edit = EditRequest {
                    snapshot: drawing.last_good().unwrap().snapshot.id().into(),
                    key: "0".into(),
                    field: field.into(),
                    value: value.clone(),
                };
                let result = editor.apply(&mut drawing, &edit);
                assert_eq!(result["ok"], true, "{field} -> {value}: {result}");
                assert!(!std::fs::read_to_string(&path).unwrap().contains("(("));
                assert_eq!(
                    crate::find_node(&drawing.last_good().unwrap().scene, "0").unwrap()["props"]
                        [field],
                    value
                );
            }
        }
        // Reload the saved constructor expressions in a fresh app instance too.
        let mut reloaded = Drawing::new(path.clone(), &library, None).unwrap();
        let edit = EditRequest {
            snapshot: reloaded.last_good().unwrap().snapshot.id().into(),
            key: "0".into(),
            field: "stroke.paint".into(),
            value: json!([1, 2, 3]),
        };
        let mut fresh_editor = Editor::new(false);
        assert_eq!(fresh_editor.apply(&mut reloaded, &edit)["ok"], true);
        let id = reloaded.last_good().unwrap().snapshot.id().to_string();
        assert_eq!(fresh_editor.undo(&mut reloaded, &id)["ok"], true);
        for expected in before.into_iter().rev() {
            let id = drawing.last_good().unwrap().snapshot.id().to_string();
            let result = editor.undo(&mut drawing, &id);
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod coverage_tests;
