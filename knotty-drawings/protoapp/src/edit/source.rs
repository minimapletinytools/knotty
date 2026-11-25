//! Text edits from the source pane share the drawing's publication and Undo path.
//! Invalid source is reported and never written, so the drawing keeps its last good render.
use super::*;

#[derive(serde::Deserialize)]
pub struct SourceText {
    snapshot: String,
    source: String,
    /// Continue the previous source-pane undo step instead of starting one,
    /// so a burst of typing reverts together.
    #[serde(default)]
    coalesce: bool,
}

impl Editor {
    pub fn save_text(&mut self, drawing: &mut Drawing, edit: SourceText) -> Value {
        let result = (|| -> Result<(), String> {
            if self.preview {
                return Err("Preview mode: edits are not applied.".into());
            }
            if self.active_preview.is_some() {
                return Err("Finish the property editor with OK or Cancel first.".into());
            }
            drawing.refresh();
            let render = drawing.check_snapshot(&edit.snapshot)?;
            let before = render.snapshot.sources()[render.snapshot.entry_path()].clone();
            if before == edit.source {
                return Ok(());
            }
            let previous = drawing.revision();
            drawing.save_source(&edit.snapshot, &edit.source)?;
            let current = drawing.revision();
            match self.undo.last_mut() {
                Some((revision, _))
                    if edit.coalesce
                        && *revision == previous
                        && self.text_revision == Some(previous) =>
                {
                    *revision = current
                }
                _ => self.record_undo(previous, current, before),
            }
            self.text_revision = Some(current);
            Ok(())
        })();
        match result {
            Ok(()) => json!({"ok":true}),
            Err(reason) => json!({"ok":false,"reason":reason}),
        }
    }
}
