//! Model steps and their physical attempts.
//!
//! A step is one logical request basis: a frozen model, instruction text,
//! project context, tool catalog, control values and history cutoff. Retrying
//! sends the same step again; changing any of those inputs creates a new step
//! rather than silently modifying an attempt.
//!
//! One step may have several attempts. Their evidence is kept separately so a
//! crash between "the provider answered" and "the transcript recorded it" is
//! recoverable without asking the provider again.

use ion_ai::{GenerationControls, ModelRef, ModelResponse, ToolSpec};
use serde::{Deserialize, Serialize};

use crate::{AttemptId, CommitSeq, EntryId, StepId, TurnId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelStep {
    pub id: StepId,
    pub turn: TurnId,
    /// Position of this step within its turn, starting at zero.
    pub ordinal: u32,
    /// Only entries at or before this id are assembled. `None` means the whole
    /// conversation at the moment the basis was captured.
    pub cut: Option<EntryId>,
    /// The configuration revision this basis was captured from, so a later
    /// reconfiguration cannot reinterpret the frozen request.
    pub config_revision: CommitSeq,
    pub model: ModelRef,
    pub instructions: String,
    /// Resolved project context, frozen as content rather than as a path to
    /// read again.
    pub context: Vec<ion_ai::Message>,
    pub controls: GenerationControls,
    pub tools: Vec<ToolSpec>,
    /// The context policy's request byte budget, frozen with the basis.
    pub max_request_bytes: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelAttempt {
    pub id: AttemptId,
    pub step: StepId,
    /// Position of this attempt within its step, starting at one.
    pub ordinal: u32,
    /// The turn's cancellation generation when dispatch intent committed.
    pub generation: u64,
    pub state: AttemptState,
    /// Response-ready evidence. Present once the provider answer is durable and
    /// until the settlement transaction consumes it.
    pub response: Option<ModelResponse>,
}

/// Where one physical attempt has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptState {
    /// The basis is frozen and no dispatch is authorized yet.
    Prepared,
    /// Dispatch intent is durable. The provider call may or may not have
    /// happened; this is the state that makes a silent repeat unsafe to infer.
    Dispatched,
    /// A validated response is durable. Transcript and tool settlement may
    /// still be pending, and another model call must not be made.
    ResponseReady,
    /// The response was consumed by the settlement transaction.
    Settled,
    /// Dispatch intent existed, no response was ever durably recorded, and the
    /// process that could have produced one is gone. The cost may or may not
    /// have been incurred.
    Indeterminate,
}

impl AttemptState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Dispatched => "dispatched",
            Self::ResponseReady => "response_ready",
            Self::Settled => "settled",
            Self::Indeterminate => "indeterminate",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "prepared" => Self::Prepared,
            "dispatched" => Self::Dispatched,
            "response_ready" => Self::ResponseReady,
            "settled" => Self::Settled,
            "indeterminate" => Self::Indeterminate,
            _ => return None,
        })
    }
}
