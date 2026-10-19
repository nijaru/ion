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

use crate::{SessionSetup, image_input::ImageSource};

mod admission;
mod input;
mod question;
mod snapshot;
pub use snapshot::*;
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

struct Input<M> {
    delivery: Delivery,
    original: Option<UserInput>,
    metadata: M,
    reservation: InputReservation,
    prepared: Option<AcceptedInput<InputId>>,
    inflight: Option<InputReservation>,
    instructions: String,
    expanded: Option<String>,
    acceptance: Option<InputAcceptance>,
    withdrawn: Option<ReturnReason>,
}
struct Custody<M> {
    inputs: BTreeMap<InputId, Input<M>>,
    updates: VecDeque<ControlUpdate<M>>,
    steering_order: VecDeque<InputId>,
    occupied: usize,
}
impl<M> Default for Custody<M> {
    fn default() -> Self {
        Self {
            inputs: BTreeMap::new(),
            updates: VecDeque::new(),
            steering_order: VecDeque::new(),
            occupied: 0,
        }
    }
}
impl<M> Custody<M> {
    fn accept(&mut self, id: InputId, operation: OperationId, turn: u64) {
        let input = self
            .inputs
            .get_mut(&id)
            .expect("accepted input retains Host custody");
        input.acceptance = Some(InputAcceptance { operation, turn });
        input.withdrawn = None;
        // Crucially before any fallible/provisional publication. Metadata remains
        // separately budgeted until reliable settlement delivery.
        input.original = None;
        input.prepared = None;
        input.inflight = None;
        input.expanded = None;
        input.instructions = String::new();
        self.updates.push_back(ControlUpdate::Accepted {
            id,
            operation,
            turn,
        });
    }
    fn return_input(&mut self, id: InputId, reason: ReturnReason) {
        let input = self
            .inputs
            .remove(&id)
            .expect("returned input retains Host custody");
        debug_assert!(input.acceptance.is_none());
        self.updates
            .push_back(ControlUpdate::Returned(ReturnedInput {
                id,
                delivery: input.delivery,
                input: input.original.expect("unaccepted original"),
                metadata: input.metadata,
                reason,
                preparation_error: None,
                reservation: Some(input.reservation),
            }));
    }
    fn pop(&mut self) -> Option<ControlUpdate<M>> {
        let mut update = self.updates.pop_front()?;
        self.occupied -= update.release();
        Some(update)
    }
}

fn turn_observer<M: Send + 'static>(
    custody: Arc<Mutex<Custody<M>>>,
    observations: broadcast::Sender<Observation>,
    root: Option<InputId>,
    operation: OperationId,
) -> impl FnMut(AgentEvent) + Send + 'static {
    move |event| {
        match &event {
            AgentEvent::TurnAccepted { turn } => {
                lock(&custody).accept(root.expect("new Turn retains a root"), operation, *turn);
            }
            AgentEvent::SteeringCommitted { turn, .. } => {
                let mut custody = lock(&custody);
                let id = custody
                    .steering_order
                    .pop_front()
                    .expect("Core commits controlled FIFO steering");
                custody.accept(id, operation, *turn);
            }
            _ => {}
        }
        let _ = observations.send(Observation::Event {
            operation,
            event: Arc::new(event),
        });
    }
}

