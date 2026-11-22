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
impl Editor {
    /// A capability probe uses the same preparation/validation as the commit,
    /// without publishing source or adding an undo entry.
    pub fn create_circle(
        &mut self,
        drawing: &mut Drawing,
        edit: &CreateCircleRequest,
        check_only: bool,
    ) -> Value {
        let result = (|| -> Result<String, String> {
            if self.active_preview.is_some() {
                return Err("Finish the current property edit first.".into());
            }
            if self.preview {
                return Err("Preview mode does not apply source edits.".into());
            }
            if ![edit.cx, edit.cy, edit.r].iter().all(|v| v.is_finite()) || edit.r <= 0.0 {
                return Err("Circle coordinates must be finite and its radius positive.".into());
            }
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
            let float = |n| FieldValue::Float(n).source();
            let circle = format!("Requests.ShapeNodeRequest Requests.anyCommon (Requests.RequestCircle (let c = Requests.anyCircle in {{ c | cx = Just ({}), cy = Just ({}), r = Just ({}) }}))", float(edit.cx),float(edit.cy),float(edit.r));
            // Local bindings keep parser depth bounded even for wide scenes.
            let mut bindings = format!("tail0 = Prepend ({circle}) ExactEnd\n");
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
                        "This destination's unravel could not add a circle: {}",
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
