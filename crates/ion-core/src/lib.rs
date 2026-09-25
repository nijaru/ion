//! Ion's durable coding-turn domain, Session storage and provider/tool boundaries.

mod artifact;
mod blob;
mod bounded_json;
mod config;
mod conversation;
mod digest;
mod drive;
mod effect_gate;
mod entry;
mod id;
mod input;
mod model;
mod native_edit;
mod native_read;
mod observation;
pub mod openai_compatible;
mod provider;
mod request;
mod session;
mod store;
mod tool_boundary;
mod tool_drive;
mod tool_exec;
mod transcript;
mod turn;

pub mod workspace_registry;

pub use artifact::{ArtifactError, ArtifactPublisher, ArtifactRead};
pub use blob::{BlobQuota, BlobRef, BlobStore, BlobStoreError, BlobStoreLimits, BlobStoreUsage};
pub use config::{
    AuthorityCeiling, ConfigError, ContextPolicy, ControlCeiling, ConversationConfig, EgressRealm,
    InstalledConfig, ProviderBinding, ProviderBindingId, ProviderCapabilities, ReturnedModelPolicy,
    SemanticCompatibilityId, StartReceiptCapability, ToolBinding, ToolBindingId, ToolConcurrency,
    ToolRecoveryPolicy, TurnEnvironment, TurnLimits, TurnSettings, WorkspaceBinding,
};
pub use conversation::{Conversation, HistoryParent};
pub use digest::ContentDigest;
pub use drive::{DriveExit, DrivePolicy};
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
pub use native_edit::{
    MAX_NATIVE_EDIT_BYTES, NativeEditBoundary, NativeEditError, native_edit_binding,
};
pub use native_read::{
    MAX_NATIVE_READ_BYTES, NativeReadBoundary, NativeReadError, native_read_binding,
};
pub use observation::{
    CommitReceipt, EntryPage, MAX_SNAPSHOT_BYTES, MAX_SNAPSHOT_ENTRIES, MAX_SNAPSHOT_INPUTS,
    MAX_WATCH_BYTES, MAX_WATCH_RECEIPTS, ObservationError, SessionChange, SessionSnapshot,
    SessionUpdate, SessionWatch, SnapshotRequest, SnapshotWatch, WatchQueueLimits, WatchRequest,
};
pub use provider::{
    ModelBoundaries, ModelBoundary, ModelBoundaryError, ModelBoundaryIdentity, ModelStart,
    ProviderAdmission, ProviderAdmissionError, StartReconciliation,
};
pub use request::{
    AssembledRequest, RequestError, SEMANTIC_REQUEST_ASSEMBLY_REVISION, SemanticRequest, assemble,
    semantic_request_assembly_revision,
};
pub use session::{
    AbandonResult, Admission, AdmitInputRequest, CancellationResult, ConfiguredConversation,
    CreatedConversation, CreatedSession, Session, SessionError, SessionHandle, SessionHealth,
    StartTurnRequest, StartedTurn, SubmitTurnRequest, SubmittedTurn,
};
pub use store::ToolRecords;
pub use tool_boundary::{
    LiveToolAuthority, MAX_TOOL_ATTEMPTS, MAX_TOOL_RECORD_BYTES, ToolBoundaries, ToolBoundary,
    ToolBoundaryError, ToolExecution,
};
pub use tool_exec::{
    ApprovalDecision, ApprovalState, BaseFact, EffectSummary, OutcomeSource, OutputCapture,
    OutputLoss, PreparedAction, ProgressCheckpoint, StartReceipt, ToolAttempt, ToolAttemptState,
    ToolAuthority, ToolExchangeState, ToolInvocation, ToolPreparation, ToolResult,
};
pub use transcript::{TranscriptContent, TranscriptMessage, TranscriptRole};
pub use turn::{Cancellation, ParkReason, Turn, TurnBudget, TurnFailure, TurnOutcome, TurnPhase};
