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
    AgentError, AgentEvent, InputBudget, InputReceipt, InputReservation, InputStatus, Operation,
    PreparedInput, SessionError, SteeringInbox, ToolOutput,
};
use serde::Serialize;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

use crate::{SessionSetup, image_input::ImageSource, question::TurnStopScope};

mod admission;
pub use admission::preflight_messages;
mod completion;
pub use completion::CompletionTargetPlan;
mod custody;
use custody::{
    AdmissionObservations, Custody, Input, InputContent, InputState, PendingInput, SnapshotValue,
    turn_observer,
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

/// Host preparation custody, not another Core operation owner. Metadata and
/// originals never enter the worker output; the worker returns Core's actual
/// retained handle and durable admission fact together.
struct Admitting<M> {
    operation: OperationId,
    stop: CancellationToken,
    inbox: Arc<SteeringInbox<InputId>>,
    turn_stop: Option<crate::question::TurnStopBinding>,
    prior_stop_requested: bool,
    payload: AdmissionPayload<M>,
    observations: Arc<AdmissionObservations>,
    work: crate::preparation::Preparation<AdmissionOutput>,
}
enum AdmissionPayload<M> {
    Request {
        id: InputId,
        key: ion_core::RequestKey,
        original: UserInput,
        metadata: SnapshotValue<M>,
        reservation: InputReservation,
        inflight: InputReservation,
    },
    Recovery {
        point: ion_core::RecoveryPoint,
        metadata: SnapshotValue<M>,
        reservation: InputReservation,
    },
    Completion,
}
enum AdmissionOutput {
    Request(ion_core::RequestSubmission),
    Recovery {
        point: ion_core::RecoveryPoint,
        work: Operation<String, AgentError>,
    },
    Completion(Option<ion_core::CompletionSubmission>),
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
    Turn(Operation<String, AgentError>),
    Compact(Operation<bool, AgentError>),
    Shell(Operation<ToolOutput, SessionError>),
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
    pub(crate) binding: Option<crate::binding::BindingWork>,
    custody: Arc<Mutex<Custody<M>>>,
    budget: InputBudget,
    input_changes: Arc<tokio::sync::Notify>,
    active: Option<Active<M>>,
    admitting: Option<Admitting<M>>,
    preparing: Option<(InputId, crate::preparation::Preparation<ion_ai::Message>)>,
    /// An initial prompt has operation identity even while its images prepare.
    starting: Option<(InputId, OperationId)>,
    mcp_reconnect: Option<mcp::Reconnect<M>>,
    observations: broadcast::Sender<Observation>,
    observer: ControlObserver,
    stop_requested: bool,
    closed: bool,
    cleanup_errors: Vec<SessionError>,
    pub(crate) owner: uuid::Uuid,
    question_changes: Option<watch::Receiver<u64>>,
    question_seen: u64,
    snapshot_sequence: u64,
    shutdown_complete: bool,
}
fn lock<M>(custody: &Mutex<Custody<M>>) -> custody::Guard<'_, M> {
    custody::Guard::new(custody)
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
            binding: None,
            custody: Arc::new(Mutex::new(Custody::default())),
            budget: InputBudget::default(),
            input_changes: Arc::new(tokio::sync::Notify::new()),
            active: None,
            admitting: None,
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
            .or_else(|| self.admitting.as_ref().map(|admission| admission.operation))
            .or(self.starting.map(|(_, id)| id))
    }
    pub fn is_busy(&self) -> bool {
        self.state().busy
    }
    /// Derive client status from this owner rather than parallel frontend flags.
    pub fn state(&self) -> ControlState {
        self.reconcile_admissions();
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
                    self.admitting.as_ref().map(|admission| OperationState {
                        id: admission.operation,
                        turn: None,
                        kind: OperationKind::Turn,
                    })
                })
                .or_else(|| {
                    self.starting.map(|(_, id)| OperationState {
                        id,
                        turn: None,
                        kind: OperationKind::Turn,
                    })
                }),
            binding: self
                .binding
                .as_ref()
                .map(crate::binding::BindingWork::operation),
            mcp_reconnect: self
                .mcp_reconnect
                .as_ref()
                .map(|reconnect| McpReconnectState {
                    id: reconnect.id,
                    stop_requested: reconnect.work.is_stopped(),
                }),
            busy: self.binding_running()
                || self.mcp_reconnect.is_some()
                || self.active.is_some()
                || self.admitting.is_some()
                || self.starting.is_some()
                || self.preparing.is_some()
                || !custody.inputs.is_empty()
                || self.wake_is_runnable(custody),
            completion_delivery: self.completion_delivery(),
            preparing_input: self.preparing.is_some() || self.admitting.is_some(),
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
    fn reconcile_admissions(&self) {
        if let Some(active) = &self.active {
            self.reconcile_active(active);
        }
    }

    fn reconcile_active(&self, active: &Active<M>) {
        // Drain steering first, then sample the root: a committed steer must
        // never become visible in Host custody ahead of its committed root.
        let acknowledgements = active.inbox.take_committed();
        let mut custody = lock(&self.custody);
        if let Some(root) = active.root
            && let Some(InputContent::Transferred { receipt, operation }) = custody
                .inputs
                .get(&root)
                .and_then(Input::pending)
                .map(|pending| &pending.content)
            && let InputStatus::Committed(commit) = receipt.status()
        {
            assert_eq!(commit.session, self.session().id());
            let operation = *operation;
            custody.accept(root, operation, commit.turn);
        }
        for acknowledgement in &acknowledgements {
            let InputStatus::Committed(commit) = acknowledgement.receipt().status() else {
                unreachable!("Core queues only committed acknowledgements")
            };
            assert_eq!(commit.session, self.session().id());
            let id = custody
                .inputs
                .iter()
                .find_map(|(&id, input)| {
                    let InputContent::Transferred { receipt, .. } = &input.pending()?.content
                    else {
                        return None;
                    };
                    receipt.same_input(acknowledgement.receipt()).then_some(id)
                })
                .expect("Core acknowledgement retains its exact Host input identity");
            custody.accept(id, active.id, commit.turn);
        }
        drop(custody);
        drop(acknowledgements);
    }

    fn work_stop(&self) -> CancellationToken {
        self.background()
            .map_or_else(CancellationToken::new, |owner| owner.work_stop())
    }
    fn ensure_host_replaceable(&self) -> Result<()> {
        if let Some(owner) = self.background() {
            owner.ensure_open()?;
        }
        ensure!(!self.closed, "session control is closed");
        ensure!(
            self.binding.is_none()
                && self.active.is_none()
                && self.admitting.is_none()
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
                    ControlUpdate::RequestAdmission(_)
                        | ControlUpdate::RecoveryAdmission(_)
                        | ControlUpdate::Returned(_)
                        | ControlUpdate::Unresolved(_)
                        | ControlUpdate::Settled(_)
                        | ControlUpdate::McpReconnectFinished(_)
                ))),
            "drain returned inputs and settlements before replacing the binding or starting new work"
        );
        Ok(())
    }

    pub(crate) fn ensure_replaceable(&self) -> Result<()> {
        self.ensure_host_replaceable()?;
        // Surface Core's closed/panic/unknown-effect state before acknowledging
        // replacement or new work, including work started by another embedder.
        let _idle = self.setup.session.reserve_idle()?;
        Ok(())
    }

    fn binding_running(&self) -> bool {
        self.binding
            .as_ref()
            .is_some_and(crate::binding::BindingWork::is_running)
    }

    /// Binding native preflight owns its Core leases on the blocking worker.
    /// Host checks here must not obtain a Session lease or touch its Store.
    pub(crate) fn ensure_binding_start(&self, reload: bool) -> Result<()> {
        if reload {
            if let Some(owner) = self.background() {
                owner.ensure_open()?;
            }
            ensure!(!self.closed, "session control is closed");
            ensure!(
                self.binding.is_none(),
                "join or take the previous binding result first"
            );
        } else {
            self.ensure_host_replaceable()?;
        }
        ensure!(
            lock(&self.custody).occupied < MAX_ADMISSIONS,
            "session result/admission capacity is full"
        );
        Ok(())
    }

    pub(crate) fn reserve_binding_result(&mut self) -> Result<()> {
        let mut custody = lock(&self.custody);
        ensure!(
            custody.occupied < MAX_ADMISSIONS,
            "session result/admission capacity is full"
        );
        custody.occupied += 1;
        Ok(())
    }

    pub(crate) fn release_binding_result(&mut self) {
        let mut custody = lock(&self.custody);
        custody.occupied = custody
            .occupied
            .checked_sub(1)
            .expect("binding result retains its reserved slot");
    }

    async fn drive(&mut self, activity: ActivityRetention) -> Option<Observation> {
        enum Progress {
            Binding(Result<crate::binding::BindingDelta>),
            Admitted(Result<AdmissionOutput>),
            Prepared(Result<Message>),
            Settled(OperationOutcome),
            McpReconnected(Result<crate::McpReconnectReport>),
            Activity(Observation),
            Question,
            Input,
        }
        if !self.binding_running()
            && self.admitting.is_none()
            && self.preparing.is_none()
            && self.active.is_none()
            && self.mcp_reconnect.is_none()
        {
            return None;
        }
        // Preparation and newly queued intent precede activity, so a busy stream
        // cannot starve admission. Drain ready observations before completion.
        let progress = tokio::select! {
            biased;
            result = async { self.binding.as_mut().expect("enabled binding preparation").join().await },
                if self.binding_running() => Progress::Binding(result),
            result = async { self.admitting.as_mut().expect("enabled admission").work.join().await },
                if self.admitting.is_some() => Progress::Admitted(result),
            result = async { self.preparing.as_mut().expect("enabled preparation").1.join().await },
                if self.preparing.is_some() => Progress::Prepared(result),
            result = async { self.mcp_reconnect.as_mut().expect("enabled MCP reconnect").work.join().await },
                if self.mcp_reconnect.is_some() => Progress::McpReconnected(result),
            () = self.input_changes.notified() => Progress::Input,
            Some(observation) = self.observer.next(), if self.admitting.is_none() => Progress::Activity(observation),
            result = async { self.active.as_mut().expect("enabled operation").work.join().await },
                if self.active.is_some() => Progress::Settled(result),
            Some(()) = async {
                self.question_changes.as_mut().expect("enabled question observer").changed().await.ok()
            }, if self.question_changes.is_some() && self.admitting.is_none() => Progress::Question,
        };
        // No await after consuming either completion until it is safely retained.
        self.reconcile_admissions();
        match progress {
            Progress::Binding(result) => self.finish_binding(result),
            Progress::Admitted(result) => self.finish_admission(result),
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
        self.reconcile_active(&active);
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
        let returned: Vec<_> = active
            .inbox
            .take_uncommitted()
            .into_iter()
            .map(|input| {
                let (_, id, reservation) = input.into_parts();
                drop(reservation);
                id
            })
            .collect();
        self.reconcile_active(&active);
        let mut custody = lock(&self.custody);
        for id in returned {
            custody.return_input(
                id,
                if self.stop_requested {
                    ReturnReason::Stopped
                } else {
                    ReturnReason::TurnEnded
                },
            );
        }
        // Even a failed publisher can leave positive receipts. Recover only
        // those facts, in their durable order; queue absence proves nothing.
        let dispositions: Vec<_> = custody
            .inputs
            .iter()
            .filter_map(|(&id, input)| {
                let InputContent::Transferred { receipt, operation } = &input.pending()?.content
                else {
                    return None;
                };
                assert_eq!(*operation, active.id);
                Some((id, receipt.status()))
            })
            .collect();
        let mut positive: Vec<_> = dispositions
            .iter()
            .filter_map(|(id, status)| {
                let InputStatus::Committed(commit) = status else {
                    return None;
                };
                Some((*commit, *id, active.id))
            })
            .collect();
        positive.sort_by_key(|(commit, _, _)| commit.entry);
        for (commit, id, operation) in positive {
            assert_eq!(commit.session, self.session().id());
            custody.accept(id, operation, commit.turn);
        }
        let turn = self.active_turn(&active, &custody);
        for (id, status) in dispositions {
            if matches!(status, InputStatus::Committed(_)) {
                continue;
            }
            if status == InputStatus::NotAccepted {
                custody.return_input(
                    id,
                    if self.stop_requested {
                        ReturnReason::Stopped
                    } else {
                        ReturnReason::NotAccepted
                    },
                );
            } else {
                custody.unresolved(id, self.session().id());
            }
        }
        let accepted = custody.take_accepted();
        assert!(
            active
                .root
                .is_none_or(|root| !custody.inputs.contains_key(&root)),
            "joined root custody is resolved by its receipt"
        );
        if outcome.failed() {
            for pending in custody.inputs.values_mut().filter_map(Input::pending_mut) {
                pending.withdraw(ReturnReason::PredecessorFailed);
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
            if self.admitting.is_none() && self.take_question_change() {
                return ControlUpdate::QuestionChanged;
            }
            if !self.is_busy() {
                return ControlUpdate::Idle;
            }
            // drive retains every completion before it can yield. Selecting away
            // from it only abandons a borrowed wait, never a task or local outcome.
            if let Some(observation) = self.drive(ActivityRetention::Retain).await {
                // Newly discovered commit receipts precede this progress item.
                lock(&self.custody).push_update(ControlUpdate::Activity(observation));
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
        if let Some(binding) = &self.binding {
            binding.request_stop();
        }
        if let Some(admitting) = &self.admitting {
            admitting.stop.cancel();
        }
        if let Some(reconnect) = &self.mcp_reconnect {
            reconnect.work.request_stop();
        }
        if let Some(active) = &self.active {
            active.work.stop();
        }
        self.completions.close();
        lock(&self.custody).lost_control();
        if let Some(questions) = &self.setup.questions {
            questions.close(self.owner, "control_dropped");
        }
        let turn_stop = self
            .active
            .as_ref()
            .and_then(|active| active.turn_stop)
            .or_else(|| {
                self.admitting.as_ref().and_then(|admission| {
                    admission.turn_stop.as_ref().map(|binding| binding.scope())
                })
            });
        if let (Some(questions), Some(scope)) = (&self.setup.questions, turn_stop) {
            questions.clear_turn_stop(self.owner, self.setup.session.id(), scope);
        }
        if let Some((_, preparation)) = &self.preparing {
            preparation.request_stop();
        }
    }
}
