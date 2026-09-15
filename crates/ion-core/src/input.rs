//! Accepted input: admission, placement and disposition.
//!
//! Admission, transcript placement, inclusion in a model request and
//! settlement are four different facts. Collapsing them loses the case a
//! client most needs to see: the host accepted a message, the turn that was
//! going to answer it was cancelled, and the message is still in the history
//! rather than silently discarded.

use std::fmt;

use ion_ai::{Content, Message, Role};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{ConversationId, EntryId, EntryKind, INPUT_ENTRY, InputId, TurnId};

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

/// How an accepted input asks to be scheduled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputMode {
    /// Start a turn now; refuse when the conversation is already busy.
    Submit,
    /// Join the next safe request boundary of the running turn.
    Steer,
    /// Queue behind the running turn and answer as a successor turn.
    FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputBody {
    Text(String),
}

impl InputBody {
    /// The transcript content this body is placed as.
    #[must_use]
    pub fn placement(&self) -> EntryPlacement {
        match self {
            Self::Text(text) => EntryPlacement {
                kind: EntryKind::builtin(INPUT_ENTRY),
                data: json!({"text": text}),
                projection: vec![Message {
                    role: Role::User,
                    content: vec![Content::Text(text.clone())],
                    provider_replay: None,
                }],
            },
        }
    }

    /// The durable byte size this body costs.
    #[must_use]
    pub fn size(&self) -> u64 {
        serde_json::to_vec(self).map_or(0, |bytes| bytes.len() as u64)
    }
}

/// The transcript content an accepted input is placed as.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryPlacement {
    pub kind: EntryKind,
    pub data: Value,
    pub projection: Vec<Message>,
}

/// Where an accepted input is in the transcript, and which turn answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputPlacement {
    pub entry: EntryId,
    pub turn: TurnId,
}

/// How far an accepted input has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputDisposition {
    /// Admitted and waiting: either for the running turn's next request
    /// boundary (steering) or for a successor turn (follow-up).
    Queued,
    /// Placed in the transcript and bound to the turn answering it.
    Placed(InputPlacement),
    /// Placed, then explicitly given up on. The entry stays; no further
    /// attempt will answer it.
    Abandoned(InputPlacement),
    /// Withdrawn before it was ever placed, so there is no entry to preserve.
    Cancelled,
}

impl InputDisposition {
    #[must_use]
    pub const fn answering_turn(self) -> Option<TurnId> {
        match self {
            Self::Placed(placement) | Self::Abandoned(placement) => Some(placement.turn),
            Self::Queued | Self::Cancelled => None,
        }
    }

    #[must_use]
    pub const fn placement(self) -> Option<InputPlacement> {
        match self {
            Self::Placed(placement) | Self::Abandoned(placement) => Some(placement),
            Self::Queued | Self::Cancelled => None,
        }
    }

    #[must_use]
    pub const fn is_queued(self) -> bool {
        matches!(self, Self::Queued)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
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

impl TryFrom<String> for RequestKey {
    type Error = RequestKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RequestKey> for String {
    fn from(value: RequestKey) -> Self {
        value.0
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
