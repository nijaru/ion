//! Durable core domain and session kernel for Ion.
//!
//! The fresh kernel is built around sessions, conversations, immutable entries,
//! admitted inputs and recoverable tasks. Legacy lane/agent/operation/effect
//! runtime APIs are intentionally not preserved during the pre-1.0 clean rewrite.

mod artifact;
pub mod builtin;
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
    CloseMode, ConversationReceipt, ConversationSpec, DriveOutcome, EntryReceipt, EntryRequest,
    InputReceipt, InputRequest, Interruption, InterruptionReason, Session, SessionError,
    Settlement, SubmissionReceipt, TaskCancellation, TaskCapacity, TaskDriver, TaskDriverError,
    TaskReceipt, TaskRequest, TurnCancellation,
};
pub use task::{
    AbortContext, DependencyOutcome, InvocationKind, MAX_PLAN_ENTRIES, MAX_PLAN_INPUTS,
    MAX_PLAN_TASKS, PlannedEntry, PlannedEntryRef, PlannedTask, PlannedTaskRef, ResourceDomain,
    RunningTask, TaskCompletion, TaskContext, TaskContextError, TaskDependency, TaskFuture,
    TaskInvocation, TaskKind, TaskKindName, TaskKindNameError, TaskOutcome, TaskOutcomeKind,
    TaskOutput, TaskPlan, TaskRecord, TaskRegistry, TaskRegistryError, TaskRunError, TaskStatus,
    TypedAbortContext, TypedContext, TypedFuture, TypedHandler, TypedOutcome, TypedReport,
    TypedTask,
};
pub use view::{
    Change, CommitEvent, EntryPage, ObservationBatch, SessionSnapshot, SessionSummary, TaskCounts,
};
