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
        let end = text.floor_char_boundary(
            MAX_DELEGATION_PARTIAL_BYTES
                .saturating_sub(self.text.len())
                .min(text.len()),
        );
        self.text.push_str(&text[..end]);
        self.truncated = end < text.len();
    }

    pub(crate) fn valid(&self) -> bool {
        self.text.len() <= MAX_DELEGATION_PARTIAL_BYTES
    }
}
