//! Coordinate editor replacement with independent foreground discovery.
use super::{
    EditorDraft, Frontend, RecoveredDraft,
    interaction::InputTarget,
    picker::{Picker, PickerItem, PickerValue},
};
use ion_ai::Message;
use ion_app::attachments::AttachmentSource;

impl Frontend {
    pub(super) fn insert(&mut self, text: &str) {
        self.gestures.invalidate();
        if self.pickers.insert(text) {
            self.editor.completion.clear();
        } else {
            self.editor.insert(text);
        }
    }

    pub(super) fn restore_editor(&mut self, source: EditorDraft) {
        self.gestures.invalidate();
        self.pickers.close();
        self.editor.restore_editor(source);
    }

    pub(super) fn replace_draft_atomic(&mut self, text: String) {
        self.gestures.invalidate();
        self.pickers.close();
        self.editor.replace_draft_atomic(text);
    }

    pub(super) fn return_command_source(&mut self, source: EditorDraft) {
        self.gestures.invalidate();
        let replace_empty =
            !self.clipboard_pending() && self.input_target() == InputTarget::Composer;
        if self.editor.return_text(source, replace_empty) {
            self.pickers.close();
        }
        self.note(format!(
            "Command text retained; {} additional draft(s) · {} cycles without resubmitting",
            self.editor.recovered.len(),
            self.keys.hint(crate::keybindings::KeyAction::Dequeue)
        ));
    }

    pub(super) fn return_draft(&mut self, source: EditorDraft, attachments: Vec<AttachmentSource>) {
        self.gestures.invalidate();
        // Automatic returns cannot steal modal focus or become the prefix and
        // attachment owner of a pending clipboard paste.
        let replace_empty =
            !self.clipboard_pending() && self.input_target() == InputTarget::Composer;
        if self.editor.return_draft(source, attachments, replace_empty) {
            self.pickers.close();
        }
    }

    pub(super) fn show_draft(&mut self, draft: RecoveredDraft) {
        self.gestures.invalidate();
        self.pickers.close();
        self.editor.show_draft(draft);
    }

    pub(super) fn recover_draft(&mut self) {
        if self.editor.recover_draft() {
            self.gestures.invalidate();
            self.pickers.close();
        }
    }

    pub(super) fn open_attachments(&mut self) {
        if self.editor.attachments.is_empty() {
            self.status = "No attachments on this draft".into();
            return;
        }
        self.gestures.invalidate();
        self.editor.break_edit_focus();
        self.editor.completion.clear();
        let items = self
            .editor
            .attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| {
                let description = match attachment {
                    AttachmentSource::File(path) | AttachmentSource::ImageFile(path) => path
                        .strip_prefix(&self.cwd)
                        .unwrap_or(path)
                        .display()
                        .to_string(),
                    AttachmentSource::Text(file) => file.name.clone(),
                    AttachmentSource::Image(image) => {
                        format!("Image · {:?}", image.content.mime_type())
                    }
                    AttachmentSource::ImageBytes { bytes, .. } => {
                        format!("Image · {} bytes", bytes.len())
                    }
                };
                PickerItem {
                    label: format!("{} · {description}", index + 1),
                    detail: None,
                    value: PickerValue::RemoveAttachment {
                        index,
                        baseline: self.editor.baseline,
                    },
                }
            })
            .collect();
        self.pickers.open(Picker {
            title: "Remove draft attachment",
            query: String::new(),
            selected: 0,
            items,
        });
    }

    pub(super) fn remove_attachment(&mut self, index: usize, baseline: u64) {
        if self.editor.baseline != baseline || index >= self.editor.attachments.len() {
            self.status = "Draft attachments changed; reopen the picker".into();
            return;
        }
        // As with adding an attachment, an earlier whole-draft undo must
        // never swap in an attachment set from a different baseline.
        self.editor.reset_edit_baseline();
        self.editor.attachments.remove(index);
        self.gestures.invalidate();
        self.status = "Attachment removed".into();
    }

    pub(super) fn restore_input(&mut self, input: Message) -> Result<(), &'static str> {
        self.gestures.invalidate();
        self.editor.restore_input(input)?;
        self.pickers.close();
        Ok(())
    }
}
