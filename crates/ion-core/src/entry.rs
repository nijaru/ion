//! Immutable transcript entries.
//!
//! An entry is appended once and never edited. Its `data` is the durable
//! record of what happened; its `projection` is the exact provider message the
//! entry contributes to a later request. Keeping both means a request can be
//! reassembled without re-deriving meaning from the record, and a record can
//! stay readable when its projection shape changes.

use std::fmt;

use ion_ai::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{ConversationId, EntryId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: EntryId,
    pub conversation_id: ConversationId,
    pub kind: EntryKind,
    pub data: Value,
    pub projection: Vec<Message>,
}

impl Entry {
    #[must_use]
    pub fn new(
        id: EntryId,
        conversation_id: ConversationId,
        kind: EntryKind,
        data: Value,
        projection: Vec<Message>,
    ) -> Self {
        Self {
            id,
            conversation_id,
            kind,
            data,
            projection,
        }
    }
}

/// The entry kind an accepted input is placed under.
pub const INPUT_ENTRY: &str = "user";
/// The entry kind a settled assistant response is placed under.
pub const ASSISTANT_ENTRY: &str = "assistant";
/// The entry kind one settled tool invocation is placed under.
pub const TOOL_ENTRY: &str = "tool";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct EntryKind(String);

impl EntryKind {
    pub fn new(value: impl Into<String>) -> Result<Self, EntryKindError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(EntryKindError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The built-in kind for a known transcript role.
    #[must_use]
    pub fn builtin(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl TryFrom<String> for EntryKind {
    type Error = EntryKindError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<EntryKind> for String {
    fn from(value: EntryKind) -> Self {
        value.0
    }
}

impl fmt::Display for EntryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("entry kind cannot be empty")]
pub struct EntryKindError;
