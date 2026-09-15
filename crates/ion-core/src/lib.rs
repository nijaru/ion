//! Durable core domain and session kernel for Ion.
//!
//! The fresh kernel is built around sessions, conversations, immutable entries,
//! admitted inputs and recoverable tasks. Legacy lane/agent/operation/effect
//! runtime APIs are intentionally not preserved during the pre-1.0 clean rewrite.

pub mod builtin;
pub mod conversation;
mod id;
pub mod session;
mod store;
pub mod task;
pub mod view;

pub use conversation::{
    ConfigError, ContextPolicy, Conversation, ConversationConfig, Entry, EntryKind, EntryKindError,
    HistoryParent, Input, InputBody, InputDisposition, InputMode, InputPlacement, InputSender,
    InstalledConfig, RequestKey, RequestKeyError, RunLimits,
};
pub use id::{ArtifactId, CommitSeq, ConversationId, EntryId, IdError, InputId, SessionId, TaskId};
// Crate-internal only: the backing sequence namespace is not part of the client
// surface (`DESIGN.md` §6).
pub(crate) use id::LocalSeq;
pub use session::{
    AdmissionReceipt, CloseMode, ConversationReceipt, ConversationSpec, DriveOutcome, EntryReceipt,
    EntryRequest, InputReceipt, InputRequest, Interruption, InterruptionReason, Session,
    SessionError, Settlement, TaskCancellation, TaskCapacity, TaskDriver, TaskDriverError,
    TaskReceipt, TaskRequest, TurnCancellation, TurnTemplate,
};
pub use task::{
    AbortContext, ContextCut, DependencyOutcome, InvocationKind, MAX_PLAN_CONVERSATIONS,
    MAX_PLAN_ENTRIES, MAX_PLAN_TASKS, PlacedInput, PlannedConversation, PlannedConversationRef,
    PlannedEntry, PlannedTarget, PlannedTask, PlannedTaskRef, PlannedTurn, RequestBasis,
    ResourceDomain, RunningTask, TaskCompletion, TaskContext, TaskContextError, TaskDependency,
    TaskFuture, TaskInvocation, TaskKind, TaskKindName, TaskKindNameError, TaskOutcome,
    TaskOutcomeKind, TaskOutput, TaskPlan, TaskRecord, TaskRegistry, TaskRegistryError,
    TaskRunError, TaskStatus, TypedAbortContext, TypedContext, TypedFuture, TypedHandler,
    TypedOutcome, TypedReport, TypedTask,
};
pub use view::{
    Change, CommitEvent, EntryPage, ObservationBatch, SessionSnapshot, SessionSummary, TaskCounts,
};
