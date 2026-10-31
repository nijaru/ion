//! Shared input custody and lifecycle control. Core owns every execution task.
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result, ensure};
use ion_ai::Message;
use ion_core::{
    AcceptedInput, CodingAgentError, CodingAgentEvent as AgentEvent, CodingSessionError,
    CodingToolOutput, InputBudget, InputReservation, Operation, SteeringInbox,
};
use serde::Serialize;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

use crate::{SessionSetup, image_input::ImageSource, question::TurnStopScope};

mod admission;
pub use admission::preflight_messages;
mod completion;
mod custody;
use custody::{
    Custody, Input, InputContent, InputState, PendingInput, SnapshotValue, turn_observer,
};
mod input;
mod mcp;
mod question;
mod snapshot;
pub use snapshot::*;
mod steering;
pub use steering::*;
use steering::{SteeringMonitor, SteeringTarget};
#[cfg(test)]
mod tests;
mod types;
pub use types::*;

const MAX_ADMISSIONS: usize = 64;
const OBSERVATION_CAPACITY: usize = 64;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
fn identity() -> u64 {
    NEXT_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("Host identity exhausted")
}

struct Active<M> {
    id: OperationId,
    turn: Option<u64>,
    root: Option<InputId>,
    work: Work,
    inbox: Arc<SteeringInbox<InputId>>,
    automated: bool,
    metadata: Option<(M, InputReservation)>,
    /// Exact root stop bound to question custody while this Turn owns control.
    turn_stop: Option<TurnStopScope>,
}
enum ActivityRetention {
    Retain,
    Discard,
}

enum Work {
    Turn(Operation<String, CodingAgentError>),
    Compact(Operation<bool, CodingAgentError>),
    Shell(Operation<CodingToolOutput, CodingSessionError>),
}
impl Work {
    fn stop(&self) {
        match self {
            Self::Turn(w) => w.request_stop(),
            Self::Compact(w) => w.request_stop(),
            Self::Shell(w) => w.request_stop(),
        }
    }
    async fn join(&mut self) -> OperationOutcome {
        match self {
            Self::Turn(w) => OperationOutcome::Turn(w.await),
            Self::Compact(w) => OperationOutcome::Compact(w.await),
            Self::Shell(w) => OperationOutcome::Shell(w.await),
        }
    }
}

