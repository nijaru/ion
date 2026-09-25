//! One accepted coding request and its durable continuation.

use serde::{Deserialize, Serialize};

use crate::{
    AttemptId, ConversationId, EntryId, InvocationId, StepId, TurnEnvironment, TurnId, TurnSettings,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub conversation: ConversationId,
    pub environment: TurnEnvironment,
    pub settings: TurnSettings,
    pub phase: TurnPhase,
    pub cancellation: Cancellation,
    pub budget: TurnBudget,
    pub admitted_at_unix_ms: i64,
    /// Optional automation/user deadline. Interactive Turns have no mandatory wall deadline.
    pub wall_deadline_unix_ms: Option<i64>,
    pub outcome: Option<TurnOutcome>,
}

impl Turn {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        self.outcome.is_some()
    }

    #[must_use]
    pub const fn is_cancelling(&self) -> bool {
        self.cancellation.requested
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnPhase {
    Ready,
    Model(StepId),
    Tools(StepId),
    WaitingInteraction(InvocationId),
    BlockedEffect {
        invocation: InvocationId,
        attempt: Option<AttemptId>,
    },
    Parked(ParkReason),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParkReason {
    MissingCredentials,
    ProviderUnavailable,
    ReturnedModelMismatch,
    ToolChoiceMismatch,
    ToolUnavailable,
    AwaitingApproval,
    AuthorityDenied,
    Capacity,
    ContextCapacity,
    RecoveryRequired,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Cancellation {
    pub requested: bool,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TurnBudget {
    pub model_steps: u32,
    pub model_attempts: u32,
    pub tool_invocations: u32,
    pub reserved_cost_microusd: u64,
    pub reported_cost_microusd: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnOutcome {
    Completed { entry: EntryId },
    Failed { failure: TurnFailure },
    Cancelled { unresolved_attempts: Vec<AttemptId> },
    Abandoned { unresolved_attempts: Vec<AttemptId> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnFailure {
    Protocol(String),
    Provider(String),
    Limit(String),
    ContextCapacity,
    Storage(String),
    Unsupported(String),
}
