//! Loads a drawing file, evaluates its `program :: Container`, and exports the
//! scene tree through Knot-side `Scene` or a direct typed native adapter.
use crate::render_profile::{self, Mode as ProfileMode, Profile, Work};
use knot_checker::host::HostDeclarations;
use knot_core::Literal;
use knot_workbench::operation::{CallError, OperationSession, PreparedOperations};
use knot_workbench::{libraries::Libraries, modules::EntryExtension, project::ProjectSnapshot};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use twine::{Limits, Outcome, ValueView};

/// One successful evaluation of the drawing.
pub struct Render {
    pub whips: Vec<crate::whips::Slider>,
    pub scene: Arc<serde_json::Value>,
    pub svg: String,
    pub snapshot: Arc<ProjectSnapshot>,
    pub source_root_key: String,
    pub timings: RenderTimings,
    exports: Exports,
    /// Owns the previous revision for bounded pure-graph reuse.
    session: OperationSession,
    #[cfg(feature = "incremental")]
    cache_age: usize,
    #[cfg(feature = "scene-observations")]
    decoded: crate::scene_value::DecodeCache,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RenderTimings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layers: Option<render_profile::Layers>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<Profile>,
    pub reused_checked_candidate: bool,
    pub compile_ms: f64,
    pub link_ms: f64,
    pub svg_ms: f64,
    pub svg_renderer: &'static str,
    pub scene_ms: f64,
    #[cfg(feature = "scene-observations")]
    pub scene_cache: crate::scene_value::DecodeStats,
    pub reuse_ms: f64,
    pub reused_globals: usize,
    pub transferred_thunks: usize,
    pub transferred_values: usize,
    pub transferred_environments: usize,
    pub cache_context_ms: f64,
    pub cache_maintenance_ms: f64,
    pub cache_maintenance_steps: u64,
    pub shared_heap_pages: usize,
    pub collected_runtime: bool,
    pub runtime_steps: u64,
    pub runtime_thunks: u64,
    pub cache_evictions: u64,
    pub cache_admission_rejections: u64,
    pub cache_policy_work: u64,
    pub runtime_entries: usize,
    pub retained_runtime_entries: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_diagnostics: Option<CacheDiagnostics>,
}
/// Before-collection evidence pressure and optional retention at this render's
/// collection boundary. These counters do not add runtime instrumentation.
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct CacheDiagnostics {
    pub starting_evidence: EvidenceSnapshot,
    pub svg_evidence: EvidenceSnapshot,
    pub retained_evidence: Option<EvidenceSnapshot>,
    pub evidence_records: usize,
    pub evidence_reads: usize,
    pub evidence_bytes: usize,
    pub evidence_unrecorded: usize,
    pub evidence_summaries: usize,
    pub evidence_summary_hits: u64,
    pub evidence_summary_index_bytes: usize,
    pub evidence_region_bytes: usize,
    pub evidence_region_normalizations: u64,
    pub call_hits: u64,
    pub call_key_skips: u64,
    pub call_validity_misses: u64,
    pub call_scope_misses: u64,
    pub retention_storage_queries: u64,
    pub retention_unknown_storage: u64,
    pub retention_storage_steps: u64,
    pub retention_graph_description_hits: u64,
    pub retention_graph_description_misses: u64,
    pub retention_graph_description_evictions: u64,
    pub retention_graph_description_bytes: usize,
    pub retention_candidates: u64,
    pub retention_admitted: u64,
    pub retention_index_rejections: u64,
}
#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct EvidenceSnapshot {
    pub records: usize,
    pub reads: usize,
    pub bytes: usize,
    pub unrecorded: usize,
}
#[cfg(feature = "incremental")]
impl EvidenceSnapshot {
    fn read(session: &OperationSession) -> Self {
        let stats = session.evidence_stats();
        Self {
            records: stats.certificates,
            reads: stats.reads,
            bytes: stats.bytes,
            unrecorded: stats.unrecorded,
        }
    }
}
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SaveTimings {
    pub total_ms: f64,
    pub render: RenderTimings,
    pub publish_ms: f64,
}

impl Render {
    #[cfg(feature = "incremental")]
    pub fn operations(&self) -> &OperationSession {
        &self.session
    }

