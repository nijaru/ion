//! Bounded visible child output, distinct from a successful committed answer.
use serde::{Deserialize, Serialize};

pub const MAX_DELEGATION_PARTIAL_BYTES: usize = 50 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationPartialOutput {
    pub text: String,
    /// The retained text is a UTF-8 prefix, not the whole response.
    pub truncated: bool,
    /// Progress was lost; text must not be joined across the missing prefix.
    pub observation_gap: bool,
}

impl DelegationPartialOutput {
    /// Append a contiguous visible-text delta without retaining more than 50 KiB.
    /// Once truncated, later deltas cannot fill a hole in the retained prefix.
    pub fn push_text(&mut self, text: &str) {
        if self.truncated {
            return;
        }
        self.insert_text(self.text.len(), text);
    }

    /// Insert observed text at a retained UTF-8 boundary, displacing the suffix
    /// when necessary to keep a bounded prefix. Earlier block updates can still
    /// change that prefix after truncation, but appends cannot fill a missing
    /// suffix. Returns the inserted byte count.
    pub fn insert_text(&mut self, position: usize, text: &str) -> usize {
        assert!(self.text.is_char_boundary(position));
        if self.truncated && position == self.text.len() {
            return 0;
        }
        let end = text.floor_char_boundary(
            MAX_DELEGATION_PARTIAL_BYTES
                .saturating_sub(position)
                .min(text.len()),
        );
        // Never join the old suffix across the insertion's missing tail.
        let retained = if end < text.len() {
            position
        } else {
            self.text.floor_char_boundary(
                MAX_DELEGATION_PARTIAL_BYTES
                    .saturating_sub(end)
                    .min(self.text.len()),
            )
        };
        self.truncated |= end < text.len() || retained < self.text.len();
        self.text.truncate(retained);
        self.text.insert_str(position, &text[..end]);
        end
    }

    pub(crate) fn valid(&self) -> bool {
        self.text.len() <= MAX_DELEGATION_PARTIAL_BYTES
    }
}
