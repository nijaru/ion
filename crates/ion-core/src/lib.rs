//! Ion's durable coding-turn domain.
//!
//! R1B adds the passive replacement Session owner, semantic SQLite transactions and
//! bounded commit-addressed observation. Provider/tool drive remains a later R1B slice.

mod blob;
mod config;
mod conversation;
mod digest;
mod drive;
mod effect_gate;
mod entry;
mod id;
mod input;
mod model;
mod observation;
mod provider;
mod request;
mod session;
mod store;
mod tool_exec;
mod transcript;
mod turn;

pub use blob::BlobRef;
pub use config::{
    AuthorityCeiling, ConfigError, ContextPolicy, ControlCeiling, ConversationConfig, EgressRealm,
    InstalledConfig, ProviderBinding, ProviderBindingId, ProviderCapabilities, ReturnedModelPolicy,
    SemanticCompatibilityId, StartReceiptCapability, ToolBinding, ToolBindingId, ToolConcurrency,
    ToolRecoveryPolicy, TurnEnvironment, TurnLimits, TurnSettings, WorkspaceBinding,
};
pub use conversation::{Conversation, HistoryParent};
pub use digest::ContentDigest;
pub use entry::{
    CheckpointDecision, ContextBoundary, ContinuationCheckpoint, Entry, EntryData, EntryRange,
    EvidenceRef,
};
pub use id::{
    AttemptId, CommitSeq, ConversationId, EntryId, IdError, InputId, InvocationId, SessionId,
    StepId, TurnId,
};
pub use input::{
    Input, InputBody, InputDisposition, InputMode, InputSender, RequestKey, RequestKeyError,
};
pub use model::{
    CostQuote, ModelAttempt, ModelAttemptState, ModelAttemptTiming, ModelStep,
    ProviderFailureEvidence, ProviderFingerprint, ProviderStartReceipt, RequestManifest,
    StepDisposition, StepPurpose,
};
pub use observation::{
    CommitReceipt, EntryPage, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_ENTRIES, MAX_SNAPSHOT_INPUTS,
    MAX_WATCH_BYTES, MAX_WATCH_RECEIPTS, ObservationError, SessionChange, SessionSnapshot,
    SessionUpdate, SessionWatch, SnapshotRequest, SnapshotWatch, WatchQueueLimits, WatchRequest,
};
pub use provider::{
    ModelBoundaries, ModelBoundary, ModelBoundaryError, ModelBoundaryIdentity, ModelStart,
    StartReconciliation,
};
pub use drive::{DriveExit, DrivePolicy};
pub use request::{
    AssembledRequest, RequestError, SEMANTIC_REQUEST_ASSEMBLY_REVISION, SemanticRequest, assemble,
    semantic_request_assembly_revision,
};
pub use session::{
    AbandonResult, Admission, AdmitInputRequest, CancellationResult, ConfiguredConversation,
    CreatedConversation, CreatedSession, Session, SessionError, SessionHandle, SessionHealth,
    StartTurnRequest, StartedTurn,
};
pub use tool_exec::{
    ApprovalState, BaseFact, EffectSummary, OutcomeSource, PreparedAction, ProgressCheckpoint,
    StartReceipt, ToolAttempt, ToolAttemptState, ToolExchangeState, ToolInvocation, ToolResult,
};
pub use transcript::{TranscriptContent, TranscriptMessage, TranscriptRole};
pub use turn::{Cancellation, ParkReason, Turn, TurnBudget, TurnFailure, TurnOutcome, TurnPhase};
