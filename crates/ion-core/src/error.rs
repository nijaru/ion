//! The public error surface.
//!
//! Errors name what a caller can act on. A refused admission, a conflicting
//! request key, an unsupported schema and a fenced session are different
//! failures, and none of them is reported as a generic string.

use std::path::PathBuf;

use ion_ai::ProviderError;
use thiserror::Error;

use crate::config::ConfigError;
use crate::id::IdError;

#[derive(Debug, Error)]
pub enum Error {
    #[error("session database {0} is owned by another live process")]
    SessionInUse(PathBuf),
    #[error("session database {0} does not exist")]
    UnknownSession(PathBuf),
    #[error("unsupported session schema version {found}; this build writes version {expected}")]
    UnsupportedSchema { found: i64, expected: i64 },
    #[error("the conversation {0} is already answering a turn")]
    Busy(crate::ConversationId),
    #[error("request key {key:?} was already used for a different request")]
    RequestKeyConflict { key: String },
    #[error("no queued input remains for the conversation")]
    NoQueuedInput,
    #[error("the queued input limit ({limit}) is reached")]
    QueueFull { limit: u32 },
    #[error("session storage quota is exhausted; {used} of {quota} bytes are in use")]
    QuotaExhausted { used: u64, quota: u64 },
    #[error("turn {0} is not terminal yet")]
    TurnNotTerminal(crate::TurnId),
    #[error("the turn {turn} has no invocation {invocation}")]
    UnknownInvocation {
        turn: crate::TurnId,
        invocation: crate::InvocationId,
    },
    #[error("invocation {0} is not in a state that can be resolved")]
    InvocationNotResolvable(crate::InvocationId),
    #[error("invocation {0} is not repeat-safe; repeating it needs a new authorized turn")]
    NotRepeatSafe(crate::InvocationId),
    #[error("the conversation {0} has no unfinished turn to resume")]
    NothingToResume(crate::ConversationId),
    #[error("configuration revision {expected} is stale; the installed revision is {actual:?}")]
    StaleConfig {
        expected: crate::CommitSeq,
        actual: Option<crate::CommitSeq>,
    },
    #[error("the session is closing or closed")]
    Closed,
    #[error("the session fenced after a persistence failure: {message}")]
    Fenced { message: String },
    #[error("the stored session is inconsistent: {0}")]
    Corrupt(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("{0}")]
    Provider(#[from] ProviderError),
    #[error("{0}")]
    Identity(#[from] IdError),
    #[error("persistence failed: {0}")]
    Persistence(String),
}

pub type Result<T> = std::result::Result<T, Error>;
