//! Literal editor state, local recovery, completion and visual navigation.
//! Host input custody starts only when a draft is handed off.
use std::collections::VecDeque;

use super::editor_recovery::EditorRecovery;
use crate::{terminal_completion::Completion, terminal_input::EditorVisualMap};
use ion_ai::{Content, Message};
use ion_host::image_input::{ImageSource, LoadedImage};

#[derive(Default)]
pub(super) struct Editor {
    pub(super) draft: String,
    pub(super) cursor: usize,
    pub(super) images: Vec<ImageSource>,
    pub(super) files: Vec<ion_host::prompt_files::TextFile>,
    pub(super) completion: Completion,
    pub(super) recovered: VecDeque<RecoveredDraft>,
    pub(super) prompt_history: Vec<String>,
    pub(super) browsing: Option<PromptBrowse>,
    pub(super) recovery: EditorRecovery,
    pub(super) editor_width: Option<usize>,
    pub(super) preferred_column: Option<usize>,
    pub(super) error: Option<Limit>,
}

#[derive(Clone, Copy)]
pub(super) enum Limit {
    Prompt,
    History,
}

impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Prompt => write!(f, "Prompt is limited to {} bytes", super::MAX_DRAFT),
            Self::History => {
                f.write_str("History input exceeds editor limit; current draft retained")
            }
        }
    }
}

pub(super) struct PromptBrowse {
    pub(super) index: usize,
    pub(super) end: usize,
    pub(super) draft: String,
    pub(super) cursor: usize,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct EditorDraft {
    pub(super) literal: String,
    pub(super) cursor: usize,
}

impl EditorDraft {
    pub(super) fn at_end(literal: String) -> Self {
        Self {
            cursor: literal.len(),
            literal,
        }
    }
}

// Returned inputs are UI recovery only, never an execution queue. Each item
// retains its own literal editor text, cursor and attachment associations.
pub(super) struct RecoveredDraft {
    pub(super) source: EditorDraft,
    pub(super) images: Vec<ImageSource>,
    pub(super) files: Vec<ion_host::prompt_files::TextFile>,
}

impl Editor {
    pub(super) fn apply(&mut self, action: crate::keybindings::KeyAction) -> bool {
        let Some(edit) = super::editing::apply(&self.draft, self.cursor, action) else {
            return false;
        };
        match edit {
            super::editing::Edit::Move(cursor) => {
                self.break_edit_focus();
                self.cursor = cursor;
            }
            super::editing::Edit::Delete { range, kill } => self.delete_text(range, kill),
        }
        true
    }

    pub(super) fn attach_image(&mut self, image: ImageSource) {
        self.reset_edit_baseline();
        self.images.push(image);
    }

    pub(super) fn take_attachments(
        &mut self,
    ) -> (Vec<ImageSource>, Vec<ion_host::prompt_files::TextFile>) {
        (
            std::mem::take(&mut self.images),
            std::mem::take(&mut self.files),
        )
    }

    pub(super) fn publish_editor_width(&mut self, width: usize) {
        if self.editor_width != Some(width) {
            self.preferred_column = None;
        }
        self.editor_width = Some(width);
    }

    pub(super) fn invalidate_geometry(&mut self) {
        self.editor_width = None;
        self.preferred_column = None;
    }

    pub(super) fn move_vertical(&mut self, down: bool) {
        let Some(width) = self.editor_width else {
            return;
        };
        let map = EditorVisualMap::new(&self.draft, width);
        if let Some(cursor) = map.movement(self.cursor, down, &mut self.preferred_column) {
            self.cursor = cursor;
        } else if down {
            self.history_next();
        } else {
            self.history_previous();
        }
    }

    pub(super) fn take_literal(&mut self) -> EditorDraft {
        self.reset_edit_baseline();
        self.completion.clear();
        self.browsing = None;
        let cursor = std::mem::take(&mut self.cursor);
        EditorDraft {
            literal: std::mem::take(&mut self.draft),
            cursor,
        }
    }
}

// Inverse of Message::user_input, not a lossy flattening of arbitrary multipart
// messages from embedded clients. Notes belong to the immediately following image.
pub(super) fn split_input(input: Message) -> Result<(String, Vec<LoadedImage>), &'static str> {
    if input.role != ion_ai::Role::User || input.provider_replay.is_some() {
        return Err("selected input cannot be represented in the editor");
    }
    let mut parts = input.content.into_iter();
    let Some(Content::Text(prompt)) = parts.next() else {
        return Err("selected input cannot be represented in the editor");
    };
    let mut images = Vec::new();
    while let Some(part) = parts.next() {
        match part {
            Content::Image(content) => images.push(LoadedImage {
                content,
                note: None,
            }),
            Content::Text(note) => {
                let Some(Content::Image(content)) = parts.next() else {
                    return Err("selected input cannot be represented in the editor");
                };
                images.push(LoadedImage {
                    content,
                    note: Some(note),
                });
            }
            Content::Thinking(_) | Content::ToolCall(_) | Content::ToolResult(_) => {
                return Err("selected input cannot be represented in the editor");
            }
        }
    }
    Ok((prompt, images))
}
