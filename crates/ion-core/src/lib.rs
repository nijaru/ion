//! Ion's durable coding-turn domain.
//!
//! R1A deliberately exposes only the replacement durable vocabulary, schema ownership
//! and pure request assembly. Session execution is rebuilt on these owners in R1B.

mod blob;
mod config;
mod conversation;
mod digest;
mod entry;
mod id;
mod input;
mod model;
mod request;
mod store;
mod tool_exec;
mod transcript;
mod turn;

pub use blob::BlobRef;
pub use config::{
    AuthorityCeiling, ConfigError, ContextPolicy, ControlCeiling, ConversationConfig, EgressRealm,
    InstalledConfig, ProviderBinding, ProviderBindingId, ProviderCapabilities, ReturnedModelPolicy,
    SemanticCompatibilityId, ToolBinding, ToolBindingId, ToolConcurrency, ToolRecoveryPolicy,
    TurnEnvironment, TurnLimits, TurnSettings, WorkspaceBinding,
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
    CostQuote, ModelAttempt, ModelAttemptState, ModelAttemptTiming, ModelStep, ProviderFailureEvidence,
    ProviderFingerprint, ProviderStartReceipt, RequestManifest, StepDisposition, StepPurpose,
};
pub use request::{AssembledRequest, RequestError, SemanticRequest, assemble};
pub use store::{SessionStore, StoreError};
pub use tool_exec::{
    ApprovalState, BaseFact, EffectSummary, OutcomeSource, PreparedAction, ProgressCheckpoint,
    StartReceipt, ToolAttempt, ToolAttemptState, ToolExchangeState, ToolInvocation, ToolResult,
};
pub use transcript::{TranscriptContent, TranscriptMessage, TranscriptRole};
pub use turn::{
    Cancellation, ParkReason, Turn, TurnBudget, TurnFailure, TurnOutcome, TurnPhase,
};
