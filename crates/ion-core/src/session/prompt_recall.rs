//! Bounded editor recall across all branches, independent of display ancestry.
use std::collections::VecDeque;

use super::*;

const RECORDS: usize = 96;
const BYTES: usize = 256 * 1024;

/// Maximum UTF-8 text length of an input retained for editor recall.
pub const MAX_RECALL_PROMPT_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct PromptRecall {
    texts: VecDeque<String>,
    bytes: usize,
}

impl PromptRecall {
    // Observe validated records during replay or after durable commit. Retain
    // only the editable text, not messages, attachments or provider payloads.
    pub(super) fn observe(&mut self, entry: &SessionEntry) {
        let Some(text) = prompt(entry) else { return };
        if text.is_empty()
            || text.len() > MAX_RECALL_PROMPT_BYTES
            || self.texts.back().is_some_and(|previous| previous == text)
        {
            return;
        }
        while self.texts.len() == RECORDS || self.bytes + text.len() > BYTES {
            self.bytes -= self
                .texts
                .pop_front()
                .expect("recall budget has an oldest prompt")
                .len();
        }
        self.texts.push_back(text.to_owned());
        self.bytes += text.len();
    }

    pub(super) fn snapshot(&self) -> Vec<String> {
        self.texts.iter().cloned().collect()
    }
}

fn prompt(entry: &SessionEntry) -> Option<&str> {
    let input = match entry {
        SessionEntry::TurnStarted {
            input, completion, ..
        } if completion.is_empty() => input,
        SessionEntry::Steering { input, .. } => input,
        _ => return None,
    };
    input.content.iter().find_map(|part| match part {
        Content::Text(text) => Some(text.as_str()),
        _ => None,
    })
}
