//! Read-only source checks for the canvas widget. Each actual drag is validated
//! again; a finite probe cannot guarantee every possible value on a source route.
use super::*;
use std::collections::BTreeMap;

#[derive(serde::Deserialize)]
pub struct GeometryCheck {
    pub snapshot: String,
    pub key: String,
    pub probes: BTreeMap<String, Value>,
}

pub(super) fn fields(kind: &str) -> Option<&'static [&'static str]> {
    match kind {
        "rect" => Some(&["x", "y", "width", "height"]),
        "circle" => Some(&["cx", "cy", "r"]),
        "ellipse" => Some(&["cx", "cy", "rx", "ry"]),
        "line" => Some(&["x1", "y1", "x2", "y2"]),
        "polyline" | "polygon" => Some(&["points"]),
        "path" => Some(&["commands"]),
        "text" => Some(&["x", "y", "font.size"]),
        _ => None,
    }
}

pub(super) fn build_request(
    key: &[usize],
    kind: &str,
    value: &Value,
    transform: &Value,
    frames: &[Value],
    separate_commands: bool,
) -> Result<(Request, Option<Request>), String> {
    let fields = fields(kind).ok_or("This selection has no canvas geometry editor.")?;
    let values = value.as_object().ok_or("Expected geometry properties.")?;
    if values.len() != fields.len() || fields.iter().any(|f| !values.contains_key(*f)) {
        return Err(
            "A geometry update must contain every geometry field and no other properties.".into(),
        );
    }
    let mut decoded = BTreeMap::new();
    for (field, value) in values {
        if ["width", "height", "r", "rx", "ry", "font.size"].contains(&field.as_str())
            && value.as_f64().is_some_and(|v| v < 0.0)
        {
            return Err("Shape dimensions cannot be negative.".into());
        }
        decoded.insert(field.clone(), decode_field(field, value)?);
    }
    // Pointer coordinates were mapped through this transform. Keep it fixed,
    // including when it shares a source binding with a geometry argument.
    decoded.insert("transform".into(), decode_field("transform", transform)?);
    // The path request has one changing argument. Keep all coordinates together
    // so the request constructor can be checked and linked across pointer moves.
    let argument = if separate_commands && kind == "path" {
        decoded
            .insert(
                "commands".into(),
                FieldValue::Structured("pathInput".into()),
            )
            .map(|commands| Request {
                expression: commands.source(),
                type_name: "List Types.PathCommand".into(),
            })
    } else {
        None
    };
    let mut request = properties::request_in_frame(key, kind, &decoded, frames)?;
    if argument.is_some() {
        request.expression = format!("\\pathInput -> {}", request.expression);
    }
    Ok((request, argument))
}

// Keep every ancestor that contributes to getScreenCTM fixed as well. The
// displayed scene may wrap a group-valued source in a synthetic SVG root.
pub(super) fn frames(render: &crate::drawing::Render, key: &[usize]) -> Result<Vec<Value>, String> {
    let mut node =
        crate::find_node(&render.scene, &render.source_root_key).ok_or("Missing source root.")?;
    let mut frames = vec![];
    for index in key {
        frames.push(json!({"kind":node["kind"],"props":node["props"]}));
        node = node["children"]
            .get(*index)
            .ok_or("Unknown scene element.")?;
    }
    Ok(frames)
}

pub struct GeometryTask {
    pub snapshot: std::sync::Arc<knot_workbench::project::ProjectSnapshot>,
    roots: Vec<std::path::PathBuf>,
    check: GeometryCheck,
    key: Vec<usize>,
    kind: String,
    transform: Value,
    frames: Vec<Value>,
    pub(crate) prepare_path: bool,
}
impl GeometryTask {
    #[cfg(not(target_os = "wasi"))]
    pub fn selection_key(&self) -> &str {
        &self.check.key
    }

