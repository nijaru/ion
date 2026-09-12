use std::fmt;

use ion_ai::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::context::ContextControl;
use crate::{ConversationId, EntryId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: EntryId,
    pub conversation_id: ConversationId,
    pub kind: EntryKind,
    pub data: Value,
    pub projection: Vec<Message>,
    pub context: ContextControl,
}

impl Entry {
    #[must_use]
    pub fn new(
        id: EntryId,
        conversation_id: ConversationId,
        kind: EntryKind,
        data: Value,
        projection: Vec<Message>,
        context: ContextControl,
    ) -> Self {
        Self {
            id,
            conversation_id,
            kind,
            data,
            projection,
            context,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
}

impl fmt::Display for EntryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("entry kind cannot be empty")]
pub struct EntryKindError;
