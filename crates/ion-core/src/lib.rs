//! Durable core domain and session kernel for Ion.
//!
//! The fresh kernel is built around sessions, conversations, immutable entries,
//! admitted inputs and recoverable tasks. Legacy lane/agent/operation/effect
//! runtime APIs are intentionally not preserved during the pre-1.0 clean rewrite.

mod artifact;
pub mod conversation;
mod id;
pub mod session;
mod store;
pub mod task;
pub mod view;

pub use artifact::Artifact;
pub use conversation::{
    Conversation, Entry, EntryKind, EntryKindError, HistoryParent, Input, InputBody,
    InputDisposition, InputMode, InputSender, RequestKey, RequestKeyError,
};
pub use id::{
    ArtifactId, CommitSeq, ConversationId, EntryId, IdError, InputId, LocalSeq, SessionId, TaskId,
};
pub use session::{
    ConversationReceipt, ConversationSpec, DriveOutcome, EntryReceipt, EntryRequest, InputReceipt,
    InputRequest, Session, SessionError, TaskCancellation, TaskDriver, TaskDriverError,
    TaskReceipt, TaskRequest,
};
pub use task::{
    AbortContext, InvocationKind, RunningTask, TaskCompletion, TaskContext, TaskContextError,
    TaskFuture, TaskInvocation, TaskKind, TaskKindName, TaskKindNameError, TaskOutcome,
    TaskOutcomeKind, TaskOutput, TaskRecord, TaskRegistry, TaskRegistryError, TaskRunError,
    TaskStatus,
};
pub use view::{Change, CommitEvent, ObservationBatch, SessionSnapshot};
