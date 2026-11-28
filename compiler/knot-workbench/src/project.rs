//! Revision-local source ownership for editing. Dependencies and generated
//! helpers can be inspected, but only original entry-file expressions are writable.
use crate::modules::{EntryExtension, ModuleGraph};
use knot_checker::host::HostDeclarations;
use knot_syntax::span::Span;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

pub struct ProjectSnapshot {
    pub graph: ModuleGraph,
    id: String,
    sources: BTreeMap<PathBuf, String>,
    entry: PathBuf,
    extension: EntryExtension,
    /// Entry-file definitions marked `@belay`: read, never edited.
    belayed: Vec<Span>,
}
impl ProjectSnapshot {
    pub fn load(
        entry: &Path,
        roots: &[PathBuf],
        hosts: &HostDeclarations,
        source: Option<String>,
        extension: &EntryExtension,
    ) -> Result<Self, String> {
        Self::load_reusing(entry, roots, hosts, source, extension, None)
    }
    pub fn load_reusing(
        entry: &Path,
        roots: &[PathBuf],
        hosts: &HostDeclarations,
        source: Option<String>,
        extension: &EntryExtension,
        previous: Option<&Self>,
    ) -> Result<Self, String> {
        let source = match source {
            Some(s) => s,
            None => std::fs::read_to_string(entry).map_err(|e| e.to_string())?,
        };
        let graph = ModuleGraph::load_reusing(
            entry,
            roots,
            hosts,
            Some(source.clone()),
            extension,
            previous.map(|p| &p.graph),
        )?;
        let entry = graph.entry().path.clone();
        let mut sources: BTreeMap<_, _> = graph
            .modules
            .iter()
            .map(|m| (m.path.clone(), m.source.clone()))
            .collect();
        sources.insert(entry.clone(), source);
        static PROCESS: OnceLock<String> = OnceLock::new();
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let process = PROCESS.get_or_init(|| {
            format!(
                "{}-{}",
                process_id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            )
        });
        let id = format!("{process}-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let decls = &graph.entry().program.canonical.decls;
        let functions = || {
            decls.iter().filter_map(|d| match &d.node {
                knot_canonical::ast::CDecl::Fn(f) => Some((d.span, f)),
                _ => None,
            })
        };
        // Every clause of a marked function, wherever the mark is written.
        let belayed = functions()
            .filter(|(_, f)| {
                functions().any(|(_, marked)| {
                    marked.name == f.name && marked.annotations.iter().any(|a| a.key == "belay")
                })
            })
            .map(|(span, f)| Span {
                start: span.start.min(f.body.span.start),
                end: span.end.max(f.body.span.end),
            })
            .collect();
        Ok(Self {
            belayed,
            graph,
            id,
            sources,
            entry,
            extension: extension.clone(),
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn sources(&self) -> &BTreeMap<PathBuf, String> {
        &self.sources
    }
    /// Link checked project exports into the typed runtime boundary. This is the
    /// source of the executable/type association, not adoption of arbitrary Core.
    pub fn prepare_operations(
        &self,
        libraries: knot_core::Program,
        hosts: &HostDeclarations,
        names: &[&str],
    ) -> Result<crate::operation::PreparedOperations, String> {
        let modules = self
            .graph
            .modules
            .iter()
            .map(|m| m.program.as_ref())
            .collect::<Vec<_>>();
        let program = knot_core::link::link_borrowed(libraries, &modules, hosts)?;
        crate::operation::PreparedOperations::from_linked(
            std::sync::Arc::new(program),
            &self.graph.entry().program,
            names,
        )
    }
    /// Exact checked ABI context for this project's linked executable. Generated
    /// transaction helpers require their own checked modules and are rejected
    /// here; this boundary covers the project and its entry exports only.
    #[cfg(feature = "incremental")]
    pub fn runtime_context(
        &self,
        program: &knot_core::Program,
        max_bytes: usize,
    ) -> Result<std::sync::Arc<[u8]>, String> {
        let modules = self
            .graph
            .modules
            .iter()
            .map(|m| m.program.as_ref())
            .collect::<Vec<_>>();
        crate::revision::runtime_context_for(program, &modules, max_bytes)
    }
    /// Generated imports/exports belong to this checked snapshot, not editable
    /// source. Candidate checking must retain them to preserve its code contract.
    pub fn entry_extension(&self) -> &EntryExtension {
        &self.extension
    }
    pub fn entry_path(&self) -> &Path {
        &self.entry
    }
    /// Optimistic validation for disk-backed apps; unsaved editor buffers should
    /// compare their own source versions instead. Includes read-only dependencies.
    pub fn matches_disk(&self) -> bool {
        self.sources
            .iter()
            .all(|(path, source)| std::fs::read_to_string(path).is_ok_and(|now| now == *source))
    }
    /// The span lies in an entry-file definition marked `@belay`.
    pub fn belayed(&self, span: Span) -> bool {
        self.belayed
            .iter()
            .any(|b| b.start <= span.start && span.end <= b.end)
    }
    /// A frozen definition does not pin its value at an editable reference site.
    /// Span ownership is checked at the reference expression, not its definition.
    pub fn editable_source(&self, path: &Path, span: Span) -> Option<&str> {
        if path != self.entry || self.belayed(span) {
            return None;
        }
        self.sources
            .get(path)?
            .get(span.start as usize..span.end as usize)
    }
}

/// WASI sessions have isolated filesystems and no operating-system process ID.
pub fn process_id() -> u32 {
    #[cfg(target_os = "wasi")] { 0 }
    #[cfg(not(target_os = "wasi"))] { std::process::id() }
}
