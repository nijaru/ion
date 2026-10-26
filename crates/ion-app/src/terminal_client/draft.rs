//! Editor replacement, history browsing and returned-input custody.
//!
//! A replacement ends the previous draft's completion episode. In-flight file
//! discovery remains owned by Pickers until joined, but cannot publish into the
//! replacement draft.
use ion_ai::Message;
use ion_host::image_input::ImageSource;

use super::{EditorDraft, Frontend, MAX_DRAFT, PromptBrowse, RecoveredDraft};

impl Frontend {
    /// The single publication point for a different editor draft. History may
    /// retain its browsing position, but completion ranges never cross drafts.
    fn set_editor(&mut self, source: EditorDraft) {
        self.pickers.close();
        self.completion.clear();
        self.draft = source.literal;
        self.cursor = source.cursor;
    }

    fn set_editor_baseline(&mut self, source: EditorDraft) {
        self.reset_edit_baseline();
        self.set_editor(source);
    }

    pub(super) fn restore_editor(&mut self, source: EditorDraft) {
        self.browsing = None;
        self.set_editor_baseline(source);
    }

    pub(super) fn replace_prompt_history(&mut self, prompts: Vec<String>, same_session: bool) {
        if self.browsing.as_ref().is_some_and(|browse| {
            !same_session || !prompts.starts_with(&self.prompt_history[..browse.end])
        }) {
            self.restore_browsed_draft();
        }
        self.prompt_history = prompts;
    }

    pub(super) fn restore_browsed_draft(&mut self) {
        if let Some(browse) = self.browsing.take() {
            self.set_editor_baseline(EditorDraft {
                literal: browse.draft,
                cursor: browse.cursor,
            });
        }
    }

    pub(super) fn history_previous(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }
        let browse = self.browsing.get_or_insert_with(|| PromptBrowse {
            index: self.prompt_history.len(),
            end: self.prompt_history.len(),
            draft: self.draft.clone(),
            cursor: self.cursor,
        });
        browse.index = browse.index.saturating_sub(1);
        let index = browse.index;
        self.recall_history_entry(index);
    }

    pub(super) fn history_next(&mut self) {
        let Some(browse) = &mut self.browsing else {
            return;
        };
        if browse.index + 1 < browse.end {
            browse.index += 1;
            let index = browse.index;
            self.recall_history_entry(index);
        } else {
            self.restore_browsed_draft();
        }
    }

    fn recall_history_entry(&mut self, index: usize) {
        let draft = &self.prompt_history[index];
        if draft.len() > MAX_DRAFT {
            self.status = "History input exceeds editor limit; current draft retained".into();
            return;
        }
        let draft = draft.clone();
        self.set_editor_baseline(EditorDraft::at_end(draft));
        self.status.clear();
    }

    pub(super) fn return_draft(
        &mut self,
        source: EditorDraft,
        images: Vec<ImageSource>,
        files: Vec<ion_host::prompt_files::TextFile>,
    ) {
        self.restore_browsed_draft();
        if self.draft.is_empty() && self.images.is_empty() && self.files.is_empty() {
            self.restore_editor(source);
            self.images = images;
            self.files = files;
        } else {
            self.recovered.push_back(RecoveredDraft {
                source,
                images,
                files,
            });
        }
    }

    pub(super) fn show_draft(&mut self, recovered: RecoveredDraft) {
        self.restore_browsed_draft();
        if !self.draft.is_empty() || !self.images.is_empty() || !self.files.is_empty() {
            self.recovered.push_back(RecoveredDraft {
                source: EditorDraft {
                    literal: std::mem::take(&mut self.draft),
                    cursor: self.cursor,
                },
                images: std::mem::take(&mut self.images),
                files: std::mem::take(&mut self.files),
            });
        }
        self.restore_editor(recovered.source);
        self.images = recovered.images;
        self.files = recovered.files;
    }

    pub(super) fn recover_draft(&mut self) {
        self.restore_browsed_draft();
        if let Some(recovered) = self.recovered.pop_front() {
            self.show_draft(recovered);
        }
        self.status = format!(
            "{} recovered draft(s) · {} cycles drafts",
            self.recovered.len(),
            self.keys.hint(crate::keybindings::KeyAction::Dequeue)
        );
    }

    pub(super) fn restore_input(&mut self, input: Message) -> Result<(), &'static str> {
        let (draft, images) = super::split_editor_input(input)?;
        if draft.len() > MAX_DRAFT {
            return Err("selected input exceeds editor limit");
        }
        self.show_draft(RecoveredDraft {
            source: EditorDraft::at_end(draft),
            images: images.into_iter().map(Into::into).collect(),
            files: Vec::new(),
        });
        Ok(())
    }
}
