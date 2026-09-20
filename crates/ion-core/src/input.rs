//! Durable accepted input, distinct from transcript placement.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{ConversationId, EntryId, InputId, InvocationId, TurnId};

const MAX_REQUEST_KEY_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestKey(String);

impl RequestKey {
    pub fn new(value: impl Into<String>) -> Result<Self, RequestKeyError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RequestKeyError::Empty);
        }
        if value.len() > MAX_REQUEST_KEY_BYTES {
            return Err(RequestKeyError::TooLong {
                length: value.len(),
                maximum: MAX_REQUEST_KEY_BYTES,
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputSender {
    User,
    Conversation(ConversationId),
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputMode {
    Submit,
    FollowUp,
    Steer,
    InteractionReply,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputBody {
    Text(String),
    InteractionReply {
        invocation: InvocationId,
        answer: Value,
    },
}

impl InputBody {
    #[must_use]
    pub fn interaction_target(&self) -> Option<InvocationId> {
        match self {
            Self::Text(_) => None,
            Self::InteractionReply { invocation, .. } => Some(*invocation),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputDisposition {
    Queued,
    /// Consumed into one Turn. Ordinary text has an Entry; targeted replies are
    /// consumed through their canonical tool-result exchange and therefore do not.
    Consumed {
        turn: TurnId,
        entry: Option<EntryId>,
    },
    Cancelled,
    Abandoned {
        turn: Option<TurnId>,
        entry: Option<EntryId>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub id: InputId,
    pub conversation: ConversationId,
    pub sender: InputSender,
    pub mode: InputMode,
    pub request_key: Option<RequestKey>,
    pub body: InputBody,
    pub disposition: InputDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RequestKeyError {
    #[error("request key must not be empty")]
    Empty,
    #[error("request key is {length} bytes; maximum is {maximum}")]
    TooLong { length: usize, maximum: usize },
}
