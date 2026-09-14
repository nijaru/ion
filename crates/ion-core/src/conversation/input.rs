use std::fmt;

use ion_ai::{Content, Message, Role};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{ConversationId, EntryId, EntryKind, InputId, TaskId};

/// The entry kind an accepted input is placed under.
///
/// The session writer places this entry when it binds the input to its turn, so
/// the kind is owned here rather than by the built-in kinds that read it.
pub const INPUT_ENTRY: &str = "user";

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

impl InputBody {
    /// How this body appears in the transcript.
    ///
    /// Placement belongs to the body, so the writer that appends the entry and
    /// the projection a model reads cannot disagree about its content.
    #[must_use]
    pub fn placement(&self) -> EntryPlacement {
        match self {
            Self::Text(text) => EntryPlacement {
                kind: EntryKind::new(INPUT_ENTRY).expect("the placement kind is valid"),
                data: json!({"text": text}),
                projection: vec![Message {
                    role: Role::User,
                    content: vec![Content::Text(text.clone())],
                    provider_replay: None,
                }],
            },
        }
    }
}

/// The transcript content an accepted input is placed as.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryPlacement {
    pub kind: EntryKind,
    pub data: Value,
    pub projection: Vec<Message>,
}

/// Where an accepted input is in the transcript, and what answers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputPlacement {
    /// The entry that carries the input.
    pub entry: EntryId,
    /// The turn currently answering it: the turn it was placed with, or the new
    /// turn an explicit retry started.
    pub turn: TaskId,
}

/// How far an accepted input has got.
///
/// Placement and answer are separate facts. The entry is committed when the
/// input is bound to the turn that will answer it, before any invocation runs,
/// so a cancelled or failed answer leaves the accepted message in the history
/// instead of stranding it outside. `Placed` therefore does not mean the answer
/// succeeded, and `Consumed` no longer exists to conflate the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputDisposition {
    /// Admitted and waiting for the turn that will answer it.
    Queued,
    /// Placed in the transcript and bound to the turn answering it.
    Placed(InputPlacement),
    /// Placed, then explicitly given up on: the entry stays, and no further
    /// attempt will answer it. Abandonment never cancels running work.
    Abandoned(InputPlacement),
    /// Withdrawn before it was ever placed, so there is no entry to preserve.
    Cancelled,
}

impl InputDisposition {
    /// The turn currently answering this input, if it has one.
    #[must_use]
    pub const fn answering_turn(&self) -> Option<TaskId> {
        match self {
            Self::Placed(placement) | Self::Abandoned(placement) => Some(placement.turn),
            Self::Queued | Self::Cancelled => None,
        }
    }

    /// Where the input is in the transcript, once it has been placed.
    #[must_use]
    pub const fn placement(&self) -> Option<InputPlacement> {
        match self {
            Self::Placed(placement) | Self::Abandoned(placement) => Some(*placement),
            Self::Queued | Self::Cancelled => None,
        }
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
