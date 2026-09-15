//! Tool invocations: the engine's only path to an external side effect.
//!
//! An invocation binds the validated arguments, the implementation identity
//! and repeat-safety policy recorded at admission, the workspace identity and
//! the generation it ran under. A replacement implementation cannot
//! reinterpret old prepared data merely because it has the same name.

use ion_ai::ToolCall;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{EntryId, InvocationId, StepId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolInvocation {
    pub id: InvocationId,
    /// The step whose settlement admitted this call.
    pub step: StepId,
    /// The assistant entry that carries the call.
    pub entry: EntryId,
    /// Index of the call within that entry's provider message.
    pub call_index: u32,
    pub call: ToolCall,
    /// The implementation identity admitted with the call: tool name plus the
    /// implementation revision the host registered. Recovery compares this,
    /// not the display name.
    pub implementation: String,
    /// Whether the recorded policy permits repeating this action after its
    /// outcome became unknown. Repeat-safe does not establish the earlier
    /// outcome; it only removes the engine's veto.
    pub repeat_safe: bool,
    /// The turn's cancellation generation when dispatch intent committed.
    pub generation: u64,
    pub state: InvocationState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InvocationState {
    /// Prepared and admitted, not dispatched.
    Prepared,
    /// Dispatch intent is durable; the side effect may or may not have
    /// happened.
    Dispatched,
    /// The action completed and its result is durable.
    Succeeded { result: Value },
    /// The action reported a known failure; nothing was left uncertain.
    Failed { message: String },
    /// The action may have happened and its result is unknown, and no client
    /// has decided what to do about it. A turn in this state makes no further
    /// progress.
    Indeterminate,
    /// The outcome is unknown and a client explicitly accepted that, with a
    /// truthful result recorded in the transcript. The exchange may continue;
    /// the external action is still not established as complete or stopped.
    Unknown,
}

impl InvocationState {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Dispatched => "dispatched",
            Self::Succeeded { .. } => "succeeded",
            Self::Failed { .. } => "failed",
            Self::Indeterminate => "indeterminate",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "prepared" => Self::Prepared,
            "dispatched" => Self::Dispatched,
            "succeeded" => Self::Succeeded {
                result: Value::Null,
            },
            "failed" => Self::Failed {
                message: String::new(),
            },
            "indeterminate" => Self::Indeterminate,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    #[must_use]
    pub const fn is_settled(&self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. } | Self::Failed { .. } | Self::Indeterminate | Self::Unknown
        )
    }
}

/// What a client decided about an invocation whose outcome is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
    /// Repeat the action. Refused unless the recorded policy is repeat-safe.
    Repeat,
    /// Record the outcome as unknown, with a truthful result, and continue the
    /// message exchange. This does not establish that the action stopped.
    Indeterminate,
}

/// The truthful result recorded for an invocation that was not dispatched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InvocationOutcome {
    Succeeded { result: Value },
    Failed { message: String },
    Indeterminate { message: String },
}

impl InvocationOutcome {
    #[must_use]
    pub fn state(&self) -> InvocationState {
        match self {
            Self::Succeeded { result } => InvocationState::Succeeded {
                result: result.clone(),
            },
            Self::Failed { message } => InvocationState::Failed {
                message: message.clone(),
            },
            Self::Indeterminate { message: _ } => InvocationState::Indeterminate,
        }
    }

    /// The transcript result the model reads.
    #[must_use]
    pub fn result_value(&self, call: &ToolCall) -> Value {
        match self {
            Self::Succeeded { result } => result.clone(),
            Self::Failed { message } | Self::Indeterminate { message } => {
                serde_json::json!({
                    "call": call.name,
                    "outcome": if matches!(self, Self::Failed { .. }) {
                        "failed"
                    } else {
                        "unknown"
                    },
                    "detail": message,
                })
            }
        }
    }
}
