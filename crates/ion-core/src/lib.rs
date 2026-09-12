//! Durable core domain for Ion.
//!
//! The fresh kernel is built around sessions, conversations, immutable entries,
//! admitted inputs and recoverable tasks. Legacy lane/agent/operation/effect
//! runtime APIs are intentionally not preserved during the pre-1.0 clean rewrite.

mod artifact;
pub mod conversation;
mod id;
pub mod task;

pub use artifact::Artifact;
pub use conversation::{
    Conversation, Entry, EntryKind, EntryKindError, HistoryParent, Input, InputBody,
    InputDisposition, InputMode, InputSender, RequestKey, RequestKeyError,
};
pub use id::{
    ArtifactId, CommitSeq, ConversationId, EntryId, IdError, InputId, LocalSeq, SessionId, TaskId,
};
pub use task::{
    TaskKindName, TaskKindNameError, TaskOutcome, TaskOutcomeKind, TaskOutput, TaskRecord,
    TaskStatus,
};
