//! Coordinate editor replacement with independent foreground discovery.
use super::{EditorDraft, Frontend, RecoveredDraft};
use ion_ai::Message;
use ion_host::image_input::ImageSource;

impl Frontend {
    pub(super) fn insert(&mut self, text: &str) {
        if self.pickers.insert(text) {
            self.editor.completion.clear();
        } else {
            self.editor.insert(text);
        }
    }

    pub(super) fn restore_editor(&mut self, source: EditorDraft) {
        self.pickers.close();
        self.editor.restore_editor(source);
    }

    pub(super) fn replace_draft_atomic(&mut self, text: String) {
        self.pickers.close();
        self.editor.replace_draft_atomic(text);
    }

    pub(super) fn return_draft(
        &mut self,
        source: EditorDraft,
        images: Vec<ImageSource>,
        files: Vec<ion_host::prompt_files::TextFile>,
    ) {
        if self.editor.return_draft(source, images, files) {
            self.pickers.close();
        }
    }

    pub(super) fn show_draft(&mut self, draft: RecoveredDraft) {
        self.pickers.close();
        self.editor.show_draft(draft);
    }

    pub(super) fn recover_draft(&mut self) {
        self.pickers.close();
        self.editor.recover_draft();
        self.status = format!(
            "{} recovered draft(s) · {} cycles drafts",
            self.editor.recovered.len(),
            self.keys.hint(crate::keybindings::KeyAction::Dequeue)
        );
    }

    pub(super) fn restore_input(&mut self, input: Message) -> Result<(), &'static str> {
        self.editor.restore_input(input)?;
        self.pickers.close();
        Ok(())
    }
}
