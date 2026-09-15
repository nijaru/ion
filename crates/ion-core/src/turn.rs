//! The durable turn: one accepted request, its continuation and its outcome.
//!
//! A turn is the unit that owns input placement, model-step continuation,
//! resource accounting, cancellation and the terminal outcome. It replaces the
//! former generic task, its plan and its distributed turn bookkeeping; there is
//! no second owner of the same facts.

use ion_ai::ProviderErrorKind;
use serde::{Deserialize, Serialize};

use crate::config::RunLimits;
use crate::{ConversationId, EntryId, InvocationId, StepId, TurnId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub id: TurnId,
    pub conversation: ConversationId,
    pub phase: TurnPhase,
    pub cancellation: Cancellation,
    /// Limits installed at admission. Ordinary configuration changes affect
    /// later turns, and can never raise this turn's ceiling.
    pub limits: RunLimits,
    pub admitted_at_unix_ms: i64,
    /// `None` while the turn is unfinished. A terminal outcome is immutable.
    pub outcome: Option<TurnOutcome>,
}

impl Turn {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        self.outcome.is_some()
    }

    /// Whether cancellation has been requested but the turn has not settled.
    #[must_use]
    pub const fn is_cancelling(&self) -> bool {
        self.cancellation.requested
    }

    /// The absolute deadline as a wall-clock millisecond value.
    ///
    /// Wall-clock moving backwards is a stated limitation rather than a
    /// promise: a live process also compares a monotonic instant.
    #[must_use]
    pub fn deadline_unix_ms(&self) -> Option<i64> {
        let deadline_ms = i64::try_from(self.limits.deadline_ms).ok()?;
        self.admitted_at_unix_ms.checked_add(deadline_ms)
    }
}

/// The next semantic action of a turn.
///
/// Phase names what the engine is about to do. Referenced attempt and
/// invocation records own external state; duplicating their full status here
/// would create two authorities for one fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnPhase {
    /// A new model step may begin at the current history cutoff.
    Ready,
    /// The step's request basis is frozen and an attempt is being dispatched
    /// or settled.
    ModelStep(StepId),
    /// The assistant response for `step` is settled; its tool calls are being
    /// executed in call order.
    Tools(StepId),
    /// An invocation may have produced an external effect and its outcome is
    /// unknown. The turn makes no further progress until a client resolves it,
    /// because repeating a possibly-performed action is not the engine's
    /// decision to make.
    Blocked {
        step: StepId,
        invocation: InvocationId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Cancellation {
    pub requested: bool,
    /// Bumped by every accepted cancellation. A dispatched invocation records
    /// the generation it ran under, so a stale completion cannot be mistaken
    /// for fresh evidence.
    pub generation: u64,
}

/// How a turn ended. Terminal and immutable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnOutcome {
    /// The selected final answer is `entry`. Nothing else completes a turn
    /// successfully: a response that still has tool calls to run has not
    /// finished answering, and only this outcome outranks a cancellation.
    Completed {
        entry: EntryId,
    },
    Failed {
        cause: TurnFailure,
    },
    /// Cancelled. `unresolved` names invocations whose external outcome is not
    /// established; their workspace claims remain in force. Cancellation is not
    /// rollback and never rewrites an action as failed.
    Cancelled {
        unresolved: Vec<InvocationId>,
    },
    /// Explicitly given up on by a client, with the same unresolved effects as
    /// cancellation.
    Abandoned {
        unresolved: Vec<InvocationId>,
    },
}

impl TurnOutcome {
    #[must_use]
    pub const fn is_completed(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }

    #[must_use]
    pub fn unresolved(&self) -> &[InvocationId] {
        match self {
            Self::Cancelled { unresolved } | Self::Abandoned { unresolved } => unresolved,
            Self::Completed { .. } | Self::Failed { .. } => &[],
        }
    }
}

/// The outcome a caller asks for, before unresolved effects are folded in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingOutcome {
    Completed { entry: EntryId },
    Cancelled,
    Abandoned,
    Failed(TurnFailure),
}

impl PendingOutcome {
    /// Finalize against the invocations whose external outcome is unknown.
    #[must_use]
    pub fn finalize(self, unresolved: Vec<InvocationId>) -> TurnOutcome {
        match self {
            Self::Completed { entry } => TurnOutcome::Completed { entry },
            Self::Cancelled => TurnOutcome::Cancelled { unresolved },
            Self::Abandoned => TurnOutcome::Abandoned { unresolved },
            Self::Failed(cause) => TurnOutcome::Failed { cause },
        }
    }
}

/// Why a turn failed. Typed facts rather than a string, so a client does not
/// parse a message to learn what happened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnFailure {
    /// The provider or its adapter produced something the engine refuses.
    Protocol { message: String },
    /// The provider reported a failure before a validated response was ready.
    Provider {
        kind: ProviderErrorKind,
        message: String,
    },
    /// A durable budget was exhausted.
    Limit { setting: String },
    /// The turn's absolute deadline passed.
    Deadline,
    /// Storage refused the transition. The session fences rather than
    /// pretending the turn continued.
    Storage { message: String },
}