    pub fn source_key(&self, displayed: &str) -> Result<Vec<usize>, String> {
        let key = if self.source_root_key.is_empty() {
            displayed
        } else if displayed == self.source_root_key {
            ""
        } else {
            displayed
                .strip_prefix(&(self.source_root_key.clone() + "."))
                .ok_or("synthetic display root is not editable")?
        };
        if key.is_empty() {
            return Ok(vec![]);
        }
        key.split('.')
            .map(|p| p.parse::<usize>().map_err(|_| "invalid scene key".into()))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SvgRenderer {
    Knot,
    #[cfg(not(target_os = "wasi"))]
    Scene,
    #[default]
    Direct,
}

pub struct Drawing {
    profile_mode: ProfileMode,
    svg_renderer: SvgRenderer,
    path: PathBuf,
    roots: Vec<PathBuf>,
    libraries: knot_core::Program,
    observed: BTreeMap<PathBuf, Option<String>>,
    revision: u64,
    last_good: Option<Render>,
    preview_base: Option<Render>,
    error: Option<String>,
}

impl Drawing {
    /// `library` is the knotty-drawings directory (KnottyDrawings, Types, Scene).
    #[cfg(test)]
    pub fn new(path: PathBuf, library: &Path, cache: Option<&Path>) -> Result<Self, String> {
        Self::with_renderer(path, library, cache, SvgRenderer::default())
    }
    pub fn with_renderer(
        path: PathBuf,
        library: &Path,
        cache: Option<&Path>,
        svg_renderer: SvgRenderer,
    ) -> Result<Self, String> {
        let path = knot_workbench::modules::module_path(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let library = knot_workbench::modules::module_path(library)
            .map_err(|e| format!("{}: {e}", library.display()))?;
        let mut roots = vec![path.parent().unwrap().to_path_buf()];
        if roots[0] != library {
            roots.push(library);
        }
        let libraries = Libraries::bundled(cache)?;
        let mut drawing = Drawing {
            profile_mode: ProfileMode::from_env()?,
            svg_renderer,
            path,
            roots,
            libraries: libraries.core,
            observed: BTreeMap::new(),
            revision: 0,
            last_good: None,
            preview_base: None,
            error: None,
        };
        drawing.refresh();
        Ok(drawing)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Follow staged previews as well as disk edits, including invalid source.
    pub fn display_source(&self) -> Option<&str> {
        if self.error.is_some() {
            return self.observed.get(&self.path)?.as_deref();
        }
        let snapshot = &self.last_good.as_ref()?.snapshot;
        snapshot
            .sources()
            .get(snapshot.entry_path())
            .map(String::as_str)
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Render before publishing, then check every captured source again and
    /// atomically replace the entry. A failed render/save preserves the document.
    pub fn save_source(&mut self, snapshot: &str, source: &str) -> Result<(), String> {
        self.save_source_profiled(snapshot, source).map(|_| ())
    }

    pub fn save_source_profiled(
        &mut self,
        snapshot: &str,
        source: &str,
    ) -> Result<SaveTimings, String> {
        let started = Instant::now();
        self.check_snapshot(snapshot)?;
        let render = self.evaluate(source)?;
        self.publish_render(snapshot, source, render, started)
    }

    /// Render the exact candidate validated by the source editor. Preview picks
    /// may share an older baseline; the displayed snapshot still gates saving.
    pub fn save_prepared_profiled(
        &mut self,
        snapshot: &str,
        edit: &knot_workbench::traversal::SourceEdit,
    ) -> Result<SaveTimings, String> {
        self.render_prepared(snapshot, edit, false)
    }

    pub fn render_prepared(
        &mut self,
        snapshot: &str,
        edit: &knot_workbench::traversal::SourceEdit,
        staged: bool,
    ) -> Result<SaveTimings, String> {
        let started = Instant::now();
        let current = self.check_snapshot(snapshot)?;
        let candidate = edit.candidate_snapshot();
        if candidate.entry_path() != self.path
            || candidate.entry_extension() != current.snapshot.entry_extension()
            || candidate.sources().len() != current.snapshot.sources().len()
            || candidate.sources().iter().any(|(path, source)| {
                path != &self.path && current.snapshot.sources().get(path) != Some(source)
            })
        {
            return Err(
                "Validated candidate belongs to a different drawing or dependencies.".into(),
            );
        }
        let mut names = current.exports.names();
        if self.profile_mode == ProfileMode::ForceData {
            names.push("program");
        }
        let render = self.evaluate_checked(
            candidate.clone(),
            current.exports.clone(),
            0.0,
            Some(edit.candidate_operations(&names)?),
        )?;
        if staged {
            self.check_snapshot(snapshot)?;
            let publish = Instant::now();
            let timings = render.timings.clone();
            let previous = self.last_good.replace(render);
            if self.preview_base.is_none() {
                self.preview_base = previous;
            } else {
                drop(previous);
            }
            self.revision += 1;
            Ok(SaveTimings {
                render: timings,
                publish_ms: publish.elapsed().as_secs_f64() * 1000.0,
                total_ms: started.elapsed().as_secs_f64() * 1000.0,
            })
        } else {
            self.publish_render(snapshot, edit.candidate_source(), render, started)
        }
    }

    fn publish_render(
        &mut self,
        snapshot: &str,
        source: &str,
        render: Render,
        started: Instant,
    ) -> Result<SaveTimings, String> {
        let timings = render.timings.clone();
        let publish = Instant::now();
        self.write_source(snapshot, source)?;
        self.revision += 1;
        self.observed = render
            .snapshot
            .sources()
            .iter()
            .map(|(p, s)| (p.clone(), Some(s.clone())))
            .collect();
        self.last_good = Some(render);
        self.error = None;
        Ok(SaveTimings {
            total_ms: started.elapsed().as_secs_f64() * 1000.0,
            render: timings,
            publish_ms: publish.elapsed().as_secs_f64() * 1000.0,
        })
    }

    fn write_source(&self, snapshot: &str, source: &str) -> Result<(), String> {
        use std::io::Write;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let temporary = self.path.with_extension(format!(
            "knot-save-{}-{}",
            knot_workbench::project::process_id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            #[cfg(not(target_os = "wasi"))]
            file.set_permissions(
                std::fs::metadata(&self.path)
                    .map_err(|e| e.to_string())?
                    .permissions(),
            )
            .map_err(|e| e.to_string())?;
            file.write_all(source.as_bytes())
                .map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            self.check_snapshot(snapshot)?;
            std::fs::rename(&temporary, &self.path).map_err(|e| e.to_string())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }

    pub fn finish_staged_preview(&mut self, snapshot: &str, keep: bool) -> Result<(), String> {
        let render = self.check_snapshot(snapshot)?;
        if self.preview_base.is_none() {
            return Ok(());
        }
        if keep {
            let source = &render.snapshot.sources()[&self.path];
            self.write_source(snapshot, source)?;
            self.observed
                .insert(self.path.clone(), Some(source.clone()));
            self.preview_base = None;
        } else {
            self.discard_staged_preview();
        }
        Ok(())
    }

    pub fn discard_staged_preview(&mut self) {
        if let Some(base) = self.preview_base.take() {
            self.last_good = Some(base);
            self.revision += 1;
        }
    }

    /// Increases whenever the displayed drawing or its backing source changes.
    /// Evaluate an optional definition preview in the same checked source revision.
    pub fn definition_preview(
        &self,
        render: &Render,
        name: &str,
    ) -> Result<Option<String>, String> {
        if name == "program" {
            return Ok(Some(render.svg.clone()));
        }
        let binding = format!("{name}_preview");
        let program = &render.snapshot.graph.entry().program;
        if program.definition(&binding).is_none() {
            return Ok(None);
        }
        let operations = render.snapshot.prepare_operations(
            self.libraries.clone(),
            &HostDeclarations::default(),
            &[&binding],
        )?;
        let mut session = operations.start(Limits::default()).map_err(describe)?;
        let value = session.binding(&binding).map_err(describe)?;
        let root = session.force(&value).map_err(describe)?;
        let scene = crate::drawing_value::decode(session.read_values(), root, None)?;
        crate::scene_svg::render(&scene).map(Some)
    }

    pub fn svg_renderer(&self) -> SvgRenderer {
        self.svg_renderer
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn last_good(&self) -> Option<&Render> {
        self.last_good.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Only the currently displayed successful snapshot can accept an edit.
    pub fn check_snapshot(&self, id: &str) -> Result<&Render, String> {
        let render = self
            .last_good
            .as_ref()
            .ok_or("no successful drawing snapshot")?;
        if self.error.is_some()
            || render.snapshot.id() != id
            || !self
                .preview_base
                .as_ref()
                .unwrap_or(render)
                .snapshot
                .matches_disk()
        {
            return Err("Drawing changed; reload before editing.".into());
        }
        Ok(render)
    }

    /// Reads the entry and all known dependencies. Retry failed loads as well,
    /// so creating a previously missing import can recover without an entry edit.
    pub fn refresh(&mut self) {
        let mut paths: Vec<_> = self.observed.keys().cloned().collect();
        if !paths.contains(&self.path) {
            paths.push(self.path.clone());
        }
        let observed: BTreeMap<_, _> = paths
            .into_iter()
            .map(|p| {
                let source = std::fs::read_to_string(&p).ok();
                (p, source)
            })
            .collect();
        let changed = observed != self.observed;
        if !changed && self.error.is_none() {
            return;
        }
        if changed {
            self.revision += 1;
        }
        self.discard_staged_preview();
        self.observed = observed;
        let result = self
            .observed
            .get(&self.path)
            .and_then(|s| s.as_ref())
            .ok_or_else(|| format!("cannot read {}", self.path.display()))
            .and_then(|source| self.evaluate(source));
        match result {
            Ok(render) => {
                // A newly discovered/recovered dependency changes the snapshot too.
                if !changed {
                    self.revision += 1;
                }
                self.observed = render
                    .snapshot
                    .sources()
                    .iter()
                    .map(|(p, s)| (p.clone(), Some(s.clone())))
                    .collect();
                self.last_good = Some(render);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    fn evaluate(&self, source: &str) -> Result<Render, String> {
        let phase = Instant::now();
        let hosts = HostDeclarations::default();
        let (extension, exports) = exports(source, self.svg_renderer);
        let snapshot = ProjectSnapshot::load_reusing(
            &self.path,
            &self.roots,
            &hosts,
            Some(source.into()),
            &extension,
            self.last_good.as_ref().map(|r| r.snapshot.as_ref()),
        )?;
        let compile_ms = phase.elapsed().as_secs_f64() * 1000.0;
        self.evaluate_checked(Arc::new(snapshot), exports, compile_ms, None)
    }

    fn evaluate_checked(
        &self,
        snapshot: Arc<ProjectSnapshot>,
        exports: Exports,
        compile_ms: f64,
        linked: Option<PreparedOperations>,
    ) -> Result<Render, String> {
        let scene_name = &exports.scene;
        let reused_checked_candidate = linked.is_some();
        let mut whips = crate::whips::declared(&snapshot.graph.entry().program);
        let (prepared, link_ms) = if let Some(prepared) = linked {
            (prepared, 0.0)
        } else {
            let phase = Instant::now();
            let mut names = exports.names();
            if self.profile_mode == ProfileMode::ForceData {
                names.push("program");
            }
            let prepared = snapshot.prepare_operations(
                self.libraries.clone(),
                &HostDeclarations::default(),
                &names,
            )?;
            (prepared, phase.elapsed().as_secs_f64() * 1000.0)
        };
        // Large drawings need more than the CLI's default budget: the scene export
        // and the SVG are both built from the same evaluated program.
        let limits = Limits {
            steps: 2_000_000_000,
            heap_entries: 50_000_000,
            #[cfg(feature = "incremental")]
            cache_minimum_work: 64,
            #[cfg(feature = "incremental")]
            cache_eviction_samples: 8,
            #[cfg(feature = "incremental")]
            compact_evidence: cfg!(feature = "compact-evidence"),
            #[cfg(feature = "incremental")]
            region_evidence: cfg!(feature = "region-evidence"),
            #[cfg(feature = "incremental")]
            joint_retention: cfg!(feature = "joint-retention"),
            ..Limits::default()
        };
        #[cfg(feature = "incremental")]
        let context_started = Instant::now();
        #[cfg(feature = "incremental")]
        // Compatibility metadata is optional cache support. A bounded metadata
        // miss starts fresh instead of preventing a valid drawing from rendering.
        let prepared = prepared
            .clone()
            .with_source_context(
                &snapshot
                    .graph
                    .modules
                    .iter()
                    .map(|m| m.program.as_ref())
                    .collect::<Vec<_>>(),
                1024 * 1024,
            )
            .unwrap_or(prepared);
        #[cfg(feature = "incremental")]
        let cache_context_ms = context_started.elapsed().as_secs_f64() * 1000.0;
        #[cfg(not(feature = "incremental"))]
        let cache_context_ms = 0.0;
        let phase = Instant::now();
        #[cfg(feature = "incremental")]
        // A failed bounded collection must not extend the retained world forever.
        // Rebase the following edit; the accepted render remains usable meanwhile.
        let (mut session, reused) =
            if let Some(previous) = self.last_good.as_ref().filter(|r| r.cache_age < 16) {
                prepared
                    .start_reusing(
                        &previous.session,
                        limits.clone(),
                        Arc::new(twine::HostRegistry::default()),
                        twine::RevisionReuse {
                            source: Some(twine::SourceForkLimits::default()),
                            ..Default::default()
                        },
                    )
                    .map_err(describe)?
            } else {
                (
                    prepared.start(limits.clone()).map_err(describe)?,
                    twine::ReuseStats::default(),
                )
            };
        #[cfg(not(feature = "incremental"))]
        let (mut session, reused) = {
            let mut session = prepared.start(limits.clone()).map_err(describe)?;
            let previous = self.last_good.as_ref();
            let reused = previous.map_or_else(twine::ReuseStats::default, |r| {
                session.reuse_with_limits(
                    &r.session,
                    twine::ReuseLimits {
                        heap_entries: 200_000,
                        payload_bytes: 16 * 1024 * 1024,
                        ..Default::default()
                    },
                )
            });
            (session, reused)
        };
        let reused_globals = reused.globals;
        #[cfg(feature = "incremental")]
        let shared_heap_pages = reused.shared_heap_pages;
        #[cfg(not(feature = "incremental"))]
        let shared_heap_pages = 0;
        #[cfg(feature = "incremental")]
        let mut cache_age = if shared_heap_pages > 0 {
            self.last_good.as_ref().map_or(0, |r| r.cache_age + 1)
        } else {
            0
        };
        let reuse_ms = phase.elapsed().as_secs_f64() * 1000.0;
        #[cfg(feature = "incremental")]
        let starting_evidence = EvidenceSnapshot::read(&session);
        let enabled = self.profile_mode != ProfileMode::Off;
        let force_data = self.profile_mode == ProfileMode::ForceData;
        let mut profile = enabled.then(|| Profile {
            force_data,
            before_outputs: Work::read(&session),
            ..Default::default()
        });
        if force_data {
            let (count, data) = render_profile::measure(&mut session, true, |s| {
                let program = s.binding("program").map_err(describe)?;
                let root = s.force(&program).map_err(describe)?;
                render_profile::force_data(s.read_values(), root)
            })?;
            let (replayed, replay) = render_profile::measure(&mut session, true, |s| {
                let program = s.binding("program").map_err(describe)?;
                let root = s.force(&program).map_err(describe)?;
                render_profile::force_data(s.read_values(), root)
            })?;
            if count != replayed {
                return Err("render profile data changed during replay".into());
            }
            let p = profile.as_mut().unwrap();
            p.data = data;
            p.data_replay = replay;
            p.data_values = count;
        }
        let phase = Instant::now();
        let (source_root_key, root_profile) =
            render_profile::measure(&mut session, enabled, |s| {
                if let Some(name) = &exports.root {
                    read_text(s, name)
                } else {
                    let binding = s.binding(scene_name).map_err(describe)?;
                    let root = s.force(&binding).map_err(describe)?;
                    crate::drawing_value::source_root_key(s.read_values(), root)
                }
            })?;
        let (mut svg, svg_profile) = if let Some(name) = &exports.svg {
            render_profile::measure(&mut session, enabled, |s| read_text(s, name))?
        } else {
            (String::new(), None)
        };
        let mut svg_ms = phase.elapsed().as_secs_f64() * 1000.0;
        if let Some(p) = &mut profile {
            p.root = root_profile;
            p.svg = svg_profile;
        }
        #[cfg(feature = "incremental")]
        let svg_evidence = EvidenceSnapshot::read(&session);
        let phase = Instant::now();
        let (root, scene_root) = render_profile::measure(&mut session, enabled, |s| {
            let binding = s.binding(scene_name).map_err(describe)?;
            s.force(&binding).map_err(describe)
        })?;
        if let Some(p) = &mut profile {
            p.scene_root = scene_root;
        }
        if force_data {
            let (count, forced) = render_profile::measure(&mut session, true, |s| {
                render_profile::force_data(s.read_values(), root)
            })?;
            let (replayed, replay) = render_profile::measure(&mut session, true, |s| {
                render_profile::force_data(s.read_values(), root)
            })?;
            if count != replayed {
                return Err("render profile scene changed during replay".into());
            }
            let p = profile.as_mut().unwrap();
            p.scene_replay = replay;
            p.scene_force = forced;
            p.scene_values = count;
        }
        let mut layers = (self.profile_mode == ProfileMode::Layers
            && self.svg_renderer == SvgRenderer::Direct)
            .then(render_profile::Layers::default);
        let decode_start = enabled.then(|| (Work::read(&session), Instant::now()));
        #[cfg(not(feature = "scene-observations"))]
        let scene = if self.svg_renderer == SvgRenderer::Direct {
            crate::drawing_value::decode(
                session.read_values(),
                root,
                layers.as_mut().map(|p| &mut p.first),
            )?
        } else {
            crate::scene_value::decode(session.read_values(), root)?
        };
        #[cfg(feature = "scene-observations")]
        let (scene, decoded, scene_cache) = if self.svg_renderer == SvgRenderer::Direct {
            (
                crate::drawing_value::decode(
                    session.read_values(),
                    root,
                    layers.as_mut().map(|p| &mut p.first),
                )?,
                Default::default(),
                Default::default(),
            )
        } else {
            crate::scene_value::decode_cached(
                session.read_values(),
                root,
                self.last_good.as_ref().map(|previous| &previous.decoded),
            )?
        };
        if let Some((before, started)) = decode_start {
            profile.as_mut().unwrap().scene_decode =
                Some(render_profile::finish(&session, before, started));
        }
        crate::whips::read_values(
            &mut whips,
            prepared.shared_program(),
            &snapshot.graph.entry().program.canonical.name,
            &scene,
            &source_root_key,
        );
        let scene_ms = phase.elapsed().as_secs_f64() * 1000.0;
        // Repeat the same decoder after its exact demands have been satisfied.
        // The repeat is outside scene_ms and only runs in the diagnostic mode.
        if matches!(
            self.profile_mode,
            ProfileMode::DecodeReplay | ProfileMode::Layers
        ) && self.svg_renderer == SvgRenderer::Direct
        {
            let (replayed, cost) = render_profile::measure(&mut session, true, |s| {
                crate::drawing_value::decode(
                    s.read_values(),
                    root,
                    layers.as_mut().map(|p| &mut p.replay),
                )
            })?;
            if replayed != scene {
                return Err("replayed drawing decode changed the scene".into());
            }
            profile.as_mut().unwrap().decode_replay = cost;
        }
        if self.svg_renderer != SvgRenderer::Knot {
            let phase = Instant::now();
            let (rendered, measured) = render_profile::measure(&mut session, enabled, |_| {
                crate::scene_svg::render(&scene)
            })?;
            svg = rendered;
            if let Some(p) = &mut profile {
                p.native_svg = measured;
            }
            svg_ms += phase.elapsed().as_secs_f64() * 1000.0;
        }
        if let Some(p) = &mut profile {
            p.after_outputs = Work::read(&session);
        }
        let runtime_steps = session.stats().steps;
        let runtime_thunks = session.stats().thunk_evaluations;
        #[cfg(feature = "incremental")]
        let (cache_evictions, cache_admission_rejections, cache_policy_work) = (
            session.stats().cache_evictions,
            session.stats().cache_admission_rejections,
            session.stats().cache_eviction_work,
        );
        #[cfg(not(feature = "incremental"))]
        let (cache_evictions, cache_admission_rejections, cache_policy_work) = (0, 0, 0);
        let runtime_entries = session.heap_entries();
        #[cfg(feature = "incremental")]
        let mut cache_diagnostics = Some(CacheDiagnostics {
            starting_evidence,
            svg_evidence,
            evidence_records: session.evidence_stats().certificates,
            evidence_reads: session.evidence_stats().reads,
            evidence_bytes: session.evidence_stats().bytes,
            evidence_unrecorded: session.evidence_stats().unrecorded,
            evidence_summaries: session.evidence_stats().summaries,
            evidence_summary_hits: session.evidence_stats().summary_hits,
            evidence_summary_index_bytes: session.evidence_stats().summary_index_bytes,
            evidence_region_bytes: session.evidence_stats().region_bytes,
            evidence_region_normalizations: session.evidence_stats().region_normalizations,
            call_hits: session.stats().application_cache_hits,
            call_key_skips: session.stats().application_key_skips,
            call_validity_misses: session.stats().application_validity_misses,
            call_scope_misses: session.stats().application_scope_misses,
            ..Default::default()
        });
        #[cfg(not(feature = "incremental"))]
        let cache_diagnostics = None;
        #[allow(unused_mut)]
        let mut cache_maintenance_ms = 0.0;
        #[allow(unused_mut)]
        let mut cache_maintenance_steps = 0;
        #[allow(unused_mut)]
        let mut collected_runtime = false;
        #[cfg(feature = "incremental")]
        if cache_age >= 16 || runtime_entries > 500_000 {
            // The native scene is fully decoded and owns no runtime handles.
            // Keep the program for queries and, in Knot mode, the completed SVG.
            // Native outputs own no runtime handles; their graphs are optional.
            let phase = Instant::now();
            let module = snapshot.graph.entry().program.canonical.name.clone();
            let mut roots = vec![knot_core::GlobalName {
                module: module.clone(),
                name: "program".into(),
            }];
            if let Some(name) = &exports.svg {
                roots.push(knot_core::GlobalName {
                    module,
                    name: name.clone(),
                });
            }
            let attempt = session
                .collect_globals(
                    &roots,
                    Limits {
                        steps: limits.steps.saturating_sub(session.stats().steps),
                        ..limits
                    },
                    twine::CollectionBudget {
                        entries: 200_000,
                        bytes: 32 * 1024 * 1024,
                        retention: Some(twine::CacheRetention::default()),
                    },
                )
                .map_err(describe)?;
            cache_maintenance_steps = attempt.work;
            if let Some(collected) = attempt.session {
                if let Some(diagnostics) = &mut cache_diagnostics {
                    diagnostics.retained_evidence = Some(EvidenceSnapshot::read(&collected));
                    diagnostics.retention_storage_queries =
                        collected.stats().retention_storage_queries;
                    diagnostics.retention_unknown_storage =
                        collected.stats().retention_unknown_storage;
                    diagnostics.retention_storage_steps = collected.stats().retention_storage_steps;
                    diagnostics.retention_graph_description_hits =
                        collected.stats().retention_graph_description_hits;
                    diagnostics.retention_graph_description_misses =
                        collected.stats().retention_graph_description_misses;
                    diagnostics.retention_graph_description_evictions =
                        collected.stats().retention_graph_description_evictions;
                    diagnostics.retention_graph_description_bytes =
                        collected.stats().retention_graph_description_bytes;
                    diagnostics.retention_candidates = collected.stats().retention_candidates;
                    diagnostics.retention_admitted = collected.stats().retention_admitted;
                    diagnostics.retention_index_rejections =
                        collected.stats().retention_index_rejections;
                }
                session = collected;
                cache_age = 0;
                collected_runtime = true;
            } else {
                // A size-triggered miss also prevents sharing on the next edit.
                cache_age = 16;
            }
            cache_maintenance_ms = phase.elapsed().as_secs_f64() * 1000.0;
        }
        Ok(Render {
            whips,
            timings: RenderTimings {
                layers,
                profile,
                reused_checked_candidate,
                compile_ms,
                link_ms,
                svg_ms,
                svg_renderer: match self.svg_renderer {
                    SvgRenderer::Knot => "knot",
                    #[cfg(not(target_os = "wasi"))]
                    SvgRenderer::Scene => "scene",
                    SvgRenderer::Direct => "direct",
                },
                scene_ms,
                #[cfg(feature = "scene-observations")]
                scene_cache,
                reuse_ms,
                reused_globals,
                transferred_thunks: reused.transferred_thunks,
                transferred_values: reused.transferred_values,
                transferred_environments: reused.transferred_environments,
                cache_context_ms,
                cache_maintenance_ms,
                cache_maintenance_steps,
                shared_heap_pages,
                collected_runtime,
                runtime_steps,
                runtime_thunks,
                cache_evictions,
                cache_admission_rejections,
                cache_policy_work,
                runtime_entries,
                retained_runtime_entries: session.heap_entries(),
                cache_diagnostics,
            },
            scene: Arc::new(scene),
            svg,
            snapshot,
            exports,
            session,
            #[cfg(feature = "incremental")]
            cache_age,
            #[cfg(feature = "scene-observations")]
            decoded,
            source_root_key,
        })
    }
}

#[derive(Clone)]
struct Exports {
    scene: String,
    svg: Option<String>,
    root: Option<String>,
}
impl Exports {
    fn names(&self) -> Vec<&str> {
        std::iter::once(self.scene.as_str())
            .chain(self.svg.as_deref())
            .chain(self.root.as_deref())
            .collect()
    }
}

/// Preserve original offsets, CRLFs and multiline headers. Imports enter the
/// parsed AST, and generated bindings live beyond the editable source range.
fn exports(source: &str, renderer: SvgRenderer) -> (EntryExtension, Exports) {
    let prefix = (0..)
        .map(|n| format!("protoappGenerated{n}"))
        .find(|p| !source.contains(p))
        .unwrap();
    let alias = format!("T{prefix}");
    let scene = format!("{prefix}Scene");
    if renderer == SvgRenderer::Direct {
        let suffix = format!("\n\n{scene} :: {alias}.Container\n{scene} = program\n");
        return (
            EntryExtension {
                imports: vec![(vec!["Types".into()], alias)],
                suffix,
            },
            Exports {
                scene,
                svg: None,
                root: None,
            },
        );
    }
    let root = format!("{prefix}Root");
    let mut suffix = format!("\n\n{scene} :: {alias}.Json\n{scene} = {alias}.scene_value program\n{root} :: String\n{root} = {alias}.source_root_key program\n");
    let svg = (renderer == SvgRenderer::Knot).then(|| {
        let name = format!("{prefix}Svg");
        suffix.push_str(&format!(
            "{name} :: String\n{name} = {alias}.render_keyed program\n"
        ));
        name
    });
    (
        EntryExtension {
            imports: vec![(vec!["Scene".into()], alias)],
            suffix,
        },
        Exports {
            scene,
            svg,
            root: Some(root),
        },
    )
}

fn read_text(session: &mut OperationSession, name: &str) -> Result<String, String> {
    let binding = session.binding(name).map_err(describe)?;
    let value = session.force(&binding).map_err(describe)?;
    match session.view(value).map_err(describe)? {
        ValueView::Literal(Literal::String(text)) => Ok(text.to_string()),
        other => Err(format!("{name}: expected a String, got {other:?}")),
    }
}

fn describe(error: CallError) -> String {
    match error {
        CallError::Runtime(Outcome::Runtime(e)) => format!("runtime error: {:?}", e.kind),
        CallError::Runtime(Outcome::BudgetExhausted) => "evaluation budget exhausted".into(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    #[test]
    fn profiling_preserves_outputs_and_accounts_for_all_output_work() {
        use knot_workbench::{
            traversal::{Request, SourceEditor},
            unravel::EditLimits,
        };
        let dir = std::env::temp_dir().join(format!("knot-render-phases-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        let source = "module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nradius :: Float\nradius = 12\nprogram = svg 200 100 [circle 20 30 radius, circle 60 30 radius]\n";
        for renderer in [SvgRenderer::Knot, SvgRenderer::Scene, SvgRenderer::Direct] {
            for mode in [
                ProfileMode::Phases,
                ProfileMode::ForceData,
                ProfileMode::DecodeReplay,
                ProfileMode::Layers,
            ] {
                std::fs::write(&path, source).unwrap();
                let mut drawing =
                    Drawing::with_renderer(path.clone(), &library(), None, renderer).unwrap();
                assert!(drawing.last_good().unwrap().timings.profile.is_none());
                let changed = source.replace("radius = 12", "radius = 14");
                let expected = drawing.evaluate(&changed).unwrap();
                drawing.profile_mode = mode;
                let engine = SourceEditor::new().unwrap();
                let original = drawing.last_good().unwrap();
                let edit = engine
                    .prepare(
                        &original.snapshot,
                        "radius",
                        &Request {
                            expression: "Just 14".into(),
                            type_name: "Maybe Float".into(),
                        },
                        drawing.roots(),
                        EditLimits::default(),
                    )
                    .unwrap();
                drawing
                    .save_prepared_profiled(edit.snapshot(), &edit)
                    .unwrap();
                let actual = drawing.last_good().unwrap();
                assert_eq!(actual.scene, expected.scene);
                assert_eq!(actual.svg, expected.svg);
                assert_eq!(actual.source_root_key, expected.source_root_key);
                let p = actual.timings.profile.unwrap();
                let phases = [
                    p.data,
                    p.data_replay,
                    p.root,
                    p.svg,
                    p.scene_root,
                    p.scene_force,
                    p.scene_replay,
                    p.scene_decode,
                    p.decode_replay,
                    p.native_svg,
                ];
                let steps: u64 = phases.iter().flatten().map(|phase| phase.work.steps).sum();
                assert_eq!(steps, p.after_outputs.steps - p.before_outputs.steps);
                if matches!(mode, ProfileMode::DecodeReplay | ProfileMode::Layers)
                    && renderer == SvgRenderer::Direct
                {
                    let replay = p.decode_replay.unwrap();
                    assert_eq!(replay.work.thunk_evaluations, 0);
                    assert_eq!(replay.work.primitive_calls, 0);
                }
                if mode == ProfileMode::Layers && renderer == SvgRenderer::Direct {
                    let layers = actual.timings.layers.as_ref().unwrap();
                    assert_eq!(layers.first.len(), 2);
                    assert_eq!(layers.replay.len(), 2);
                    assert!(layers
                        .first
                        .iter()
                        .all(|l| l.kind == "circle" && l.children == 0));
                    assert!(layers
                        .replay
                        .iter()
                        .all(|l| l.thunks == 0 && l.primitives == 0));
                }
                if mode == ProfileMode::ForceData {
                    assert!(p.data_values > 0 && p.scene_values > 0);
                    assert_eq!(p.data_replay.unwrap().work.thunk_evaluations, 0);
                    assert_eq!(p.data_replay.unwrap().work.primitive_calls, 0);
                    assert_eq!(p.scene_decode.unwrap().work.thunk_evaluations, 0);
                    assert_eq!(p.scene_replay.unwrap().work.thunk_evaluations, 0);
                } else {
                    assert!(p.data.is_none() && p.scene_force.is_none());
                }
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "scene-observations")]
    #[test]
    fn decoded_scene_receipts_keep_fresh_parity_through_edits() {
        let dir = std::env::temp_dir().join(format!("knot-scene-receipts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        let source = |width| {
            format!("module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nwidth :: Float\nwidth = {width}\nfixed = circle 50 50 10 |> set_fill blue\nprogram = svg 200 100 [rect 10 20 width 30, fixed]\n")
        };
        std::fs::write(&path, source(40)).unwrap();
        let mut drawing =
            Drawing::with_renderer(path.clone(), &library(), None, SvgRenderer::Scene).unwrap();
        let mut hits = 0;
        for width in [41, 42, 40, 43] {
            let snapshot = drawing.last_good().unwrap().snapshot.id().to_owned();
            drawing.save_source(&snapshot, &source(width)).unwrap();
            let current = drawing.last_good().unwrap();
            let fresh = Drawing::new(path.clone(), &library(), None).unwrap();
            assert_eq!(current.scene, fresh.last_good().unwrap().scene);
            assert_eq!(current.svg, fresh.last_good().unwrap().svg);
            hits += current.timings.scene_cache.hits;
        }
        assert!(
            hits > 0,
            "unchanged leaf must reuse decoded output on a source edit"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn native_scene_renderer_survives_edits_and_collection_without_forcing_knot_svg() {
        let dir = std::env::temp_dir().join(format!("knot-native-renderer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        let source = |width| {
            format!("module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nprogram = svg 200 100 [rect 10 20 {width} 30 |> set_fill blue]\n")
        };
        for renderer in [SvgRenderer::Scene, SvgRenderer::Direct] {
            std::fs::write(&path, source(40)).unwrap();
            let mut drawing =
                Drawing::with_renderer(path.clone(), &library(), None, renderer).unwrap();
            assert_eq!(drawing.error(), None);
            #[cfg(feature = "incremental")]
            let mut collected = false;
            for width in 41..=58 {
                let snapshot = drawing.last_good().unwrap().snapshot.id().to_owned();
                drawing.save_source(&snapshot, &source(width)).unwrap();
                let current = drawing.last_good().unwrap();
                assert_ne!(current.timings.svg_renderer, "knot");
                #[cfg(feature = "scene-observations")]
                if renderer == SvgRenderer::Direct {
                    assert_eq!(current.timings.scene_cache.attempts, 0);
                }
                let fresh = Drawing::new(path.clone(), &library(), None).unwrap();
                let expected = fresh.last_good().unwrap();
                assert_eq!(current.scene, expected.scene);
                assert_eq!(current.svg, expected.svg);
                #[cfg(feature = "incremental")]
                {
                    collected |= current.timings.collected_runtime;
                }
            }
            #[cfg(feature = "incremental")]
            assert!(
                collected,
                "native output must cross the ordinary collection boundary"
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn default_renderer_needs_no_knot_scene_exporter() {
        let dir = std::env::temp_dir().join(format!("knot-native-default-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "Types.knot",
            "KnottyDrawings.knot",
            "DrawingUnravel.knot",
            "Constrictions.knot",
        ] {
            std::fs::copy(library().join(name), dir.join(name)).unwrap();
        }
        // A local Scene module must not be imported by the native backend.
        std::fs::write(dir.join("Scene.knot"), "not a valid Knot module").unwrap();
        let path = dir.join("Drawing.knot");
        for (program, root) in [
            ("svg 200 100 [circle 20 30 10]", ""),
            (
                "{ kind = Group, common = defaultCommon, children = [circle 20 30 10] }",
                "0",
            ),
        ] {
            std::fs::write(&path, format!("module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nimport Types exposing (..)\nprogram = {program}\n")).unwrap();
            let drawing = Drawing::new(path.clone(), &dir, None).unwrap();
            assert_eq!(drawing.error(), None);
            let rendered = drawing.last_good().unwrap();
            assert_eq!(rendered.timings.svg_renderer, "direct");
            assert_eq!(rendered.source_root_key, root);
            assert!(rendered.svg.contains("<circle"));
            assert_eq!(rendered.exports.names().len(), 1);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn direct_decoder_failure_keeps_the_last_good_render() {
        let dir = std::env::temp_dir().join(format!("knot-direct-failure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        let source = "module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nprogram = svg 200 100 [circle 50 50 10]\n";
        std::fs::write(&path, source).unwrap();
        let mut drawing =
            Drawing::with_renderer(path.clone(), &library(), None, SvgRenderer::Direct).unwrap();
        assert_eq!(drawing.error(), None);
        let previous = drawing.last_good().unwrap();
        let snapshot = previous.snapshot.id().to_owned();
        let scene = previous.scene.clone();
        let lazy_error = source.replace(
            "circle 50 50 10",
            "circle 50 50 (error \"lazy radius failed\")",
        );
        let nonfinite = source.replace("circle 50 50 10", "circle 50 50 (0 / 0)");
        let nested = source.replace("program =", "import Types exposing (Node)\nnest :: Int -> Node\nnest n = if n == 0 then circle 50 50 10 else group [nest (n - 1)]\nprogram =")
            .replace("[circle 50 50 10]", "[nest 130]");
        for (candidate, expected) in [
            (lazy_error, "lazy radius failed"),
            (nonfinite, "non-finite drawing Float"),
            (nested, "drawing nesting exceeds"),
        ] {
            let error = drawing.save_source(&snapshot, &candidate).unwrap_err();
            assert!(error.contains(expected), "{error}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
            assert_eq!(drawing.last_good().unwrap().scene, scene);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scene_payload_failure_preserves_the_published_render_and_source() {
        let dir = std::env::temp_dir().join(format!("knot-scene-failure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "Types.knot",
            "KnottyDrawings.knot",
            "DrawingUnravel.knot",
            "Constrictions.knot",
        ] {
            std::fs::copy(library().join(name), dir.join(name)).unwrap();
        }
        let path = dir.join("Drawing.knot");
        // A valid SVG can be produced before a later lazy scene field fails.
        std::fs::write(
            dir.join("Scene.knot"),
            r#"module Scene exposing (..)
import Types exposing (..)
type Json = JsonNull | JsonArray (List Json)
source_root_key :: Container -> String
source_root_key _ = ""
render_keyed :: Container -> String
render_keyed _ = "<svg/>"
scene_value :: Container -> Json
scene_value document = JsonArray [JsonNull, case document.kind of
    Viewport viewport -> if viewport.width == 0 then error "scene payload failed" else JsonNull
    Group -> JsonNull]
"#,
        )
        .unwrap();
        let source = "module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nprogram = svg 10 20 []\n";
        std::fs::write(&path, source).unwrap();
        let mut drawing =
            Drawing::with_renderer(path.clone(), &dir, None, SvgRenderer::Knot).unwrap();
        assert_eq!(drawing.error(), None);
        let before = drawing.last_good().unwrap();
        let snapshot = before.snapshot.id().to_owned();
        let scene = before.scene.clone();
        let svg = before.svg.clone();
        let revision = drawing.revision();
        assert!(drawing
            .save_source(&snapshot, &source.replace("svg 10", "svg 0"))
            .is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
        assert_eq!(drawing.revision(), revision);
        let retained = drawing.check_snapshot(&snapshot).unwrap();
        assert_eq!(retained.scene, scene);
        assert_eq!(retained.svg, svg);
        drawing
            .save_source(&snapshot, &source.replace("svg 10", "svg 30"))
            .unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("svg 30"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn demo_exports_a_keyed_scene() {
        let drawing = Drawing::new(
            library().join("protoapp/tests/fixtures/demo.knot"),
            &library(),
            None,
        )
        .unwrap();
        assert_eq!(drawing.error(), None);
        let render = drawing.last_good().unwrap();
        assert!(render.svg.contains("data-key=\"2.1\""));
        let scene = &render.scene;
        assert_eq!(scene["kind"], "svg");
        assert_eq!(scene["children"][2]["kind"], "group");
        assert_eq!(scene["children"][1]["props"]["cx"], 120.0);
        assert_eq!(
            scene["children"][1]["props"]["fill.paint"],
            serde_json::json!([255, 99, 71])
        );
        assert_eq!(scene["children"][2]["children"][1]["key"], "2.1");
        assert_eq!(scene["children"][2]["children"][1]["kind"], "line");
    }

    #[test]
    fn large_example_fits_the_budget() {
        let drawing =
            Drawing::new(library().join("example-drawing.knot"), &library(), None).unwrap();
        assert_eq!(drawing.error(), None);
        assert!(
            drawing.last_good().unwrap().scene["children"]
                .as_array()
                .unwrap()
                .len()
                > 50
        );
    }

    #[test]
    fn scalar_reuse_render_matches_fresh_after_an_unrelated_edit() {
        let dir = std::env::temp_dir().join(format!("knot-render-reuse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        let source = "module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\ncost :: Int -> Float\ncost n = if n == 0 then 20 else 1 + cost (n - 1)\nradius :: Float\nradius = cost 1000\nprogram = svg 800 600 [rect 10 10 40 20, circle 100 100 radius]\n";
        std::fs::write(&path, source).unwrap();
        let mut drawing = Drawing::new(path.clone(), &library(), None).unwrap();
        let revision = drawing.last_good().unwrap().snapshot.id().to_owned();
        drawing
            .save_source(&revision, &source.replace("40 20", "45 20"))
            .unwrap();
        let reused = drawing.last_good().unwrap();
        assert!(reused.timings.reused_globals > 0 || reused.timings.shared_heap_pages > 0);
        let fresh = Drawing::new(path, &library(), None).unwrap();
        let fresh = fresh.last_good().unwrap();
        assert_eq!(reused.scene, fresh.scene);
        assert_eq!(reused.svg, fresh.svg);
        assert!(reused.session.stats().thunk_evaluations < fresh.session.stats().thunk_evaluations);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "incremental")]
    #[test]
    fn shared_render_revisions_collect_at_complete_roots_and_keep_fresh_parity() {
        let dir = std::env::temp_dir().join(format!(
            "knot-render-source-sequence-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        let source = |radius| {
            format!("module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nradius :: Float\nradius = {radius}\nprogram = svg 300 200 [circle 50 50 radius, rect 100 30 40 20]\n")
        };
        std::fs::write(&path, source(10)).unwrap();
        let mut drawing = Drawing::new(path.clone(), &library(), None).unwrap();
        let mut collections = 0;
        for radius in 11..=42 {
            let revision = drawing.last_good().unwrap().snapshot.id().to_owned();
            let timing = drawing
                .save_source_profiled(&revision, &source(radius))
                .unwrap();
            assert!(timing.render.shared_heap_pages > 0);
            if timing.render.collected_runtime {
                collections += 1;
            }
            let fresh = Drawing::new(path.clone(), &library(), None).unwrap();
            let current = drawing.last_good().unwrap();
            let fresh = fresh.last_good().unwrap();
            assert_eq!(current.scene, fresh.scene);
            assert_eq!(current.svg, fresh.svg);
            assert!(current.cache_age < 16);
            assert!(current.session.heap_entries() < 100_000);
        }
        assert_eq!(collections, 2);
        // Simulate a collection miss. The next edit must discard the accumulated
        // cache even when source sharing would otherwise be possible.
        drawing.last_good.as_mut().unwrap().cache_age = 16;
        let revision = drawing.last_good().unwrap().snapshot.id().to_owned();
        let timing = drawing
            .save_source_profiled(&revision, &source(43))
            .unwrap();
        assert_eq!(timing.render.shared_heap_pages, 0);
        let current = drawing.last_good().unwrap();
        assert_eq!(current.cache_age, 0);
        let fresh = Drawing::new(path, &library(), None).unwrap();
        assert_eq!(current.scene, fresh.last_good().unwrap().scene);
        assert_eq!(current.svg, fresh.last_good().unwrap().svg);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "incremental")]
    #[test]
    fn decoded_large_scene_does_not_pin_its_runtime_export_graph() {
        let dir =
            std::env::temp_dir().join(format!("knot-render-large-roots-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        let shapes = (0..200)
            .map(|n| format!("rect {} {} width 12", (n % 20) * 20, (n / 20) * 20))
            .collect::<Vec<_>>()
            .join(", ");
        let source = |width| {
            format!("module Drawing exposing (..)\nimport KnottyDrawings exposing (..)\nwidth :: Float\nwidth = {width}\nprogram = svg 500 400 [{shapes}]\n")
        };
        std::fs::write(&path, source(10)).unwrap();
        let mut drawing =
            Drawing::with_renderer(path.clone(), &library(), None, SvgRenderer::Knot).unwrap();
        // Exercise the maintenance boundary without making 16 identical passes.
        drawing.last_good.as_mut().unwrap().cache_age = 15;
        let revision = drawing.last_good().unwrap().snapshot.id().to_owned();
        let timing = drawing
            .save_source_profiled(&revision, &source(11))
            .unwrap();
        assert!(timing.render.collected_runtime);
        assert!(timing.render.runtime_entries > timing.render.retained_runtime_entries * 2);
        let current = drawing.last_good().unwrap();
        let fresh =
            Drawing::with_renderer(path.clone(), &library(), None, SvgRenderer::Knot).unwrap();
        assert_eq!(current.scene, fresh.last_good().unwrap().scene);
        assert_eq!(current.svg, fresh.last_good().unwrap().svg);
        // Later source evaluation needs only current checked code and retained
        // roots; it must not depend on the dropped decoded scene's handles.
        let revision = current.snapshot.id().to_owned();
        drawing.save_source(&revision, &source(12)).unwrap();
        let current = drawing.last_good().unwrap();
        let fresh = Drawing::with_renderer(path, &library(), None, SvgRenderer::Knot).unwrap();
        assert_eq!(current.scene, fresh.last_good().unwrap().scene);
        assert_eq!(current.svg, fresh.last_good().unwrap().svg);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn generated_exports_avoid_existing_names() {
        let (extension, exports) = exports("protoappGenerated0Scene = 2\r\n", SvgRenderer::Knot);
        assert_eq!(exports.scene, "protoappGenerated1Scene");
        assert!(extension.suffix.starts_with("\n\n"));
    }

    #[test]
    fn checked_candidate_handoff_matches_fresh_render_and_keeps_publication_guards() {
        use knot_workbench::{
            traversal::{Request, SourceEditor},
            unravel::EditLimits,
        };
        let dir = std::env::temp_dir().join(format!("knot-render-handoff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Drawing.knot");
        let dependency = dir.join("Size.knot");
        let source = "module Drawing exposing (radius)\r\nimport KnottyDrawings exposing (..)\r\nimport Size\r\nradius :: Float\r\nradius = 10\r\nprogram = if radius == 0 then error \"cannot render\" else svg Size.width 100 [circle 20 30 radius]\r\n";
        let size = "module Size exposing (..)\nwidth :: Float\nwidth = 100\n";
        std::fs::write(&path, source).unwrap();
        std::fs::write(&dependency, size).unwrap();
        let mut drawing = Drawing::new(path.clone(), &library(), None).unwrap();
        assert_eq!(drawing.error(), None);
        let editor = SourceEditor::new().unwrap();
        let prepare = |drawing: &Drawing, value| {
            editor
                .prepare(
                    &drawing.last_good().unwrap().snapshot,
                    "radius",
                    &Request {
                        expression: format!("Just {value}"),
                        type_name: "Maybe Float".into(),
                    },
                    drawing.roots(),
                    EditLimits::default(),
                )
                .unwrap()
        };
        let edit = prepare(&drawing, 15);
        let candidate = edit.candidate_snapshot().clone();
        let expected = drawing.evaluate(edit.candidate_source()).unwrap();
        let id = drawing.last_good().unwrap().snapshot.id().to_owned();
        let timing = drawing.save_prepared_profiled(&id, &edit).unwrap();
        let rendered = drawing.last_good().unwrap();
        assert!(timing.render.reused_checked_candidate);
        assert_eq!(timing.render.compile_ms, 0.0);
        assert_eq!(timing.render.link_ms, 0.0);
        assert!(Arc::ptr_eq(&candidate, &rendered.snapshot));
        assert_eq!(rendered.svg, expected.svg);
        assert_eq!(rendered.scene, expected.scene);
        assert_eq!(rendered.source_root_key, expected.source_root_key);
        assert!(rendered.snapshot.matches_disk());
        assert!(edit.candidate_source().contains("\r\n"));

        let id = rendered.snapshot.id().to_owned();
        let before = std::fs::read_to_string(&path).unwrap();
        let failing = prepare(&drawing, 0);
        assert!(drawing.save_prepared_profiled(&id, &failing).is_err());
        assert_eq!(drawing.last_good().unwrap().snapshot.id(), id);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        let next = prepare(&drawing, 20);
        std::fs::write(&dependency, size.replace("100", "200")).unwrap();
        assert!(drawing.save_prepared_profiled(&id, &next).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        std::fs::write(&dependency, size).unwrap();
        let external = format!("{before}\n-- another editor\n");
        std::fs::write(&path, &external).unwrap();
        assert!(drawing.save_prepared_profiled(&id, &next).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), external);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    #[test]
    fn stale_scenes_dependencies_and_recovery_keep_snapshot_identity_honest() {
        let dir = std::env::temp_dir().join(format!("knot-app-snapshots-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Main.knot");
        let dep = dir.join("Size.knot");
        let source = "module Main exposing\r\n  (..)\r\nimport KnottyDrawings exposing (..)\r\nimport Size\r\nprogram = svg Size.width 300 []\r\n";
        std::fs::write(&file, source).unwrap();
        std::fs::write(
            &dep,
            "module Size exposing (..)\nwidth :: Float\nwidth = 400\n",
        )
        .unwrap();
        let library = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut drawing = Drawing::new(file.clone(), &library, None).unwrap();
        assert_eq!(drawing.error(), None);
        let id = drawing.last_good().unwrap().snapshot.id().to_string();
        assert!(drawing.check_snapshot(&id).is_ok());
        std::fs::write(&file, "invalid source").unwrap();
        drawing.refresh();
        assert!(drawing.error().is_some());
        assert_eq!(drawing.last_good().unwrap().snapshot.id(), id);
        assert!(drawing.check_snapshot(&id).is_err());
        std::fs::write(&file, source).unwrap();
        drawing.refresh();
        assert_eq!(drawing.error(), None);
        assert_ne!(drawing.last_good().unwrap().snapshot.id(), id);
        let id = drawing.last_good().unwrap().snapshot.id().to_string();
        std::fs::write(
            &dep,
            "module Size exposing (..)\nwidth :: Float\nwidth = 500\n",
        )
        .unwrap();
        assert!(drawing.check_snapshot(&id).is_err());
        drawing.refresh();
        assert_eq!(drawing.error(), None);
        assert_eq!(drawing.last_good().unwrap().scene["props"]["width"], 500.0);
        std::fs::remove_file(&file).unwrap();
        drawing.refresh();
        assert!(drawing.error().is_some());
        std::fs::write(&file, source).unwrap();
        drawing.refresh();
        assert_eq!(drawing.error(), None);
        let group = "module Main exposing (..)\nimport Types exposing (..)\nprogram = { kind = Group, common = defaultCommon, children = [] }\n";
        std::fs::write(&file, group).unwrap();
        drawing.refresh();
        assert_eq!(drawing.error(), None);
        let render = drawing.last_good().unwrap();
        assert!(render.source_key("").is_err());
        assert_eq!(render.source_key("0").unwrap(), Vec::<usize>::new());
        assert_eq!(render.source_key("0.2").unwrap(), vec![2]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
