//! Public control contracts and bounded passive observation.
use crate::image_input::ImageSource;
use anyhow::Result;
use ion_ai::Message;
use ion_core::{AgentError, AgentEvent, InputReservation, SessionError, ToolOutput};
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
/// Independent maintenance identity and stop intent, not a Core operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct McpReconnectState {
    pub id: OperationId,
    pub stop_requested: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ControlState {
    pub completion_delivery: crate::CompletionDelivery,
    pub operation: Option<OperationState>,
    /// Retained binding preparation/result, independent of Core execution.
    pub binding: Option<crate::binding::BindingOperationId>,
    /// Independent user management, never a steering target or a Core Turn.
    pub mcp_reconnect: Option<McpReconnectState>,
    pub busy: bool,
    pub preparing_input: bool,
    pub pending_steering: usize,
    pub pending_follow_ups: usize,
    pub stop_requested: bool,
    /// Admission closure, not proof of completed cleanup.
    pub closed: bool,
    pub shutdown_complete: bool,
}

/// Identity of the foreground work and inputs affected by a stop request.
/// Progress and admission acknowledgement do not change this identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopTarget {
    pub(super) owner: uuid::Uuid,
    pub(super) session: uuid::Uuid,
    pub(super) operation: Option<OperationId>,
    pub(super) binding: Option<crate::binding::BindingOperationId>,
    pub(super) reconnect: Option<OperationId>,
    pub(super) inputs: Vec<InputId>,
}

/// Prepared messages are literal. Prompt expands only `text`; file snapshots
/// remain data and never select resources or commands.
#[derive(Clone, Debug, Serialize)]
pub enum UserInput {
    Prompt {
        text: String,
        images: Vec<ImageSource>,
        files: Vec<crate::prompt_files::TextFile>,
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
    /// Neither a positive durable receipt nor proof of rollback is available.
    Unresolved(UnresolvedRequest<M>),
}

/// An owned read-only reconciliation result. A new plan grants no admission;
/// launch rechecks local capacity and Core arbitrates any concurrent key race.
pub struct RequestPlan {
    pub(super) session: uuid::Uuid,
    pub(super) key: ion_core::RequestKey,
    pub(super) input: Message,
    pub(super) existing: Option<ion_core::RequestReceipt>,
}
impl RequestPlan {
    pub fn existing(&self) -> Option<&ion_core::RequestReceipt> {
        self.existing.as_ref()
    }
}

pub enum RequestLaunch<M> {
    Pending(InputId),
    Existing {
        receipt: ion_core::RequestReceipt,
        metadata: M,
    },
}

pub struct UnresolvedRequest<M> {
    pub session: uuid::Uuid,
    pub key: ion_core::RequestKey,
    pub id: InputId,
    pub operation: OperationId,
    pub input: UserInput,
    pub metadata: M,
    pub reason: anyhow::Error,
}

/// Reliable admission decision, published before passive operation activity.
/// Started metadata remains in accepted custody until settlement. Other outcomes
/// retain their reservation here until the controlling application takes them.
pub struct RequestDecision<M> {
    pub id: InputId,
    pub result: Result<RequestAdmission<M>, Box<RejectedInput<M>>>,
    pub(super) reservation: Option<InputReservation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionFailureKind {
    NotAccepted,
    Unknown,
}

pub struct RecoveryFailure<M> {
    pub point: ion_core::RecoveryPoint,
    pub metadata: M,
    pub reason: anyhow::Error,
    pub kind: AdmissionFailureKind,
}
impl<M> std::fmt::Debug for RecoveryFailure<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RecoveryFailure")
            .field("kind", &self.kind)
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

pub struct RecoveryDecision<M> {
    pub operation: OperationId,
    pub result: Result<RecoveryAdmission, RecoveryFailure<M>>,
    pub(super) reservation: Option<InputReservation>,
}

/// A committed continuation and its local owner; no new input was accepted.
#[derive(Debug)]
pub struct RecoveryAdmission {
    pub operation: OperationId,
    pub point: ion_core::RecoveryPoint,
}

/// No input in this batch was queued. Originals and metadata retain their order.
pub struct RejectedBatch<M> {
    pub inputs: Vec<(UserInput, M)>,
    pub reason: anyhow::Error,
}

impl<M> std::fmt::Debug for RejectedBatch<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RejectedBatch")
            .field("count", &self.inputs.len())
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl<M> std::fmt::Display for RejectedBatch<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.reason.fmt(f)
    }
}

impl<M> std::error::Error for RejectedBatch<M> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.reason.as_ref())
    }
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
    /// This queued input was never attempted because preceding work failed.
    PredecessorFailed,
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
/// Exact custody with no safe replay claim. The read-only receipt may still
/// resolve if a retained storage leaf outlives a failed root task.
pub struct UnresolvedInput<M> {
    pub session: uuid::Uuid,
    pub id: InputId,
    pub delivery: Delivery,
    pub operation: OperationId,
    pub input: UserInput,
    pub metadata: M,
    pub receipt: ion_core::InputReceipt,
    pub(super) reservation: Option<InputReservation>,
}

