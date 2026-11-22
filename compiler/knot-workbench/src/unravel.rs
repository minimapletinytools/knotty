//! Revision-safe document edits and shared source transaction error/budget types.
//! FloatEditor only formats a request for SourceEditor; it has no own executor.
use crate::{
    project::ProjectSnapshot,
    traversal::{Request, SourceEditor},
};
use knot_syntax::span::Span;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use twine::{Limits, Outcome};

#[derive(Debug)]
pub enum EditError {
    Parse(String),
    Check(String),
    Unsupported(String),
    RuleFailed,
    ConflictingRequests,
    ValidationFailed,
    NonFinite,
    Runtime(Outcome),
    SourceTooLarge,
    StaleRevision,
    EditDomainViolation(String),
    RevisionOverflow,
    Internal(String),
}
impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for EditError {}
impl From<Outcome> for EditError {
    fn from(value: Outcome) -> Self {
        Self::Runtime(value)
    }
}

/// An editor-owned document. Replacing text always advances its revision, even
/// if the text later returns to a previous value. No API here writes to disk.
#[derive(Debug)]
pub struct Document {
    identity: u64,
    revision: u64,
    source: String,
    path: PathBuf,
}
impl Document {
    pub fn new(source: impl Into<String>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let identity = NEXT.fetch_add(1, Ordering::Relaxed);
        Self {
            identity,
            revision: 0,
            source: source.into(),
            path: std::env::temp_dir().join(format!(
                "knot-buffer-{}-{identity}.knot",
                std::process::id()
            )),
        }
    }
    /// Give an unsaved buffer a module/import location without writing it.
    pub fn at_path(source: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            ..Self::new(source)
        }
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn replace_source(&mut self, source: String) -> Result<(), EditError> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(EditError::RevisionOverflow)?;
        self.source = source;
        self.revision = revision;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct LiteralPatch {
    pub span: Span,
    pub before: String,
    pub replacement: String,
}
#[derive(Debug, Default, Clone, Copy)]
pub struct EditUsage {
    pub steps: u64,
    /// Cumulative allocation count across proposal and candidate validation.
    pub heap_entries: usize,
}
/// Per-transaction source/compiler retention ceilings, separate from runtime fuel.
/// Increasing these permits more ordinary source work; it does not disable
/// runtime allocation, cancellation or source-size checks.
#[derive(Debug, Clone)]
pub struct TraversalLimits {
    pub source_sites: usize,
    /// Generated-source checks/evaluations, including source-cache lookups.
    /// Direct calls of already prepared code use runtime fuel/heap limits.
    pub queries: usize,
    pub retained_values: usize,
    pub compiled_bytes: usize,
    /// Fresh executable candidates retained within one transaction.
    pub candidate_revisions: usize,
    /// At most two orders: supplied outputs, then their reverse.
    pub solve_attempts: usize,
    /// Cumulative repairs of explicit output locks across all attempts.
    pub lock_repairs: usize,
    /// Cumulative checked executable nodes examined for callback dependencies.
    pub dependency_nodes: usize,
    /// Cumulative candidate nodes, preserving sharing within each candidate.
    pub materialized_values: usize,
}
impl Default for TraversalLimits {
    fn default() -> Self {
        Self {
            source_sites: 256,
            queries: 512,
            retained_values: 4096,
            compiled_bytes: 4 * 1024 * 1024,
            candidate_revisions: 32,
            solve_attempts: 2,
            lock_repairs: 16,
            dependency_nodes: 100_000,
            materialized_values: 10_000,
        }
    }
}
#[derive(Debug, Clone)]
pub struct EditLimits {
    pub runtime: Limits,
    pub traversal: TraversalLimits,
    pub source_bytes: usize,
}
impl Default for EditLimits {
    fn default() -> Self {
        Self {
            runtime: Limits::default(),
            traversal: TraversalLimits::default(),
            source_bytes: 1024 * 1024,
        }
    }
}

/// Only the editor can construct a validated edit. Inspect it, then apply once
/// against the same document/revision. Failure never changes the document.
#[derive(Debug)]
pub struct ValidatedEdit {
    document: u64,
    revision: u64,
    source: String,
    candidate_source: String,
    patches: Vec<LiteralPatch>,
    imports: BTreeMap<PathBuf, String>,
    path: PathBuf,
    usage: EditUsage,
}
impl ValidatedEdit {
    pub fn candidate_source(&self) -> &str {
        &self.candidate_source
    }
    pub fn patches(&self) -> &[LiteralPatch] {
        &self.patches
    }
    pub fn usage(&self) -> EditUsage {
        self.usage
    }
    pub fn apply(self, document: &mut Document) -> Result<UndoEdit, EditError> {
        if document.identity != self.document
            || document.revision != self.revision
            || document.source != self.source
            || !imports_match(&self.imports, &self.path, &self.source)
        {
            return Err(EditError::StaleRevision);
        }
        document.replace_source(self.candidate_source)?;
        Ok(UndoEdit {
            document: document.identity,
            revision: document.revision,
            source: self.source,
            imports: self.imports,
            path: self.path,
        })
    }
}
#[derive(Debug)]
pub struct UndoEdit {
    document: u64,
    revision: u64,
    source: String,
    imports: BTreeMap<PathBuf, String>,
    path: PathBuf,
}
impl UndoEdit {
    pub fn apply(self, document: &mut Document) -> Result<(), EditError> {
        if document.identity != self.document
            || document.revision != self.revision
            || !imports_match(&self.imports, &self.path, &self.source)
        {
            return Err(EditError::StaleRevision);
        }
        document.replace_source(self.source)
    }
}

/// A linear undo history. Only this owner can rebase the next undo token after
/// undoing its successor; arbitrary external revisions still invalidate history.
#[derive(Debug, Default)]
pub struct EditHistory {
    undo: Vec<UndoEdit>,
}
impl EditHistory {
    pub fn apply(&mut self, edit: ValidatedEdit, document: &mut Document) -> Result<(), EditError> {
        self.check(document)?;
        self.undo.push(edit.apply(document)?);
        Ok(())
    }
    pub fn undo(&mut self, document: &mut Document) -> Result<bool, EditError> {
        self.check(document)?;
        let Some(undo) = self.undo.pop() else {
            return Ok(false);
        };
        // Check overflow before consuming history.
        if document.revision == u64::MAX {
            self.undo.push(undo);
            return Err(EditError::RevisionOverflow);
        }
        // check() already validated the top token; keep failed history intact.
        document.replace_source(undo.source)?;
        if let Some(previous) = self.undo.last_mut() {
            previous.revision = document.revision;
        }
        Ok(true)
    }
    fn check(&self, document: &Document) -> Result<(), EditError> {
        if self.undo.last().is_some_and(|last| {
            last.document != document.identity
                || last.revision != document.revision
                || !imports_match(&last.imports, &last.path, &last.source)
        }) {
            Err(EditError::StaleRevision)
        } else {
            Ok(())
        }
    }
    pub fn clear(&mut self) {
        self.undo.clear();
    }
}

fn imports_match(
    imports: &BTreeMap<PathBuf, String>,
    entry: &std::path::Path,
    source: &str,
) -> bool {
    if imports.is_empty() {
        return true;
    }
    if !imports
        .iter()
        .all(|(path, source)| std::fs::read_to_string(path).is_ok_and(|now| now == *source))
    {
        return false;
    }
    // Re-resolve imports as well as comparing bytes: retargeting an import
    // symlink must invalidate a proposal even if the old target still exists.
    crate::modules::ModuleGraph::load_with_source(
        entry,
        &[],
        &Default::default(),
        Some(source.into()),
    )
    .is_ok_and(|graph| {
        graph
            .modules
            .iter()
            .filter(|m| m.path != entry)
            .map(|m| (m.path.clone(), m.source.clone()))
            .collect::<BTreeMap<_, _>>()
            == *imports
    })
}
impl SourceEditor {
    /// Use the shared source routing and policies; apply only changes this buffer.
    pub fn prepare_document(
        &self,
        document: &Document,
        output: &str,
        request: &Request,
        limits: EditLimits,
    ) -> Result<ValidatedEdit, EditError> {
        self.prepare_document_many(
            document,
            &[crate::traversal::OutputRequest { output, request }],
            limits,
        )
    }
    /// Prepare all outputs atomically with one checked candidate and Undo entry.
    pub fn prepare_document_many(
        &self,
        document: &Document,
        requests: &[crate::traversal::OutputRequest<'_>],
        limits: EditLimits,
    ) -> Result<ValidatedEdit, EditError> {
        if limits.runtime.cancellation.load(Ordering::Relaxed) {
            return Err(Outcome::Cancelled.into());
        }
        if document.source.len() > limits.source_bytes {
            return Err(EditError::SourceTooLarge);
        }
        let snapshot = ProjectSnapshot::load(
            &document.path,
            &[],
            &Default::default(),
            Some(document.source.clone()),
            &Default::default(),
        )
        .map_err(EditError::Check)?;
        let edit = self.prepare_many(&snapshot, requests, &[], limits)?;
        Ok(ValidatedEdit {
            document: document.identity,
            revision: document.revision,
            source: document.source.clone(),
            candidate_source: edit.candidate_source().into(),
            path: snapshot.entry_path().into(),
            patches: edit.patches().to_vec(),
            usage: edit.usage(),
            imports: snapshot
                .sources()
                .iter()
                .filter(|(path, _)| path.as_path() != snapshot.entry_path())
                .map(|(path, source)| (path.clone(), source.clone()))
                .collect(),
        })
    }
}
/// Convenience API for `Maybe Float` requests; no independent executor or selector.
pub struct FloatEditor {
    editor: SourceEditor,
}
impl FloatEditor {
    pub fn new() -> Result<Self, EditError> {
        Ok(Self {
            editor: SourceEditor::new()?,
        })
    }
    pub fn prepare(
        &self,
        document: &Document,
        output: &str,
        target: f64,
        limits: EditLimits,
    ) -> Result<ValidatedEdit, EditError> {
        if !target.is_finite() {
            return Err(EditError::NonFinite);
        }
        self.editor.prepare_document(
            document,
            output,
            &Request {
                expression: format!("Just ({target:?})"),
                type_name: "Maybe Float".into(),
            },
            limits,
        )
    }
}
