//! Creation asks for one additional output node. The selected unravel decides
//! where it belongs; this module neither searches source syntax nor picks a site.
use super::*;

#[derive(serde::Deserialize, Debug)]
pub struct CreateCircleRequest {
    pub snapshot: String,
    pub parent: String,
    pub cx: f64,
    pub cy: f64,
    pub r: f64,
}
#[derive(serde::Deserialize, Debug)]
pub struct CreateShapeRequest {
    pub snapshot: String,
    pub parent: String,
    #[serde(flatten)]
    pub geometry: ShapeGeometry,
}

#[derive(serde::Deserialize, Debug)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ShapeGeometry {
    Circle {
        cx: f64,
        cy: f64,
        r: f64,
    },
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Ellipse {
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
    },
}

impl ShapeGeometry {
    fn request(&self) -> Result<String, String> {
        let (constructor, fields, positive): (&str, &[(&str, f64)], bool) = match *self {
            Self::Circle { cx, cy, r } => ("Circle", &[("cx", cx), ("cy", cy), ("r", r)], r > 0.0),
            Self::Rect {
                x,
                y,
                width,
                height,
            } => (
                "Rect",
                &[("x", x), ("y", y), ("width", width), ("height", height)],
                width > 0.0 && height > 0.0,
            ),
            Self::Ellipse { cx, cy, rx, ry } => (
                "Ellipse",
                &[("cx", cx), ("cy", cy), ("rx", rx), ("ry", ry)],
                rx > 0.0 && ry > 0.0,
            ),
        };
        if !positive || fields.iter().any(|(_, v)| !v.is_finite()) {
            return Err("Shape coordinates must be finite and dimensions positive.".into());
        }
        let fields = fields
            .iter()
            .map(|(name, value)| format!("{name} = Just ({})", FieldValue::Float(*value).source()))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!("Requests.ShapeNodeRequest Requests.anyCommon (Requests.Request{constructor} (let c = Requests.any{constructor} in {{ c | {fields} }}))"))
    }
}

impl Editor {
    pub fn create_circle(
        &mut self,
        drawing: &mut Drawing,
        edit: &CreateCircleRequest,
        check_only: bool,
    ) -> Value {
        self.create_shape(
            drawing,
            &CreateShapeRequest {
                snapshot: edit.snapshot.clone(),
                parent: edit.parent.clone(),
                geometry: ShapeGeometry::Circle {
                    cx: edit.cx,
                    cy: edit.cy,
                    r: edit.r,
                },
            },
            check_only,
        )
    }

    /// A capability probe uses the same preparation/validation as the commit,
    /// without publishing source or adding an undo entry.
    pub fn create_shape(
        &mut self,
        drawing: &mut Drawing,
        edit: &CreateShapeRequest,
        check_only: bool,
    ) -> Value {
        let result = (|| -> Result<String, String> {
            if self.active_preview.is_some() {
                return Err("Finish the current property edit first.".into());
            }
            if self.preview {
                return Err("Preview mode does not apply source edits.".into());
            }
            let shape = edit.geometry.request()?;
            drawing.refresh();
            let render = drawing.check_snapshot(&edit.snapshot)?;
            let key = render.source_key(&edit.parent)?;
            let parent =
                crate::find_node(&render.scene, &edit.parent).ok_or("Unknown destination.")?;
            if !matches!(parent["kind"].as_str(), Some("group" | "svg")) {
                return Err("The destination must be a group or SVG container.".into());
            }
            let count = parent["children"]
                .as_array()
                .ok_or("Missing destination children.")?
                .len();
            if key.len() > 32 || key.iter().any(|&i| i > 1000) || count > 1000 {
                return Err("Destination exceeds bounded edit traversal.".into());
            }
            let new_key = if edit.parent.is_empty() {
                count.to_string()
            } else {
                format!("{}.{count}", edit.parent)
            };
            // Local bindings keep parser depth bounded even for wide scenes.
            let mut bindings = format!("tail0 = Prepend ({shape}) ExactEnd\n");
            for i in 0..count {
                bindings.push_str(&format!(
                    "tail{} = Prepend Requests.AnyNode tail{i}\n",
                    i + 1
                ));
            }
            let frames = geometry::frames(render, &key)?;
            let mut container = container_request(parent, &format!("tail{count}"))?;
            for (depth, index) in key.iter().enumerate().rev() {
                container = container_request(
                    &frames[depth],
                    &format!("Requests.slot {index} (Requests.ContainerNodeRequest ({container}))"),
                )?;
            }
            let request = Request {
                expression: format!(
                    "let\n{}in {container}",
                    bindings
                        .lines()
                        .map(|l| format!("  {l}\n"))
                        .collect::<String>()
                ),
                type_name: "Requests.ContainerRequest".into(),
            };
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
                    "program",
                    &request,
                    drawing.roots(),
                    EditLimits::default(),
                )
                .map_err(|e| {
                    format!(
                        "This destination's unravel could not add this shape: {}",
                        edit_error(e)
                    )
                })?;
            if !check_only {
                if self
                    .undo
                    .last()
                    .is_some_and(|(r, _)| *r != drawing.revision())
                {
                    self.undo.clear();
                }
                drawing.save_prepared_profiled(prepared.snapshot(), &prepared)?;
                self.undo.push((drawing.revision(), before));
            }
            Ok(new_key)
        })();
        match result {
            Ok(key) => json!({"ok":true,"key":key,"parent":edit.parent,"snapshot":edit.snapshot}),
            Err(reason) => json!({"ok":false,"reason":reason}),
        }
    }
}

// Coordinates come from the displayed CTM; all contributing frames must stay fixed.
fn container_request(node: &Value, children: &str) -> Result<String, String> {
    let transform = decode_field("transform", &node["props"]["transform"])?.source();
    let mut fields = vec![
        format!("children = ({children})"),
        format!(
            "common = (let c = Requests.anyCommon in {{ c | transform = Just ({transform}) }})"
        ),
    ];
    if node["kind"] == "svg" {
        let viewport = ["width", "height", "viewBox"]
            .iter()
            .map(|name| {
                Ok(format!(
                    "{name} = Just ({})",
                    decode_field(name, &node["props"][name])?.source()
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        fields.push(format!(
            "kind = Requests.ViewportKind (let v = Requests.anyViewport in {{ v | {} }})",
            viewport.join(", ")
        ));
    }
    Ok(format!(
        "let c = Requests.anyContainer in {{ c | {} }}",
        fields.join(", ")
    ))
}