/// One controlling owner, independent of passive clients. All asynchronous waits borrow retained handles; dropping
/// a next/shutdown future cannot discard a transition, error, or recoverable input.
pub struct SessionControl<M = ()> {
    pub(crate) setup: SessionSetup,
    pub(crate) completions: Arc<crate::completion::CompletionReceiver>,
    custody: Arc<Mutex<Custody<M>>>,
    budget: InputBudget,
    input_changes: Arc<tokio::sync::Notify>,
    active: Option<Active<M>>,
    preparing: Option<(InputId, crate::preparation::Preparation<ion_ai::Message>)>,
    /// An initial prompt has operation identity even while its images prepare.
    starting: Option<(InputId, OperationId)>,
    mcp_reconnect: Option<mcp::Reconnect<M>>,
    observations: broadcast::Sender<Observation>,
    observer: ControlObserver,
    stop_requested: bool,
    closed: bool,
    cleanup_errors: Vec<CodingSessionError>,
    pub(crate) owner: uuid::Uuid,
    question_changes: Option<watch::Receiver<u64>>,
    question_seen: u64,
    snapshot_sequence: u64,
    shutdown_complete: bool,
}
fn lock<M>(custody: &Mutex<Custody<M>>) -> MutexGuard<'_, Custody<M>> {
    custody
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl<M: Serialize + Send + 'static> SessionControl<M> {
    pub(crate) fn from_setup(setup: SessionSetup) -> Self {
        let (observations, receiver) = broadcast::channel(OBSERVATION_CAPACITY);
        let owner = uuid::Uuid::now_v7();
        let completions = crate::completion::CompletionReceiver::new(setup.session.id());
        completions.set_policy(setup.initial_completion_delivery);
        if let Some(application) = setup.owner() {
            application.completions().register(owner, &completions);
        }
        let question_changes = setup.questions.as_ref().map(|questions| {
            questions
                .bind_control(owner, setup.session.id())
                .expect("question custody is owned by one SessionSetup");
            questions.subscribe()
        });
        let question_seen = question_changes
            .as_ref()
            .map_or(0, |changes| *changes.borrow());
        Self {
            setup,
            completions,
            custody: Arc::new(Mutex::new(Custody::default())),
            budget: InputBudget::default(),
            input_changes: Arc::new(tokio::sync::Notify::new()),
            active: None,
            preparing: None,
            starting: None,
            mcp_reconnect: None,
            observations,
            observer: ControlObserver {
                receiver,
                suppress: false,
                initial: false,
            },
            stop_requested: false,
            closed: false,
            cleanup_errors: Vec::new(),
            owner,
            question_changes,
            question_seen,
            snapshot_sequence: 0,
            shutdown_complete: false,
        }
    }

    pub fn observe(&self) -> ControlObserver {
        ControlObserver {
            receiver: self.observations.subscribe(),
            suppress: true,
            initial: true,
        }
    }
    pub fn current_operation(&self) -> Option<OperationId> {
        self.active
            .as_ref()
            .map(|active| active.id)
            .or(self.starting.map(|(_, id)| id))
    }
    pub fn is_busy(&self) -> bool {
        self.state().busy
    }
    /// Derive client status from this owner rather than parallel frontend flags.
    pub fn state(&self) -> ControlState {
        let custody = lock(&self.custody);
        self.state_with(&custody)
    }
    fn state_with(&self, custody: &Custody<M>) -> ControlState {
        let active_root = self.active.as_ref().and_then(|active| active.root);
        let mut pending_steering = 0;
        let mut pending_follow_ups = 0;
        for (&id, input) in &custody.inputs {
            let Some(pending) = input.pending() else {
                continue;
            };
            if pending.withdrawn.is_some() || active_root == Some(id) {
                continue;
            }
            match input.delivery {
                Delivery::Steer(_) => pending_steering += 1,
                Delivery::FollowUp => pending_follow_ups += 1,
                Delivery::Start => {}
            }
        }
        ControlState {
            operation: self
                .active
                .as_ref()
                .map(|active| OperationState {
                    id: active.id,
                    turn: self.active_turn(active, custody),
                    kind: match active.work {
                        Work::Turn(_) => OperationKind::Turn,
                        Work::Compact(_) => OperationKind::Compact,
                        Work::Shell(_) => OperationKind::Shell,
                    },
                })
                .or_else(|| {
                    self.starting.map(|(_, id)| OperationState {
                        id,
                        turn: None,
                        kind: OperationKind::Turn,
                    })
                }),
            mcp_reconnect: self
                .mcp_reconnect
                .as_ref()
                .map(|reconnect| McpReconnectState {
                    id: reconnect.id,
                    stop_requested: reconnect.work.is_stopped(),
                }),
            busy: self.mcp_reconnect.is_some()
                || self.active.is_some()
                || self.starting.is_some()
                || self.preparing.is_some()
                || !custody.inputs.is_empty()
                || self.wake_is_runnable(custody),
            completion_delivery: self.completion_delivery(),
            preparing_input: self.preparing.is_some(),
            pending_steering,
            pending_follow_ups,
            stop_requested: self.stop_requested,
            closed: self.closed,
            shutdown_complete: self.shutdown_complete,
        }
    }
    fn active_turn(&self, active: &Active<M>, custody: &Custody<M>) -> Option<u64> {
        active.turn.or_else(|| {
            active
                .root
                .and_then(|id| custody.inputs.get(&id))
                .and_then(|input| input.acceptance().map(|accepted| accepted.turn))
        })
    }
    fn work_stop(&self) -> CancellationToken {
        self.background()
            .map_or_else(CancellationToken::new, |owner| owner.work_stop())
    }
    pub(crate) fn ensure_replaceable(&self) -> Result<()> {
        if let Some(owner) = self.background() {
            owner.ensure_open()?;
        }
        ensure!(!self.closed, "session control is closed");
        ensure!(
            self.active.is_none()
                && self.starting.is_none()
                && self.preparing.is_none()
                && self.mcp_reconnect.is_none()
                && lock(&self.custody).inputs.is_empty(),
            "session control has active work or pending inputs"
        );
        ensure!(
            !lock(&self.custody)
                .updates
                .iter()
                .any(|update| update.with(|update| matches!(
                    update,
                    ControlUpdate::Returned(_)
                        | ControlUpdate::Settled(_)
                        | ControlUpdate::McpReconnectFinished(_)
                ))),
            "drain returned inputs and settlements before replacing the binding or starting new work"
        );
        // Surface Core's closed/panic/unknown-effect state before acknowledging
        // replacement or new work, including work started by another embedder.
        let _idle = self.setup.session.reserve_idle()?;
        Ok(())
    }

    async fn drive(&mut self, activity: ActivityRetention) -> Option<Observation> {
        enum Progress {
            Prepared(Result<Message>),
            Settled(OperationOutcome),
            McpReconnected(Result<crate::McpReconnectReport>),
            Activity(Observation),
            Question,
            Input,
        }
        if self.preparing.is_none() && self.active.is_none() && self.mcp_reconnect.is_none() {
            return None;
        }
        // Preparation and newly queued intent precede activity, so a busy stream
        // cannot starve admission. Drain ready observations before completion.
        let progress = tokio::select! {
            biased;
            result = async { self.preparing.as_mut().expect("enabled preparation").1.join().await },
                if self.preparing.is_some() => Progress::Prepared(result),
            result = async { self.mcp_reconnect.as_mut().expect("enabled MCP reconnect").work.join().await },
                if self.mcp_reconnect.is_some() => Progress::McpReconnected(result),
            () = self.input_changes.notified() => Progress::Input,
            Some(observation) = self.observer.next() => Progress::Activity(observation),
            result = async { self.active.as_mut().expect("enabled operation").work.join().await },
                if self.active.is_some() => Progress::Settled(result),
            Some(()) = async {
                self.question_changes.as_mut().expect("enabled question observer").changed().await.ok()
            }, if self.question_changes.is_some() => Progress::Question,
        };
        // No await after consuming either completion until it is safely retained.
        match progress {
            Progress::Prepared(result) => self.finish_preparation(result),
            Progress::Settled(result) => self.finish_operation(result, activity),
            Progress::McpReconnected(result) => self.finish_mcp_reconnect(result),
            Progress::Activity(observation) => return Some(observation),
            Progress::Question | Progress::Input => {}
        }
        self.advance();
        None
    }

    fn finish_operation(&mut self, outcome: OperationOutcome, activity: ActivityRetention) {
        lock(&self.custody).steering_target = None;
        let active = self.active.take().expect("retained Core operation");
        if outcome.failed() {
            self.completions
                .revoke(ion_core::CompletionRevocation::OperationFailed);
        }
        self.clear_turn_stop(active.turn_stop);
        // Publication can race the select's earlier observer poll. The joined
        // operation has finished publishing: retain its bounded ready tail before
        // settlement, using the same lag/suffix filtering as ordinary intake.
        // Snapshot maintenance and shutdown may discard activity instead.
        if matches!(activity, ActivityRetention::Retain) {
            let mut custody = lock(&self.custody);
            while let Some(observation) = self.observer.try_next() {
                custody.push_update(ControlUpdate::Activity(observation));
            }
        }
        // Never leak a previous operation's unread stream into the next Turn.
        let skipped = self.observer.receiver.len();
        self.observer = ControlObserver {
            receiver: self.observations.subscribe(),
            suppress: false,
            initial: false,
        };
        if skipped > 0 {
            lock(&self.custody).push_update(ControlUpdate::Activity(Observation::Gap {
                skipped: u64::try_from(skipped).expect("observation capacity fits u64"),
            }));
        }
        let returned = active.inbox.take_uncommitted();
        let mut custody = lock(&self.custody);
        let turn = self.active_turn(&active, &custody);
        for input in returned {
            let (_, id, reservation) = input.into_parts();
            drop(reservation);
            custody.steering_order.retain(|queued| *queued != id);
            custody.return_input(
                id,
                if self.stop_requested {
                    ReturnReason::Stopped
                } else {
                    ReturnReason::TurnEnded
                },
            );
        }
        // Core removes an inbox entry only after persistence. Once joined, any
        // transferred entry absent from the returned inbox is committed even if
        // an observer failed before publishing its acknowledgement. Never return
        // that guidance as unaccepted or make it eligible for replay.
        while let Some(id) = custody.steering_order.pop_front() {
            custody.accept(
                id,
                active.id,
                turn.expect("consumed steering has an acknowledged Turn"),
            );
        }
        let accepted_ids: Vec<_> = custody
            .inputs
            .iter()
            .filter_map(|(&id, input)| input.acceptance().is_some().then_some(id))
            .collect();
        let mut accepted = Vec::new();
        for id in accepted_ids {
            let input = custody.inputs.remove(&id).unwrap();
            let acceptance = input
                .acceptance()
                .expect("accepted metadata retains its Turn");
            accepted.push(AcceptedMetadata {
                id,
                metadata: input.metadata.into_inner(),
                acceptance,
                reservation: Some(input.reservation),
            });
        }
        if let Some(root) = active.root
            && custody.inputs.contains_key(&root)
        {
            custody.return_input(
                root,
                if self.stop_requested {
                    ReturnReason::Stopped
                } else {
                    ReturnReason::NotAccepted
                },
            );
        }
        if outcome.failed() {
            for pending in custody.inputs.values_mut().filter_map(Input::pending_mut) {
                pending.withdrawn = Some(ReturnReason::Stopped);
            }
            if let Some((_, preparation)) = &self.preparing {
                preparation.request_stop();
            }
        }
        let (metadata, reservation) = match active.metadata {
            Some((metadata, reservation)) => (Some(metadata), Some(reservation)),
            None => (None, None),
        };
        custody.push_update(ControlUpdate::Settled(Settlement {
            operation: active.id,
            turn,
            automated: active.automated,
            outcome,
            stop_requested: self.stop_requested,
            accepted,
            metadata,
            reservation,
        }));
    }

    /// Return custody updates before polling live activity, retaining an operation's
    /// ready activity tail ahead of its settlement. There is no background Host
    /// execution loop; Core and preparation continue independently.
    pub async fn next(&mut self) -> ControlUpdate<M> {
        loop {
            self.advance();
            if let Some(update) = self.take_update() {
                return update;
            }
            if self.take_question_change() {
                return ControlUpdate::QuestionChanged;
            }
            if !self.is_busy() {
                return ControlUpdate::Idle;
            }
            // drive retains every completion before it can yield. Selecting away
            // from it only abandons a borrowed wait, never a task or local outcome.
            if let Some(observation) = self.drive(ActivityRetention::Retain).await {
                return ControlUpdate::Activity(observation);
            }
        }
    }

    /// Close admission and request stop without waiting or consuming results.
    pub fn request_shutdown(&mut self) {
        self.closed = true;
        if let Some(questions) = &self.setup.questions {
            questions.close(self.owner, "shutdown");
        }
        self.request_stop();
    }

    /// Join shutdown without consuming recovery results or cleanup failures.
    /// Cancelling this wait retains closure, handles and all lifecycle custody.
    pub async fn join_shutdown(&mut self) {
        self.request_shutdown();
        while self.is_busy() {
            self.advance();
            if self.is_busy() {
                drop(self.drive(ActivityRetention::Discard).await);
            }
        }
        if !self.shutdown_complete {
            if let Err(error) = self.setup.session.shutdown().await {
                self.cleanup_errors.push(error);
            }
            self.shutdown_complete = true;
        }
    }

    /// Join, then explicitly transfer all remaining recovery results to the caller.
    pub async fn shutdown(&mut self) -> ShutdownReport<M> {
        self.join_shutdown().await;
        let mut updates = Vec::new();
        while let Some(update) = self.take_update() {
            updates.push(update);
        }
        ShutdownReport {
            updates,
            cleanup_errors: std::mem::take(&mut self.cleanup_errors),
        }
    }
}

impl<M> Drop for SessionControl<M> {
    fn drop(&mut self) {
        if let Some(reconnect) = &self.mcp_reconnect {
            reconnect.work.request_stop();
        }
        if let Some(active) = &self.active {
            active.work.stop();
        }
        self.completions.close();
        let mut custody = lock(&self.custody);
        custody.steering_target = None;
        for input in custody.inputs.values() {
            if input.pending().is_some()
                && let Some(status) = &input.steering_status
            {
                status.publish(SteeringStatus::Unknown);
            }
        }
        drop(custody);
        if let Some(questions) = &self.setup.questions {
            questions.close(self.owner, "control_dropped");
        }
        if let Some(active) = &self.active
            && let (Some(questions), Some(scope)) = (&self.setup.questions, active.turn_stop)
        {
            questions.clear_turn_stop(self.owner, self.setup.session.id(), scope);
        }
        if let Some((_, preparation)) = &self.preparing {
            preparation.request_stop();
        }
    }
}
