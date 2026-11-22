//! Reuse checked, linked baseline operations across picks on one gesture.
//! The image owns code only. Each pick starts a new session and supplies its
//! request and all retained arguments through that session's typed handles.
use super::*;
use crate::operation::PreparedOperations;

const MAX_OPERATIONS: usize = 128;
const MAX_BYTES: usize = 256 * 1024;

#[derive(Default)]
pub(in crate::traversal::probe) struct Recording {
    operations: HashMap<String, (String, Option<String>)>,
    bytes: usize,
}

pub(in crate::traversal::probe) struct Image {
    prepared: PreparedOperations,
    operations: HashMap<String, (String, Option<String>)>,
    modules: usize,
}

impl Probes {
    pub(in crate::traversal) fn enable_route(&mut self) -> Result<(), EditError> {
        // Tight caller budgets use the ordinary demand-driven path. Starting
        // the image introduces all its export thunks at once.
        let defaults = EditLimits::default();
        if self.limits.runtime.heap_entries < defaults.runtime.heap_entries
            || self.limits.runtime.steps < defaults.runtime.steps
            || self.limits.traversal.compiled_bytes < defaults.traversal.compiled_bytes
        {
            return Ok(());
        }
        if let Some(image) = self.context.route.get() {
            let prepared = &image.prepared;
            let started = Instant::now();
            let session = prepared
                .start(self.limits.runtime.clone())
                .map_err(operation_error)?;
            self.link_ms += started.elapsed().as_secs_f64() * 1000.;
            self.live = Some(Live::new(prepared.clone(), session, image.modules, 0));
            self.route_image = Some(image.clone());
        } else {
            self.recording = Some(Recording::default());
        }
        Ok(())
    }

    pub(super) fn record_operation(
        &mut self,
        key: String,
        label: &str,
        normalized: Option<&String>,
        source_bytes: usize,
    ) {
        let Some(recording) = &mut self.recording else {
            return;
        };
        let live = self
            .live
            .as_ref()
            .expect("recording an extension to the live program");
        let bytes = key.len() + label.len() + normalized.map_or(0, String::len);
        if recording.bytes.saturating_add(bytes) > MAX_BYTES
            || live.source_bytes.saturating_add(source_bytes) > MAX_BYTES
            || live.sequence >= MAX_OPERATIONS
        {
            // Freeze the checked prefix before installing the first operation
            // that would exceed its budget. Remaining calls link on demand.
            self.finish_recording();
            return;
        }
        recording.bytes += bytes;
        recording
            .operations
            .insert(key, (label.into(), normalized.cloned()));
    }

    // Freeze only the first baseline, before any candidate code is installed.
    pub(super) fn finish_recording(&mut self) {
        let Some(recording) = self.recording.take() else {
            return;
        };
        let Some(live) = self.live.as_ref() else {
            return;
        };
        if live.sequence > MAX_OPERATIONS
            || live.source_bytes > MAX_BYTES
            || recording.operations.is_empty()
        {
            return;
        }
        self.pending_route = Some(Arc::new(Image {
            prepared: live.prepared.clone(),
            operations: recording.operations,
            modules: live.sequence,
        }));
    }

    pub(in crate::traversal) fn publish_route(&mut self) {
        if let Some(image) = self.pending_route.take() {
            let _ = self.original_context.route.set(image);
        }
    }

    pub(super) fn route_operation(&self, key: &str) -> Option<(String, Option<String>)> {
        self.route_image.as_ref()?.operations.get(key).cloned()
    }
}
