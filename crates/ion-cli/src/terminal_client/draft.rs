//! Editor replacement, history browsing and returned-input custody.
//!
//! A replacement ends the previous draft's completion episode. In-flight file
//! discovery remains owned by Pickers until joined, but cannot publish into the
//! replacement draft.
use ion_ai::Message;
use ion_app::attachments::AttachmentSource;

use super::{
    MAX_DRAFT,
    editor::{Editor, EditorDraft, PromptBrowse, RecoveredDraft},
};

fn recallable(prompt: &str) -> bool {
    !prompt.is_empty() && prompt.len() <= MAX_DRAFT
}

impl Editor {
    /// The single publication point for a different editor draft. History may
    /// retain its browsing position, but completion ranges never cross drafts.
    fn set_editor(&mut self, source: EditorDraft) {
        self.completion.clear();
        self.draft = source.literal;
        self.cursor = source.cursor;
    }

    fn set_editor_baseline(&mut self, source: EditorDraft) {
        self.reset_edit_baseline();
        self.set_editor(source);
    }

    pub(super) fn restore_editor(&mut self, source: EditorDraft) {
        self.retain_browsed_draft();
        self.set_editor_baseline(source);
    }

    pub(super) fn replace_prompt_history(&mut self, mut prompts: Vec<String>, same_session: bool) {
        // History navigation is for editable drafts. Oversized prompts stay
        // in the Session, but must not occupy invisible Up/Down steps.
        prompts.retain(|prompt| recallable(prompt));
        // Like Pi, collapse consecutive duplicates in arrow history. Keep
        // Ion's exact text: whitespace can be significant in a prompt.
        prompts.dedup();
        // A browse owns its original bounded window. New commits can evict
        // old prompts without moving the selection or the displaced draft.
        if !same_session {
            self.restore_browsed_draft();
        }
        self.prompt_history = prompts.into();
    }

    /// Editing or submitting a recalled prompt replaces the visible draft,
    /// not the unsent source that history browsing temporarily displaced.
    pub(super) fn retain_browsed_draft(&mut self) {
        if let Some(browse) = self.browsing.take()
            && !browse.draft.is_empty()
        {
            self.recovered.push_back(RecoveredDraft {
                source: EditorDraft {
                    literal: browse.draft,
                    cursor: browse.cursor,
                },
                attachments: Vec::new(),
            });
        }
    }

    pub(super) fn restore_browsed_draft(&mut self) -> bool {
        let Some(browse) = self.browsing.take() else {
            return false;
        };
        self.set_editor_baseline(EditorDraft {
            literal: browse.draft,
            cursor: browse.cursor,
        });
        true
    }

    pub(super) fn history_previous(&mut self) {
        // Arrow recall loads only historical text. An attachment belongs
        // to the current draft, not to a different prompt reached by Up.
        if !self.attachments.is_empty()
            || (self.browsing.is_none() && self.prompt_history.is_empty())
        {
            return;
        }
        let browse = self.browsing.get_or_insert_with(|| PromptBrowse {
            index: self.prompt_history.len(),
            window: self.prompt_history.clone(),
            draft: self.draft.clone(),
            cursor: self.cursor,
        });
        let Some(index) = browse.index.checked_sub(1) else {
            return;
        };
        browse.index = index;
        let text = browse.window[index].clone();
        self.set_editor_baseline(EditorDraft::at_end(text));
    }

    pub(super) fn history_next(&mut self) {
        if !self.attachments.is_empty() {
            return;
        }
        let Some(browse) = &mut self.browsing else {
            return;
        };
        if browse.index + 1 < browse.window.len() {
            browse.index += 1;
            let text = browse.window[browse.index].clone();
            self.set_editor_baseline(EditorDraft::at_end(text));
        } else {
            self.restore_browsed_draft();
        }
    }

    /// Direct-command metadata owns the vacated text slot, never attachments.
    pub(super) fn return_text(&mut self, source: EditorDraft, replace_empty: bool) -> bool {
        let restored = replace_empty && self.restore_browsed_draft();
        if replace_empty && self.draft.is_empty() {
            self.restore_editor(source);
            true
        } else {
            self.recovered.push_back(RecoveredDraft {
                source,
                attachments: Vec::new(),
            });
            restored
        }
    }

    pub(super) fn return_draft(
        &mut self,
        source: EditorDraft,
        attachments: Vec<AttachmentSource>,
        replace_empty: bool,
    ) -> bool {
        let restored = replace_empty && self.restore_browsed_draft();
        if replace_empty && self.draft.is_empty() && self.attachments.is_empty() {
            self.restore_editor(source);
            self.attachments = attachments;
            true
        } else {
            self.recovered.push_back(RecoveredDraft {
                source,
                attachments,
            });
            restored
        }
    }

    pub(super) fn show_draft(&mut self, recovered: RecoveredDraft) {
        self.restore_browsed_draft();
        if !self.draft.is_empty() || !self.attachments.is_empty() {
            self.recovered.push_back(RecoveredDraft {
                source: EditorDraft {
                    literal: std::mem::take(&mut self.draft),
                    cursor: self.cursor,
                },
                attachments: std::mem::take(&mut self.attachments),
            });
        }
        self.restore_editor(recovered.source);
        self.attachments = recovered.attachments;
    }

    pub(super) fn recover_draft(&mut self) -> bool {
        let Some(recovered) = self.recovered.pop_front() else {
            return false;
        };
        self.show_draft(recovered);
        true
    }

    pub(super) fn restore_input(&mut self, input: Message) -> Result<(), &'static str> {
        let (draft, images) = super::editor::split_input(input)?;
        if draft.len() > MAX_DRAFT {
            return Err("selected input exceeds editor limit");
        }
        self.show_draft(RecoveredDraft {
            source: EditorDraft::at_end(draft),
            attachments: images.into_iter().map(Into::into).collect(),
        });
        Ok(())
    }
}
