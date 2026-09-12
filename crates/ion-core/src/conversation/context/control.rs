use ion_ai::Message;
use serde::{Deserialize, Serialize};

use crate::EntryId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ContextControl {
    pub head: Option<EntryId>,
    pub edits: Vec<ContextEdit>,
}

impl ContextControl {
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn head(head: EntryId) -> Self {
        Self {
            head: Some(head),
            edits: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContextEdit {
    Omit {
        target: EntryId,
    },
    Replace {
        target: EntryId,
        messages: Vec<Message>,
    },
}

impl ContextEdit {
    #[must_use]
    pub const fn target(&self) -> EntryId {
        match self {
            Self::Omit { target } | Self::Replace { target, .. } => *target,
        }
    }
}
