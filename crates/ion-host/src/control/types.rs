//! Public control contracts and bounded passive observation.
use crate::image_input::ImageSource;
use anyhow::Result;
use ion_ai::Message;
use ion_core::{
    CodingAgentError, CodingAgentEvent as AgentEvent, CodingSessionError, CodingToolOutput,
    InputReservation,
};
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Local custody identities, not protocol IDs or durable deduplication keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct InputId(pub(super) u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct OperationId(pub(super) u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Delivery {
    Start,
    Steer(OperationId),
    FollowUp,
}

/// Local control activity, not a durable execution-start or acceptance fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum OperationKind {
    Turn,
    Compact,
    Shell,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct OperationState {
    pub id: OperationId,
    /// Acknowledged logical Turn; None is not proof of absent admission.
    pub turn: Option<u64>,
    pub kind: OperationKind,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ControlState {
    pub operation: Option<OperationState>,
    pub busy: bool,
    pub preparing_input: bool,
    pub pending_steering: usize,
    pub pending_follow_ups: usize,
    pub stop_requested: bool,
    /// Admission closure, not proof of completed cleanup.
    pub closed: bool,
    pub shutdown_complete: bool,
}

/// Prepared messages are literal: only Prompt performs resource expansion.
#[derive(Clone, Debug, Serialize)]
pub enum UserInput {
    Prompt {
        text: String,
        images: Vec<ImageSource>,
    },
    Message(Message),
}

/// Existing delivery returns this attempt's metadata without taking new custody.
/// Started means durable admission, unlike ordinary submit's local InputId.
pub enum RequestAdmission<M> {
    Started {
        id: InputId,
        receipt: ion_core::RequestReceipt,
    },
    Existing {
        receipt: ion_core::RequestReceipt,
        metadata: M,
    },
}

/// A committed continuation and its local owner; no new input was accepted.
#[derive(Debug)]
pub struct RecoveryAdmission {
    pub operation: OperationId,
    pub point: ion_core::RecoveryPoint,
}

pub struct RejectedInput<M> {
    pub delivery: Delivery,
    pub input: UserInput,
    pub metadata: M,
    pub reason: anyhow::Error,
}
impl<M> std::fmt::Debug for RejectedInput<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RejectedInput")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl<M> std::fmt::Display for RejectedInput<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.reason.fmt(f)
    }
}
impl<M> std::error::Error for RejectedInput<M> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.reason.as_ref())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum InputPhase {
    Preparing,
    Queued,
}
#[derive(Debug)]
pub enum ReturnReason {
    Stopped,
    Cleared,
    TurnEnded,
    Preparation(anyhow::Error),
    NotAccepted,
}

pub struct ReturnedInput<M> {
    pub id: InputId,
    pub delivery: Delivery,
    pub input: UserInput,
    pub metadata: M,
    pub reason: ReturnReason,
    /// A preparation fault racing withdrawal remains observable as well.
    pub preparation_error: Option<anyhow::Error>,
    pub(super) reservation: Option<InputReservation>,
}
pub struct AcceptedMetadata<M> {
    pub id: InputId,
    pub acceptance: super::InputAcceptance,
    pub metadata: M,
    pub(super) reservation: Option<InputReservation>,
}

pub enum OperationOutcome {
    Turn(Result<String, CodingAgentError>),
    Compact(Result<bool, CodingAgentError>),
    Shell(Result<CodingToolOutput, CodingSessionError>),
}
impl OperationOutcome {
    pub(super) fn failed(&self) -> bool {
        match self {
            Self::Turn(r) => r.is_err(),
            Self::Compact(r) => r.is_err(),
            Self::Shell(r) => r.is_err(),
        }
    }
}

pub struct Settlement<M> {
    pub operation: OperationId,
    pub turn: Option<u64>,
    pub outcome: OperationOutcome,
    pub stop_requested: bool,
    /// Root and absorbed steering metadata in admission order.
    pub accepted: Vec<AcceptedMetadata<M>>,
    /// Shell/compaction/continuation metadata (no new user-message acceptance).
    pub metadata: Option<M>,
    pub(super) reservation: Option<InputReservation>,
}

/// Progress can have gaps. After a gap, deltas are suppressed until a complete
/// committed boundary or explicit response restart. Reconcile against Session
/// inspection at Gap and Settlement; never append a surviving delta to a lost prefix.
#[derive(Clone, Debug)]
pub enum Observation {
    Event {
        operation: OperationId,
        event: Arc<AgentEvent>,
    },
    Gap {
        /// Zero denotes initial subscription: inspect committed Session state.
        skipped: u64,
    },
}

pub enum ControlUpdate<M> {
    Input {
        id: InputId,
        delivery: Delivery,
        phase: InputPhase,
    },
    /// Local transfer to a Core operation, before durable acceptance. Retains
    /// correlation even when admission fails and the original input is returned.
    Dispatched {
        id: InputId,
        operation: OperationId,
    },
    Accepted {
        id: InputId,
        operation: OperationId,
        turn: u64,
    },
    Activity(Observation),
    Returned(ReturnedInput<M>),
    Settled(Settlement<M>),
    /// No work remains now; admission remains open unless shutdown was requested.
    Idle,
}
impl<M> ControlUpdate<M> {
    pub(super) fn release(&mut self) -> usize {
        match self {
            Self::Returned(input) => {
                input.reservation.take();
                1
            }
            Self::Settled(settlement) => {
                settlement.reservation.take();
                for input in &mut settlement.accepted {
                    input.reservation.take();
                }
                settlement.accepted.len() + usize::from(settlement.metadata.is_some())
            }
            _ => 0,
        }
    }
}

/// A passive subscriber. Dropping it or falling behind cannot stop execution.
pub struct ControlObserver {
    pub(super) receiver: broadcast::Receiver<Observation>,
    pub(super) suppress: bool,
    pub(super) initial: bool,
}
impl ControlObserver {
    pub async fn next(&mut self) -> Option<Observation> {
        if std::mem::take(&mut self.initial) {
            return Some(Observation::Gap { skipped: 0 });
        }
        loop {
            match self.receiver.recv().await {
                Ok(Observation::Event { operation, event }) => {
                    let boundary = matches!(
                        event.as_ref(),
                        AgentEvent::TurnAccepted { .. }
                            | AgentEvent::AssistantCommitted { .. }
                            | AgentEvent::ResponseRestarted
                            | AgentEvent::TurnEnded { .. }
                    );
                    if self.suppress && !boundary {
                        continue;
                    }
                    if boundary {
                        self.suppress = false;
                    }
                    return Some(Observation::Event { operation, event });
                }
                Ok(gap) => return Some(gap),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    self.suppress = true;
                    return Some(Observation::Gap { skipped });
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

pub struct ShutdownReport<M> {
    pub updates: Vec<ControlUpdate<M>>,
    /// Independent cleanup failures never replace primary operation failures.
    pub cleanup_errors: Vec<CodingSessionError>,
}