pub struct AcceptedMetadata<M> {
    pub id: InputId,
    pub acceptance: super::InputAcceptance,
    pub metadata: M,
    pub(super) reservation: Option<InputReservation>,
}

pub enum OperationOutcome {
    /// Native preflight failed or stopped before the executor was dispatched.
    /// This does not claim rollback of arbitrary work performed by preflight.
    NotDispatched(anyhow::Error),
    Turn(Result<String, AgentError>),
    Compact(Result<bool, AgentError>),
    Shell(Result<ToolOutput, SessionError>),
}
impl OperationOutcome {
    pub(super) fn failed(&self) -> bool {
        match self {
            Self::NotDispatched(_) => true,
            Self::Turn(r) => r.is_err(),
            Self::Compact(r) => r.is_err(),
            Self::Shell(r) => r.is_err(),
        }
    }
}

pub struct Settlement<M> {
    /// No fabricated user input or client correlation accompanies this operation.
    pub automated: bool,
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

/// Reliable result of user management, not a Core Turn settlement.
pub struct McpReconnectFinished<M> {
    pub operation: OperationId,
    pub server: String,
    pub metadata: M,
    pub result: Result<crate::McpReconnectReport>,
    /// Cancellation intent does not overwrite an already completed result.
    pub stop_requested: bool,
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
    BindingFinished(crate::binding::BindingFinished),
    RequestAdmission(RequestDecision<M>),
    RecoveryAdmission(RecoveryDecision<M>),
    Automated {
        operation: OperationId,
        turn: Option<u64>,
    },
    Input {
        id: InputId,
        delivery: Delivery,
        phase: InputPhase,
    },
    /// Local input/operation correlation, not a durable acceptance receipt.
    /// Ordinary queued work may still fail admission and return its original;
    /// keyed admission has already committed before this notification.
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
    /// Live question custody changed. State is pulled through the responder;
    /// this invalidation carries no payload and is omitted from snapshots.
    QuestionChanged,
    Returned(ReturnedInput<M>),
    Unresolved(UnresolvedInput<M>),
    Settled(Settlement<M>),
    McpReconnectFinished(McpReconnectFinished<M>),
    /// No work remains now; admission remains open unless shutdown was requested.
    Idle,
}
impl<M> ControlUpdate<M> {
    pub(super) fn release(&mut self) -> usize {
        match self {
            // Binding custody releases its own result slot on transfer.
            Self::BindingFinished(_) => 0,
            Self::RequestAdmission(decision) => usize::from(decision.reservation.take().is_some()),
            Self::RecoveryAdmission(decision) => usize::from(decision.reservation.take().is_some()),
            Self::McpReconnectFinished(finished) => {
                finished.reservation.take();
                1
            }
            Self::Returned(input) => {
                input.reservation.take();
                1
            }
            Self::Unresolved(input) => {
                input.reservation.take();
                1
            }
            Self::Settled(settlement) => {
                settlement.reservation.take();
                for input in &mut settlement.accepted {
                    input.reservation.take();
                }
                settlement.accepted.len()
                    + usize::from(settlement.metadata.is_some())
                    + usize::from(settlement.automated)
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
    // Synchronous intake for the finite tail of an already joined publisher.
    // Unlike polling recv(), try_recv() cannot yield to Tokio's cooperative
    // budget while published messages are still ready.
    pub(super) fn try_next(&mut self) -> Option<Observation> {
        if std::mem::take(&mut self.initial) {
            return Some(Observation::Gap { skipped: 0 });
        }
        loop {
            match self.receiver.try_recv() {
                Ok(observation) => {
                    if let Some(observation) = self.filter_activity(observation) {
                        return Some(observation);
                    }
                }
                Err(broadcast::error::TryRecvError::Lagged(skipped)) => {
                    self.suppress = true;
                    return Some(Observation::Gap { skipped });
                }
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => return None,
            }
        }
    }

    fn filter_activity(&mut self, observation: Observation) -> Option<Observation> {
        if matches!(observation, Observation::Gap { .. }) {
            self.suppress = true;
        }
        if let Observation::Event { event, .. } = &observation {
            let boundary = matches!(
                event.as_ref(),
                AgentEvent::TurnAccepted { .. }
                    | AgentEvent::CompletionCommitted { .. }
                    | AgentEvent::AssistantCommitted { .. }
                    | AgentEvent::ResponseRestarted
                    | AgentEvent::TurnEnded { .. }
            );
            if boundary {
                self.suppress = false;
            }
            if self.suppress {
                return None;
            }
        }
        Some(observation)
    }

    pub async fn next(&mut self) -> Option<Observation> {
        if std::mem::take(&mut self.initial) {
            return Some(Observation::Gap { skipped: 0 });
        }
        loop {
            match self.receiver.recv().await {
                Ok(observation) => {
                    if let Some(observation) = self.filter_activity(observation) {
                        return Some(observation);
                    }
                }
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
    pub cleanup_errors: Vec<SessionError>,
}