struct Active<M> {
    id: OperationId,
    turn: Option<u64>,
    root: Option<InputId>,
    work: Work,
    inbox: Arc<SteeringInbox<InputId>>,
    metadata: Option<(M, InputReservation)>,
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
    custody: Arc<Mutex<Custody<M>>>,
    budget: InputBudget,
    active: Option<Active<M>>,
    preparing: Option<(InputId, crate::preparation::Preparation)>,
    /// An initial prompt has operation identity even while its images prepare.
    starting: Option<(InputId, OperationId)>,
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
            custody: Arc::new(Mutex::new(Custody::default())),
            budget: InputBudget::default(),
            active: None,
            preparing: None,
            starting: None,
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
            if input.acceptance.is_some() || input.withdrawn.is_some() || active_root == Some(id) {
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
            busy: self.active.is_some()
                || self.starting.is_some()
                || self.preparing.is_some()
                || !custody.inputs.is_empty(),
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
                .and_then(|input| input.acceptance.map(|accepted| accepted.turn))
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
            !self.is_busy(),
            "session control has active work or pending inputs"
        );
        ensure!(
            !lock(&self.custody).updates.iter().any(|update| matches!(
                update,
                ControlUpdate::Returned(_) | ControlUpdate::Settled(_)
            )),
            "drain returned inputs and settlements before replacing the binding or starting new work"
        );
        // Surface Core's closed/panic/unknown-effect state before acknowledging
        // replacement or new work, including work started by another embedder.
        let _idle = self.setup.session.reserve_idle()?;
        Ok(())
    }

    async fn drive(&mut self) -> Option<Observation> {
        enum Progress {
            Prepared(Result<Message>),
            Settled(OperationOutcome),
            Activity(Observation),
            Question,
        }
        let progress = match (&mut self.preparing, &mut self.active) {
            // Drain bounded ready observations before consuming completion, so
            // healthy readers do not lose an entire fast operation's activity.
            // Preparation remains first: a busy stream cannot starve admission.
            (Some((_, preparation)), Some(active)) => tokio::select! {
                biased;
                result = preparation.join() => Progress::Prepared(result),
                Some(observation) = self.observer.next() => Progress::Activity(observation),
                result = active.work.join() => Progress::Settled(result),
                Some(()) = async {
                    match self.question_changes.as_mut() {
                        Some(changes) => changes.changed().await.ok(),
                        None => Some(()),
                    }
                }, if self.question_changes.is_some() => Progress::Question,
            },
            (Some((_, preparation)), None) => tokio::select! {
                biased;
                result = preparation.join() => Progress::Prepared(result),
                Some(observation) = self.observer.next() => Progress::Activity(observation),
            },
            (None, Some(active)) => tokio::select! {
                biased;
                Some(observation) = self.observer.next() => Progress::Activity(observation),
                result = active.work.join() => Progress::Settled(result),
                Some(()) = async {
                    match self.question_changes.as_mut() {
                        Some(changes) => changes.changed().await.ok(),
                        None => Some(()),
                    }
                }, if self.question_changes.is_some() => Progress::Question,
            },
            (None, None) => return None,
        };
        // No await after consuming either completion until it is safely retained.
        match progress {
            Progress::Prepared(result) => self.finish_preparation(result),
            Progress::Settled(result) => self.finish_operation(result),
            Progress::Activity(observation) => return Some(observation),
            Progress::Question => {}
        }
        self.advance();
        None
    }

    fn finish_operation(&mut self, outcome: OperationOutcome) {
        let active = self.active.take().expect("retained Core operation");
        // Settlement supersedes any unread provisional stream for this owner.
        // Explicitly invalidate it rather than leaking old deltas into the next Turn.
        let skipped = self.observer.receiver.len();
        self.observer = ControlObserver {
            receiver: self.observations.subscribe(),
            suppress: false,
            initial: false,
        };
        if skipped > 0 {
            lock(&self.custody)
                .updates
                .push_back(ControlUpdate::Activity(Observation::Gap {
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
        let accepted_ids: Vec<_> = custody
            .inputs
            .iter()
            .filter_map(|(&id, input)| input.acceptance.is_some().then_some(id))
            .collect();
        let mut accepted = Vec::new();
        for id in accepted_ids {
            let input = custody.inputs.remove(&id).unwrap();
            accepted.push(AcceptedMetadata {
                id,
                metadata: input.metadata,
                acceptance: input
                    .acceptance
                    .expect("accepted metadata retains its Turn"),
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
            for input in custody
                .inputs
                .values_mut()
                .filter(|input| input.acceptance.is_none())
            {
                input.withdrawn = Some(ReturnReason::Stopped);
            }
            if let Some((_, preparation)) = &self.preparing {
                preparation.request_stop();
            }
        }
        let (metadata, reservation) = match active.metadata {
            Some((metadata, reservation)) => (Some(metadata), Some(reservation)),
            None => (None, None),
        };
        custody
            .updates
            .push_back(ControlUpdate::Settled(Settlement {
                operation: active.id,
                turn,
                outcome,
                stop_requested: self.stop_requested,
                accepted,
                metadata,
                reservation,
            }));
    }

    /// Return reliable custody updates before provisional activity. There is no
    /// background Host execution loop; Core and preparation continue independently.
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
            if let Some(observation) = self.drive().await {
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
                drop(self.drive().await);
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
        if let Some(questions) = &self.setup.questions {
            questions.close(self.owner, "control_dropped");
        }
        if let Some(active) = &self.active {
            active.work.stop();
        }
        if let Some((_, preparation)) = &self.preparing {
            preparation.request_stop();
        }
    }
}
