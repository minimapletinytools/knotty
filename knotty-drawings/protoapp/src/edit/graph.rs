//! Direct edits to the drawing's source graph share the drawing's publication and Undo path.
use super::*;
use knot_workbench::tangle::{candidate, Change};

#[derive(serde::Deserialize)]
pub struct GraphEdit {
    snapshot: String,
    definition: String,
    changes: Vec<Change>,
}

impl Editor {
    pub fn can_undo(&self, drawing: &Drawing) -> bool {
        self.active_preview.is_none()
            && self
                .undo
                .last()
                .is_some_and(|(revision, _)| *revision == drawing.revision())
    }

    pub fn save_graph(&mut self, drawing: &mut Drawing, edit: GraphEdit) -> Result<(), String> {
        if self.preview {
            return Err("Preview mode: edits are not applied.".into());
        }
        if self.active_preview.is_some() {
            return Err("Finish the drawing preview before editing its source graph.".into());
        }
        drawing.refresh();
        let snapshot = &drawing.check_snapshot(&edit.snapshot)?.snapshot;
        let next = candidate(snapshot, &edit.definition, &edit.changes)?;
        let original = &snapshot.sources()[snapshot.entry_path()];
        if &next != original {
            let before = original.clone();
            let previous = drawing.revision();
            drawing.save_source(&edit.snapshot, &next)?;
            self.record_undo(previous, drawing.revision(), before);
        }
        Ok(())
    }
}