    #[cfg(any(test, target_os = "wasi"))]
    pub fn evaluate(
        &self,
        engine: &SourceEditor,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Value {
        match engine.context(&self.snapshot) {
            Ok(context) => self.evaluate_in(engine, context, cancellation, |_| {}),
            Err(error) => self.error(&error.to_string()),
        }
    }
    pub fn evaluate_in(
        &self,
        engine: &SourceEditor,
        context: knot_workbench::traversal::PreparedContext,
        cancellation: std::sync::Arc<std::sync::atomic::AtomicBool>,
        publish: impl FnOnce(knot_workbench::traversal::PreparedContext),
    ) -> Value {
        use std::sync::atomic::Ordering;
        let started = std::time::Instant::now();
        let mut capabilities = Map::new();
        let mut evaluated: Vec<(&Value, Value)> = vec![];
        let context = if self.prepare_path {
            context.with_route_reuse()
        } else {
            context
        };
        let mut checked_path = false;
        for (name, values) in &self.check.probes {
            if cancellation.load(Ordering::Relaxed) {
                return self.cancelled();
            }
            let result =
                if let Some((_, result)) = evaluated.iter().find(|(value, _)| *value == values) {
                    result.clone()
                } else {
                    let mut limits = EditLimits::default();
                    limits.runtime.cancellation = cancellation.clone();
                    let result = build_request(
                        &self.key,
                        &self.kind,
                        values,
                        &self.transform,
                        &self.frames,
                        self.prepare_path,
                    )
                    .and_then(|(request, argument)| {
                        let result = match argument {
                            Some(input) => engine.prepare_call_in(
                                &self.snapshot,
                                &context,
                                "program",
                                &request,
                                &input,
                                &self.roots,
                                limits,
                            ),
                            None => engine.prepare_in(
                                &self.snapshot,
                                &context,
                                "program",
                                &request,
                                &self.roots,
                                limits,
                            ),
                        };
                        result.map(|_| ()).map_err(edit_error)
                    });
                    let result = match result {
                        Ok(()) => {
                            checked_path |= self.prepare_path;
                            json!({"editable":true})
                        }
                        Err(reason) => json!({"editable":false,"reason":reason}),
                    };
                    evaluated.push((values, result.clone()));
                    result
                };
            capabilities.insert(name.clone(), result);
        }
        if cancellation.load(Ordering::Relaxed) {
            return self.cancelled();
        }
        if !self.snapshot.matches_disk() {
            return self.error("Drawing changed; reload before editing.");
        }
        let path_ready_ms = checked_path.then(|| {
            publish(context);
            started.elapsed().as_secs_f64() * 1000.0
        });
        json!({"ok":true,"snapshot":self.check.snapshot,"key":self.check.key,"capabilities":capabilities,
            "timings":{"compute_ms":started.elapsed().as_secs_f64()*1000.0,"probes":evaluated.len(),"path_ready_ms":path_ready_ms}})
    }
    pub fn cancelled(&self) -> Value {
        json!({"ok":false,"cancelled":true,"snapshot":self.check.snapshot,"key":self.check.key})
    }
    pub fn error(&self, reason: &str) -> Value {
        json!({"ok":false,"snapshot":self.check.snapshot,"key":self.check.key,"reason":reason})
    }
}
impl Editor {
    pub fn geometry_task(
        &self,
        drawing: &Drawing,
        check: GeometryCheck,
    ) -> Result<GeometryTask, String> {
        if self.preview {
            return Err("Preview mode does not apply source edits.".into());
        }
        if self.active_preview.is_some() {
            return Err("Finish the current edit first.".into());
        }
        let render = drawing.check_snapshot(&check.snapshot)?;
        let node = crate::find_node(&render.scene, &check.key).ok_or("Unknown scene element.")?;
        let kind = node["kind"].as_str().unwrap_or("");
        fields(kind).ok_or("This selection has no canvas geometry editor.")?;
        if check.probes.len() != 4
            || ["moveX", "moveY", "sizeX", "sizeY"]
                .iter()
                .any(|k| !check.probes.contains_key(*k))
        {
            return Err("Expected four bounded geometry checks.".into());
        }
        let key = render.source_key(&check.key)?;
        let frames = frames(render, &key)?;
        Ok(GeometryTask {
            prepare_path: self.reuse_path_queries
                && kind == "path"
                && std::env::var("KNOTTY_PREPARE_PATH").as_deref() == Ok("1")
                && std::env::var("KNOTTY_LIST_ROUTES").as_deref() != Ok("1"),
            snapshot: render.snapshot.clone(),
            roots: drawing.roots().to_vec(),
            check,
            key,
            kind: kind.to_owned(),
            transform: node["props"]["transform"].clone(),
            frames,
        })
    }
    #[cfg(test)]
    pub fn geometry_check(&mut self, drawing: &mut Drawing, check: GeometryCheck) -> Value {
        drawing.refresh();
        match self.geometry_task(drawing, check) {
            Ok(task) => {
                if self.engine.is_none() {
                    self.engine = Some(crate::edit::new_engine().unwrap());
                }
                task.evaluate(self.engine.as_ref().unwrap(), Default::default())
            }
            Err(reason) => json!({"ok":false,"reason":reason}),
        }
    }
}
