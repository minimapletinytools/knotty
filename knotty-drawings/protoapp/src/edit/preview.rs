//! One live property transaction. Each proposal starts from the pre-editor source;
//! only the validated result is published, avoiding a visible intermediate undo.
use super::*;
use knot_workbench::project::ProjectSnapshot;
use std::time::Instant;

#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum PreviewRequest {
    Begin {
        session: String,
        snapshot: String,
        key: String,
        field: String,
    },
    BeginGeometry {
        session: String,
        snapshot: String,
        key: String,
    },
    Update {
        session: String,
        snapshot: String,
        value: Value,
    },
    Accept {
        session: String,
    },
    Cancel {
        session: String,
    },
}
pub(super) struct PropertyPreview {
    session: String,
    baseline: std::sync::Arc<ProjectSnapshot>,
    context: Option<knot_workbench::traversal::PreparedContext>,
    baseline_collection_work: u64,
    key: Vec<usize>,
    target: PreviewTarget,
    transform: Value,
    frames: Vec<Value>,
    kind: String,
    expected_snapshot: String,
    undo_depth: usize,
    applied: bool,
}
#[derive(Clone)]
enum PreviewTarget {
    Property(String),
    Geometry,
}

impl Editor {
    #[cfg(test)]
    pub fn preview(&mut self, drawing: &mut Drawing, request: PreviewRequest) -> Value {
        self.preview_response(drawing, request, None, false)
    }
    pub fn preview_response(
        &mut self,
        drawing: &mut Drawing,
        request: PreviewRequest,
        path: Option<knot_workbench::traversal::PreparedContext>,
        sparse: bool,
    ) -> Value {
        let finishing = matches!(
            &request,
            PreviewRequest::Accept { .. } | PreviewRequest::Cancel { .. }
        );
        drawing.refresh();
        let base = match &request {
            PreviewRequest::Update { snapshot, .. } if sparse => drawing
                .last_good()
                .filter(|r| r.snapshot.id() == snapshot)
                .map(crate::scene_response::Base::from),
            _ => None,
        };
        let result = match request {
            PreviewRequest::Begin {
                session,
                snapshot,
                key,
                field,
            } => self.preview_begin(
                drawing,
                session,
                snapshot,
                key,
                PreviewTarget::Property(field),
                None,
            ),
            PreviewRequest::BeginGeometry {
                session,
                snapshot,
                key,
            } => self.preview_begin(
                drawing,
                session,
                snapshot,
                key,
                PreviewTarget::Geometry,
                path,
            ),
            PreviewRequest::Update {
                session,
                snapshot,
                value,
            } => self.preview_update(drawing, &session, &snapshot, &value),
            PreviewRequest::Accept { session } => self.preview_finish(drawing, &session, true),
            PreviewRequest::Cancel { session } => self.preview_finish(drawing, &session, false),
        };
        match result {
            Ok(mut value) => {
                value["ok"] = json!(true);
                value["drawing"] =
                    crate::scene_response::payload(drawing, self, self.compact_previews, base);
                value
            }
            Err(reason) => {
                json!({"ok":false,"reason":reason,"closed":finishing && self.active_preview.is_none()})
            }
        }
    }
    fn preview_begin(
        &mut self,
        drawing: &Drawing,
        session: String,
        snapshot: String,
        key: String,
        target: PreviewTarget,
        path: Option<knot_workbench::traversal::PreparedContext>,
    ) -> Result<Value, String> {
        let started = Instant::now();
        if self.preview {
            return Err("Preview mode: source edits are disabled.".into());
        }
        if self.active_preview.is_some() {
            return Err("Another property editor is already open.".into());
        }
        if session.is_empty() || session.len() > 128 {
            return Err("Invalid picker session.".into());
        }
        let render = drawing.check_snapshot(&snapshot)?;
        let node = crate::find_node(&render.scene, &key).ok_or("Unknown scene element.")?;
        let kind = node["kind"].as_str().unwrap_or("").to_owned();
        match &target {
            PreviewTarget::Property(field)
                if node["props"].get(field).is_none() || !supported(&kind, field) =>
            {
                return Err("Unknown property.".into())
            }
            PreviewTarget::Geometry if geometry::fields(&kind).is_none() => {
                return Err("This selection has no canvas geometry editor.".into())
            }
            _ => {}
        }
        let transform = node["props"]["transform"].clone();
        let key = render.source_key(&key)?;
        let frames = if matches!(target, PreviewTarget::Geometry) {
            geometry::frames(render, &key)?
        } else {
            vec![]
        };
        // Keep the checked render exports in the editing contract so the final
        // candidate can be rendered directly after validation.
        let baseline = render.snapshot.clone();
        if !baseline.matches_disk() {
            return Err("Drawing changed while opening the picker.".into());
        }
        drawing.check_snapshot(&snapshot)?;
        if self
            .undo
            .last()
            .is_some_and(|(r, _)| *r != drawing.revision())
        {
            self.undo.clear();
        }
        let path_prepared = path.is_some();
        self.active_preview = Some(PropertyPreview {
            session: session.clone(),
            baseline,
            context: path,
            baseline_collection_work: 0,
            key,
            target,
            transform,
            frames,
            kind,
            expected_snapshot: snapshot,
            undo_depth: self.undo.len(),
            applied: false,
        });
        Ok(
            json!({"session":session,"timings":{"path_prepared":path_prepared,"begin_ms":started.elapsed().as_secs_f64()*1000.0}}),
        )
    }
    fn preview_update(
        &mut self,
        drawing: &mut Drawing,
        session: &str,
        snapshot: &str,
        value: &Value,
    ) -> Result<Value, String> {
        let started = Instant::now();
        let preview = self
            .active_preview
            .as_mut()
            .filter(|p| p.session == session)
            .ok_or("Property editor session expired.")?;
        if preview.expected_snapshot != snapshot {
            return Err("Stale property preview; wait for the current pick.".into());
        }
        let render = drawing.check_snapshot(snapshot)?;
        let current = render.snapshot.sources()[render.snapshot.entry_path()].clone();
        let (request, argument) = match &preview.target {
            PreviewTarget::Property(field) => (
                property_request(
                    &preview.key,
                    &preview.kind,
                    field,
                    decode_field(field, value)?,
                )?,
                None,
            ),
            PreviewTarget::Geometry => geometry::build_request(
                &preview.key,
                &preview.kind,
                value,
                &preview.transform,
                &preview.frames,
                self.reuse_path_queries,
            )?,
        };
        let phase = Instant::now();
        if self.engine.is_none() {
            self.engine = Some(crate::edit::new_engine().map_err(|e| e.to_string())?);
        }
        if preview.context.is_none() {
            let engine = self.engine.as_ref().unwrap();
            #[cfg(feature = "incremental")]
            let context = {
                // The native scene and SVG already own their decoded output.
                // Preserve the Knot program as the query's live root; optional
                // computations use the runtime collector's general cost policy.
                let mut limits = twine::Limits::default();
                let collected = render
                    .operations()
                    .collect_globals(
                        &[knot_core::GlobalName {
                            module: preview
                                .baseline
                                .graph
                                .entry()
                                .program
                                .canonical
                                .name
                                .clone(),
                            name: "program".into(),
                        }],
                        limits.clone(),
                        twine::CollectionBudget {
                            entries: 200_000,
                            bytes: 32 * 1024 * 1024,
                            ..Default::default()
                        },
                    )
                    .map_err(|e| format!("query baseline: {e:?}"))?;
                preview.baseline_collection_work = collected.work;
                limits.steps = limits.steps.saturating_sub(collected.work);
                match collected.session {
                    Some(baseline) => engine.context_reusing(&preview.baseline, &baseline, limits),
                    // A bounded retention miss leaves the checked query fresh.
                    None => engine.context(&preview.baseline),
                }
            };
            #[cfg(not(feature = "incremental"))]
            let context = engine.context(&preview.baseline);
            let context = context.map_err(edit_error)?;
            let context = if self.reuse_path_queries
                && matches!(preview.target, PreviewTarget::Geometry)
                && preview.kind == "path"
            {
                context.with_route_reuse()
            } else {
                context
            };
            let context = if std::env::var("KNOTTY_LIST_ROUTES").as_deref() == Ok("1") {
                context.with_list_routes()
            } else {
                context
            };
            preview.context = Some(context);
        }
        let baseline = preview.context.as_ref().unwrap().baseline_stats();
        let baseline_collection_work = preview.baseline_collection_work;
        let engine_init_ms = phase.elapsed().as_secs_f64() * 1000.0;
        // Equivalent to undoing the old pick and applying this one, but validate
        // first and publish only once. A failed proposal keeps the last good pick.
        let phase = Instant::now();
        let engine = self.engine.as_ref().unwrap();
        let context = preview.context.as_ref().unwrap();
        let prepared = match argument {
            Some(input) => engine.prepare_call_in(
                &preview.baseline,
                context,
                "program",
                &request,
                &input,
                drawing.roots(),
                EditLimits::default(),
            ),
            None => engine.prepare_in(
                &preview.baseline,
                context,
                "program",
                &request,
                drawing.roots(),
                EditLimits::default(),
            ),
        }
        .map_err(edit_error)?;
        let unravel_wall_ms = phase.elapsed().as_secs_f64() * 1000.0;
        let original = &preview.baseline.sources()[preview.baseline.entry_path()];
        let changed = prepared.candidate_source() != original;
        let save = if prepared.candidate_source() == current {
            Default::default()
        } else {
            drawing.render_prepared(snapshot, &prepared, self.stage_previews)?
        };
        self.undo.truncate(preview.undo_depth);
        if changed {
            self.undo.push((drawing.revision(), original.clone()));
        } else if let Some((revision, _)) = self.undo.last_mut() {
            *revision = drawing.revision();
        }
        let preview = self.active_preview.as_mut().unwrap();
        preview.applied = changed;
        preview.expected_snapshot = drawing.last_good().unwrap().snapshot.id().into();
        Ok(
            json!({"session":session,"timings":{"total_ms":started.elapsed().as_secs_f64()*1000.0,"engine_init_ms":engine_init_ms,"baseline":baseline,"baseline_collection_work":baseline_collection_work,"unravel_wall_ms":unravel_wall_ms,"unravel":prepared.timings(),"save":save,"steps":prepared.usage().steps,"heap_entries":prepared.usage().heap_entries}}),
        )
    }
    fn preview_finish(
        &mut self,
        drawing: &mut Drawing,
        session: &str,
        keep: bool,
    ) -> Result<Value, String> {
        if self
            .active_preview
            .as_ref()
            .is_none_or(|p| p.session != session)
        {
            return Err("Property editor session expired.".into());
        }
        let preview = self.active_preview.take().unwrap();
        // A stale session may close, but must never restore over external edits.
        if let Err(error) = drawing.check_snapshot(&preview.expected_snapshot) {
            drawing.discard_staged_preview();
            return Err(error);
        }
        if self.stage_previews {
            if let Err(error) =
                drawing.finish_staged_preview(&preview.expected_snapshot, keep && preview.applied)
            {
                self.active_preview = Some(preview);
                return Err(error);
            }
        } else if !keep && preview.applied {
            let original = &preview.baseline.sources()[preview.baseline.entry_path()];
            if let Err(error) = drawing.save_source(&preview.expected_snapshot, original) {
                self.active_preview = Some(preview); // allow retry after a failed save
                return Err(error);
            }
        }
        if !keep || !preview.applied {
            self.undo.truncate(preview.undo_depth);
            if let Some((revision, _)) = self.undo.last_mut() {
                *revision = drawing.revision();
            }
        }
        Ok(json!({"closed":true}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    const SOURCE:&str="module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nimport Types exposing (..)\nprogram = svg 100 100 [circle 20 30 10 |> set_fill tomato]\n";
    struct TestDrawing {
        dir: PathBuf,
        drawing: Drawing,
        editor: Editor,
    }
    impl TestDrawing {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "knot-paint-preview-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("Drawing.knot");
            std::fs::write(&path, SOURCE).unwrap();
            let drawing = Drawing::new(
                path,
                &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".."),
                None,
            )
            .unwrap();
            Self {
                dir,
                drawing,
                editor: Editor::new(false),
            }
        }
        fn snapshot(&self) -> String {
            self.drawing.last_good().unwrap().snapshot.id().into()
        }
        fn begin(&mut self) {
            let snapshot = self.snapshot();
            let r = self.editor.preview(
                &mut self.drawing,
                PreviewRequest::Begin {
                    session: "picker".into(),
                    snapshot,
                    key: "0".into(),
                    field: "fill.paint".into(),
                },
            );
            assert_eq!(r["ok"], true, "{r}");
        }
        fn update(&mut self, value: Value) -> Value {
            let snapshot = self.snapshot();
            self.editor.preview(
                &mut self.drawing,
                PreviewRequest::Update {
                    session: "picker".into(),
                    snapshot,
                    value,
                },
            )
        }
        fn source(&self) -> String {
            std::fs::read_to_string(self.drawing.path()).unwrap()
        }
        fn finish(&mut self, keep: bool) -> Value {
            self.editor.preview(
                &mut self.drawing,
                if keep {
                    PreviewRequest::Accept {
                        session: "picker".into(),
                    }
                } else {
                    PreviewRequest::Cancel {
                        session: "picker".into(),
                    }
                },
            )
        }
    }
    impl Drop for TestDrawing {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    #[test]
    fn previews_replace_one_undo_entry_and_cancel_preserves_prior_history() {
        let mut t = TestDrawing::new();
        let edit = EditRequest {
            snapshot: t.snapshot(),
            key: "0".into(),
            field: "cx".into(),
            value: json!(25),
        };
        assert_eq!(t.editor.apply(&mut t.drawing, &edit)["ok"], true);
        let before = t.source();
        t.begin();
        for value in [json!([0, 0, 0]), json!([53, 45, 44]), json!([1, 2, 3])] {
            let result = t.update(value);
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(t.editor.undo.len(), 2);
            #[cfg(feature = "incremental")]
            {
                assert!(
                    result["timings"]["baseline"]["shared_heap_pages"]
                        .as_u64()
                        .unwrap()
                        > 0,
                    "{result}"
                );
                assert!(
                    result["timings"]["unravel"]["runtime_forks"]
                        .as_u64()
                        .unwrap()
                        > 0,
                    "{result}"
                );
            }
            assert!(result["timings"]["unravel"]["total_ms"].as_f64().unwrap() > 0.0);
            assert_eq!(
                result["timings"]["save"]["render"]["reused_checked_candidate"],
                true
            );
            assert_eq!(result["timings"]["save"]["render"]["compile_ms"], 0.0);
        }
        let snapshot = t.snapshot();
        assert_eq!(t.editor.undo(&mut t.drawing, &snapshot)["ok"], false);
        assert_eq!(t.finish(false)["ok"], true);
        assert_eq!(t.source(), before);
        assert_eq!(t.editor.undo.len(), 1);
        let snapshot = t.snapshot();
        assert_eq!(t.editor.undo(&mut t.drawing, &snapshot)["ok"], true);
        assert_eq!(t.source(), SOURCE);
    }
    #[test]
    fn ok_keeps_the_applied_value_without_another_save_and_undo_returns_original() {
        let mut t = TestDrawing::new();
        t.begin();
        assert_eq!(t.update(json!([0, 0, 0]))["ok"], true);
        assert_eq!(t.update(json!([1, 2, 3]))["ok"], true);
        let source = t.source();
        let snapshot = t.snapshot();
        let revision = t.drawing.revision();
        assert_eq!(t.finish(true)["ok"], true);
        assert_eq!(t.source(), source);
        assert_eq!(t.snapshot(), snapshot);
        assert_eq!(t.drawing.revision(), revision);
        assert_eq!(t.editor.undo.len(), 1);
        assert_eq!(t.editor.undo(&mut t.drawing, &snapshot)["ok"], true);
        assert_eq!(t.source(), SOURCE);
    }
    #[test]
    fn failed_pick_keeps_last_preview_and_returning_to_original_removes_preview_history() {
        let mut t = TestDrawing::new();
        t.begin();
        assert_eq!(t.update(json!([0, 0, 0]))["ok"], true);
        let before = t.source();
        assert_eq!(t.update(Value::Null)["ok"], false);
        assert_eq!(t.source(), before);
        assert_eq!(t.update(json!([255, 99, 71]))["ok"], true);
        assert_eq!(t.source(), SOURCE);
        assert!(t.editor.undo.is_empty());
        let revision = t.drawing.revision();
        assert_eq!(t.finish(false)["ok"], true);
        assert_eq!(t.drawing.revision(), revision);
    }
    #[test]
    fn staged_edits_commit_once_or_restore_the_display_and_undo_history() {
        for keep in [false, true] {
            let mut t = TestDrawing::new();
            t.editor.stage_previews = true;
            let original_scene = t.drawing.last_good().unwrap().scene.clone();
            t.begin();
            for color in [json!([1, 2, 3]), json!([3, 4, 5])] {
                assert_eq!(t.update(color)["ok"], true);
                assert_eq!(t.source(), SOURCE);
            }
            assert_eq!(t.update(Value::Null)["ok"], false);
            let displayed = t.drawing.last_good().unwrap();
            let candidate = displayed.snapshot.sources()[t.drawing.path()].clone();
            assert_ne!(displayed.scene, original_scene);
            assert_eq!(t.finish(keep)["ok"], true);
            t.drawing.refresh();
            if keep {
                assert_eq!(t.source(), candidate);
                let snapshot = t.snapshot();
                assert_eq!(t.editor.undo(&mut t.drawing, &snapshot)["ok"], true);
            } else {
                assert!(t.editor.undo.is_empty());
            }
            assert_eq!(t.drawing.last_good().unwrap().scene, original_scene);
            assert_eq!(t.source(), SOURCE);
        }
    }

    #[test]
    fn staged_commit_cannot_overwrite_an_external_change() {
        let mut t = TestDrawing::new();
        t.editor.stage_previews = true;
        t.begin();
        assert_eq!(t.update(json!([1, 2, 3]))["ok"], true);
        let external = SOURCE.to_owned() + "\n-- external\n";
        std::fs::write(t.drawing.path(), &external).unwrap();
        assert_eq!(t.finish(true)["ok"], false);
        assert_eq!(t.source(), external);
    }

    #[test]
    fn stale_requests_and_external_edits_are_not_overwritten_by_cancel() {
        let mut t = TestDrawing::new();
        t.begin();
        let stale = t.snapshot();
        assert_eq!(t.update(json!([0, 0, 0]))["ok"], true);
        let result = t.editor.preview(
            &mut t.drawing,
            PreviewRequest::Update {
                session: "picker".into(),
                snapshot: stale,
                value: json!([1, 2, 3]),
            },
        );
        assert_eq!(result["ok"], false);
        let external = t.source() + "\n-- external edit\n";
        std::fs::write(t.drawing.path(), &external).unwrap();
        let result = t.finish(false);
        assert_eq!(result["ok"], false);
        assert_eq!(result["closed"], true);
        assert_eq!(t.source(), external);
        assert!(t.editor.active_preview.is_none());
    }
}
