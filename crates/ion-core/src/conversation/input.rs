use std::fmt;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ConversationId, EntryId, InputId, TaskId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub id: InputId,
    pub target: ConversationId,
    pub sender: InputSender,
    pub mode: InputMode,
    pub request_key: Option<RequestKey>,
    pub body: InputBody,
    pub disposition: InputDisposition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputSender {
    User,
    Host,
    Conversation(ConversationId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputMode {
    Submit,
    Steer,
    FollowUp,
    QueueOnly,
    Notice,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputBody {
    Text(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputDisposition {
    Queued,
    Assigned(TaskId),
    Consumed(EntryId),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestKey(String);

impl RequestKey {
    pub fn new(value: impl Into<String>) -> Result<Self, RequestKeyError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RequestKeyError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("request key cannot be empty")]
pub struct RequestKeyError;
