//! The single session supervisor.
//!
//! One task owns admission decisions, turn scheduling and the `JoinSet` of
//! running turns. Work is bounded and joined explicitly, never detached: a
//! drive that panics parks its turn instead of restarting it, and a drive whose
//! generation has been superseded stops writing instead of racing the recovery
//! that replaced it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{FutureExt, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_util::sync::CancellationToken;

use super::command::{AdmissionReceipt, CancelReceipt, Request, SessionEvent, SubmitRequest};
use super::handle::SessionHandle;
use crate::attempt::{AttemptState, ModelAttempt, ModelStep};
use crate::config::ConversationConfig;
use crate::error::{Error, Result};
use crate::invocation::{InvocationOutcome, InvocationState};
use crate::limits::SessionLimits;
use crate::request::{RequestError, assemble};
use crate::store::sqlite::conversation::{ConfigureConversation, ReadCommit, ReadConversation};
use crate::store::sqlite::entry::{ReadEntries, ReadEntriesUpTo};
use crate::store::sqlite::input::{
    AdmitInput, Admitted, ReadInput, ReadQueuedInputs, WithdrawInput,
};
use crate::store::sqlite::turn::{
    AdmittedCall, AttemptStart, BeginStep, CancelOutcome, CancelTurn, CommitDispatch,
    CommitInvocationDispatch, CommitInvocationResult, CommitResponse, FinishTurn, PrepareAttempt,
    ReadTurnView, ReadUnfinishedTurn, ResolveInvocation, RetireAttempt, SettleAttempt,
    StartSuccessor, StepStart,
};
use crate::store::{Db, StoreError};
use crate::tool::{Stop, Tool, ToolOutcome, ToolRegistry};
use crate::turn::{PendingOutcome, Turn, TurnFailure, TurnOutcome, TurnPhase};
use crate::view::TurnView;
use crate::{CommitSeq, ConversationId, InvocationId, TurnId};

/// What a close request established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// Every local execution was joined and storage ownership was released.
    Closed,
    /// Work did not stop within the join grace. The supervisor still owns it,
    /// storage ownership is retained, and the caller must not treat the
    /// workspace as quiescent; another close retries the join.
    StillClosing { pending: usize },
}

impl CloseOutcome {
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }

    #[must_use]
    pub const fn pending(&self) -> usize {
        match self {
            Self::Closed => 0,
            Self::StillClosing { pending } => *pending,
        }
    }
}

/// One action handed to the supervisor to run.
///
/// A turn drive never owns a running action: it asks the supervisor to run it
/// and then waits for evidence, so a drive that stops waiting cannot drop the
/// action it started.
pub(crate) struct SpawnRequest {
    pub(crate) turn: TurnId,
    pub(crate) invocation: InvocationId,
    pub(crate) tool: Arc<dyn Tool>,
    pub(crate) call: ion_ai::ToolCall,
    pub(crate) stop: Stop,
    /// Where the waiting drive receives the evidence. A dropped receiver means
    /// the drive stopped waiting; the action keeps running either way, and its
    /// late report is published rather than used to revise a settled turn.
    pub(crate) evidence: oneshot::Sender<ToolOutcome>,
}

/// One finished action, reported through the supervisor's join set.
struct ExecutionEnd {
    turn: TurnId,
    invocation: InvocationId,
    outcome: ToolOutcome,
    /// Whether the drive that requested it was still waiting for it.
    awaited: bool,
}

/// Everything a drive needs that outlives one turn.
pub(crate) struct Shared {
    /// A client clone of the database queue.
    pub(crate) db: Db,
    pub(crate) services: Services,
    pub(crate) limits: SessionLimits,
    pub(crate) events: broadcast::Sender<SessionEvent>,
    /// A client clone of the request queue, used to hand a finished turn's
    /// successor back to the supervisor instead of spawning from a drive.
    pub(crate) requests: mpsc::Sender<Request>,
    /// Where a turn drive hands an action to the supervisor that owns it.
    pub(crate) spawns: mpsc::Sender<SpawnRequest>,
}

/// The host-provided services a session drives.
#[derive(Clone)]
pub struct Services {
    pub model: Arc<dyn ion_ai::ModelService>,
    pub tools: Arc<ToolRegistry>,
}

impl std::fmt::Debug for Services {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Services").finish_non_exhaustive()
    }
}

impl Services {
    #[must_use]
    pub fn new(model: Arc<dyn ion_ai::ModelService>, tools: Arc<ToolRegistry>) -> Self {
        Self { model, tools }
    }
}

/// The session owner task.
pub(crate) struct Supervisor {
    shared: Arc<Shared>,
    owner_db: Db,
    requests: mpsc::Receiver<Request>,
    spawns: mpsc::Receiver<SpawnRequest>,
    handle: SessionHandle,
    active: BTreeMap<TurnId, CancellationToken>,
    /// Turns a client asked to continue while a drive already held their slot.
    ///
    /// A drive can end without settling its turn (a parked phase hands control
    /// back to a client), so the request is remembered and re-checked when the
    /// drive that held the slot is reaped. Without it, a resume or resolution
    /// that lands in that window would be silently dropped.
    pending: BTreeSet<TurnId>,
    joins: JoinSet<(TurnId, bool)>,
    /// Actions this session owns. They are joined explicitly, never detached.
    executions: JoinSet<ExecutionEnd>,
    /// The stop handle for each owned action, so a close, a panicking drive or
    /// a second close attempt can still ask it to stop.
    stops: BTreeMap<InvocationId, (TurnId, Stop)>,
    /// Once set, new admission and dispatch are refused and only closing work
    /// continues.
    closing: bool,
    /// Whether the client request queue has closed; the run loop then only
    /// drains owned work instead of busy-polling a closed channel.
    requests_gone: bool,
}

impl Supervisor {
    pub(crate) fn new(
        shared: Arc<Shared>,
        owner_db: Db,
        requests: mpsc::Receiver<Request>,
        spawns: mpsc::Receiver<SpawnRequest>,
        handle: SessionHandle,
    ) -> Self {
        Self {
            shared,
            owner_db,
            requests,
            spawns,
            handle,
            active: BTreeMap::new(),
            pending: BTreeSet::new(),
            joins: JoinSet::new(),
            executions: JoinSet::new(),
            stops: BTreeMap::new(),
            closing: false,
            requests_gone: false,
        }
    }

    /// Serve requests until the session is closed.
    ///
    /// The session is usable on open without resuming anything: `Session::open`
    /// starts this service, and work starts only when a client asks for it.
    pub(crate) async fn run(mut self) {
        let mut closed = false;
        while !closed {
            if self.closing && self.joins.is_empty() && self.executions.is_empty() {
                break;
            }
            tokio::select! {
                biased;
                request = self.requests.recv(), if !self.requests_gone => {
                    match request {
                        None => {
                            // Clients are gone; owned work is not. Storage
                            // ownership outlives the last client that could
                            // have retried the join.
                            self.requests_gone = true;
                            if self.shutdown().await.is_closed() {
                                closed = true;
                            }
                        }
                        Some(Request::Close { reply }) => {
                            let outcome = self.shutdown().await;
                            let _ = reply.send(outcome);
                            closed = outcome.is_closed();
                        }
                        Some(request) => {
                            if self.closing {
                                refuse(request);
                            } else {
                                self.dispatch(request).await;
                            }
                        }
                    }
                }
                spawn = self.spawns.recv(), if !self.closing => {
                    if let Some(spawn) = spawn {
                        self.spawn_execution(spawn);
                    }
                }
                joined = self.joins.join_next(), if !self.joins.is_empty() => {
                    if let Some(joined) = joined {
                        self.finish_drive(joined);
                        self.drain_pending().await;
                    }
                }
                ended = self.executions.join_next(), if !self.executions.is_empty() => {
                    if let Some(ended) = ended {
                        self.finish_execution(ended);
                    }
                }
            }
        }
        self.owner_db.close().await;
        let _ = self.shared.events.send(SessionEvent::Closed);
    }

    /// Run an action this session owns.
    ///
    /// The stop handle lives here as well as in the task, so a close or a
    /// panicking drive can still ask an action to stop after its requester is
    /// gone.
    fn spawn_execution(&mut self, request: SpawnRequest) {
        let SpawnRequest {
            turn,
            invocation,
            tool,
            call,
            stop,
            evidence,
        } = request;
        self.stops.insert(invocation, (turn, stop.clone()));
        self.executions.spawn(async move {
            // A panicking action establishes nothing about what happened, and
            // it must not take the supervisor down with it.
            let outcome = std::panic::AssertUnwindSafe(async { tool.execute(&call, &stop).await })
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    ToolOutcome::Indeterminate(
                        "the action panicked; whether it happened is not established".to_owned(),
                    )
                });
            let awaited = evidence.send(outcome.clone()).is_ok();
            ExecutionEnd {
                turn,
                invocation,
                outcome,
                awaited,
            }
        });
    }

    /// Handle an action report with no waiting drive.
    ///
    /// The fact is published as evidence. It never revises a settled turn and
    /// never resumes continuation; durable retention for a possibly live
    /// operation belongs to the execution backend, not to this channel.
    fn finish_execution(
        &mut self,
        ended: std::result::Result<ExecutionEnd, tokio::task::JoinError>,
    ) {
        if let Ok(end) = ended {
            self.stops.remove(&end.invocation);
            if !end.awaited {
                let _ = self.shared.events.send(SessionEvent::InvocationEvidence {
                    turn: end.turn,
                    invocation: end.invocation,
                    outcome: end.outcome,
                });
            }
        }
    }

    async fn dispatch(&mut self, request: Request) {
        match request {
            Request::Submit { request, reply } => {
                let result = self.submit(request).await;
                let _ = reply.send(result);
            }
            Request::Cancel { turn, reply } => {
                let result = self.cancel(turn).await;
                let _ = reply.send(result);
            }
            Request::Resume {
                conversation,
                reply,
            } => {
                let result = self.resume(conversation).await;
                let _ = reply.send(result);
            }
            Request::Resolve { request, reply } => {
                let result = self.resolve(request).await;
                let _ = reply.send(result);
            }
            Request::Configure { request, reply } => {
                let result = self
                    .shared
                    .db
                    .run(ConfigureConversation {
                        conversation: request.conversation,
                        expected: request.expected,
                        config: request.config,
                    })
                    .await
                    .map_err(StoreError::into_error);
                let _ = reply.send(result);
            }
            Request::Turn { turn, reply } => {
                let result = self
                    .shared
                    .db
                    .run(ReadTurnView { turn })
                    .await
                    .map_err(StoreError::into_error);
                let _ = reply.send(result);
            }
            Request::Conversation {
                conversation,
                reply,
            } => {
                let result = self
                    .shared
                    .db
                    .run(ReadConversation { conversation })
                    .await
                    .map_err(StoreError::into_error);
                let _ = reply.send(result);
            }
            Request::Entries { query, reply } => {
                let result = self
                    .shared
                    .db
                    .run(ReadEntries {
                        conversation: query.conversation,
                        after: query.after,
                        limit: query.limit,
                    })
                    .await
                    .map_err(StoreError::into_error);
                let _ = reply.send(result);
            }
            Request::ReadInput { input, reply } => {
                let result = self
                    .shared
                    .db
                    .run(ReadInput { input })
                    .await
                    .map_err(StoreError::into_error);
                let _ = reply.send(result);
            }
            Request::Withdraw { input, reply } => {
                let result = self
                    .shared
                    .db
                    .run(WithdrawInput { input })
                    .await
                    .map_err(StoreError::into_error);
                let _ = reply.send(result);
            }
            Request::Close { .. } => unreachable!("close is handled by the run loop"),
        }
    }

    async fn submit(&mut self, request: SubmitRequest) -> Result<AdmissionReceipt> {
        let conversation = request.conversation.unwrap_or_else(|| self.handle.root());
        let admitted = self
            .shared
            .db
            .run(AdmitInput {
                conversation,
                sender: request.sender,
                mode: request.mode,
                request_key: request.request_key,
                body: crate::input::InputBody::Text(request.text),
                limits: self.shared.limits,
                now_unix_ms: now_unix_ms(),
            })
            .await
            .map_err(StoreError::into_error)?;
        match admitted {
            Admitted::Started {
                input,
                turn,
                commit,
            } => {
                self.spawn_drive(turn);
                Ok(AdmissionReceipt::started(input, turn, commit))
            }
            Admitted::Queued { input, commit } => Ok(AdmissionReceipt::queued(input, commit)),
            Admitted::Replay { input, turn } => {
                // A replay names work that was already accepted. If that work is
                // still unfinished and not being driven, this is the moment to
                // continue it; a replay never creates a second invocation.
                if let Some(turn) = turn
                    && self.turn_is_unfinished(turn).await?
                {
                    self.spawn_drive(turn);
                }
                Ok(AdmissionReceipt::replay(input, turn))
            }
        }
    }

    async fn cancel(&mut self, turn: TurnId) -> Result<CancelReceipt> {
        match self
            .shared
            .db
            .run(CancelTurn { turn })
            .await
            .map_err(StoreError::into_error)?
        {
            CancelOutcome::Marked(cancellation) => {
                if let Some(token) = self.active.get(&turn) {
                    token.cancel();
                }
                Ok(CancelReceipt {
                    turn,
                    generation: Some(cancellation.generation),
                    outcome: None,
                })
            }
            CancelOutcome::AlreadyTerminal => {
                let outcome = self
                    .shared
                    .db
                    .run(ReadTurnView { turn })
                    .await
                    .map_err(StoreError::into_error)?
                    .and_then(|view| view.turn.outcome);
                Ok(CancelReceipt {
                    turn,
                    generation: None,
                    outcome,
                })
            }
        }
    }

    async fn resume(&mut self, conversation: ConversationId) -> Result<Option<TurnId>> {
        let unfinished = self
            .shared
            .db
            .run(ReadUnfinishedTurn { conversation })
            .await
            .map_err(StoreError::into_error)?;
        if let Some(turn) = unfinished {
            self.spawn_drive(turn.id);
            return Ok(Some(turn.id));
        }
        // Nothing is running: answer the oldest queued input as its own turn.
        let queued = self
            .shared
            .db
            .run(ReadQueuedInputs {
                conversation,
                mode: None,
            })
            .await
            .map_err(StoreError::into_error)?;
        let Some(input) = queued.first().copied() else {
            return Ok(None);
        };
        let turn = self
            .shared
            .db
            .run(StartSuccessor {
                conversation,
                input,
                limits: self.shared.limits,
                now_unix_ms: now_unix_ms(),
            })
            .await
            .map_err(StoreError::into_error)?;
        self.spawn_drive(turn);
        Ok(Some(turn))
    }

    async fn resolve(&mut self, request: super::command::ResolveRequest) -> Result<()> {
        self.shared
            .db
            .run(ResolveInvocation {
                turn: request.turn,
                invocation: request.invocation,
                resolution: request.resolution,
                limits: self.shared.limits,
            })
            .await
            .map_err(StoreError::into_error)?;
        // Continue the turn: more calls may remain, or the exchange may now be
        // complete and ready for its next request basis.
        self.spawn_drive(request.turn);
        Ok(())
    }

    async fn turn_is_unfinished(&self, turn: TurnId) -> Result<bool> {
        let view = self
            .shared
            .db
            .run(ReadTurnView { turn })
            .await
            .map_err(StoreError::into_error)?;
        Ok(view.is_some_and(|view| view.turn.outcome.is_none()))
    }

    /// Start a drive unless the turn already has one.
    ///
    /// Idempotence matters: `resume`, a replayed submission and a successor
    /// drain can all ask for the same turn, and the engine must create one
    /// invocation per step, not one per request. A request that arrives while a
    /// drive is finishing is remembered rather than refused.
    fn spawn_drive(&mut self, turn: TurnId) {
        if self.active.contains_key(&turn) {
            self.pending.insert(turn);
            return;
        }
        self.start_drive(turn);
    }

    /// Continue turns whose request arrived while a drive was finishing.
    async fn drain_pending(&mut self) {
        if self.closing || self.pending.is_empty() {
            return;
        }
        let wanted: Vec<TurnId> = self.pending.iter().copied().collect();
        for turn in wanted {
            if self.active.contains_key(&turn) {
                continue;
            }
            self.pending.remove(&turn);
            match self.turn_is_unfinished(turn).await {
                Ok(true) => self.start_drive(turn),
                Ok(false) => {}
                Err(error) => {
                    let _ = self.shared.events.send(SessionEvent::Fenced {
                        message: error.to_string(),
                    });
                }
            }
        }
    }

    /// Start a drive. The caller has decided this turn should be driven.
    fn start_drive(&mut self, turn: TurnId) {
        self.pending.remove(&turn);
        let token = CancellationToken::new();
        self.active.insert(turn, token.clone());
        let shared = Arc::clone(&self.shared);
        self.joins.spawn(async move {
            let drive = std::panic::AssertUnwindSafe(drive(shared, turn, token));
            let completed = drive.catch_unwind().await.is_ok();
            (turn, completed)
        });
    }

    fn finish_drive(
        &mut self,
        joined: std::result::Result<(TurnId, bool), tokio::task::JoinError>,
    ) {
        match joined {
            Ok((turn, completed)) => {
                self.active.remove(&turn);
                if !completed {
                    // A panicking drive parks its turn with its durable state
                    // intact. Nothing is retried automatically; recovery is an
                    // explicit resume, and an unresolved external action stays
                    // unresolved while the supervisor keeps owning it.
                    let stops: Vec<Stop> = self
                        .stops
                        .values()
                        .filter(|(owner, _)| *owner == turn)
                        .map(|(_, stop)| stop.clone())
                        .collect();
                    for stop in stops {
                        stop.request();
                    }
                    let _ = self.shared.events.send(SessionEvent::TurnPhase {
                        turn,
                        phase: TurnPhase::Ready,
                        commit: None,
                    });
                }
            }
            Err(_) => {
                // A task that never completed leaves its turn unfinished; its
                // token is cancelled so no later resume races a dead drive.
                for token in self.active.values() {
                    token.cancel();
                }
            }
        }
    }

    /// Stop admission and dispatch, signal every owner, and join bounded.
    ///
    /// Storage ownership is released only when nothing is left running: a close
    /// that could not join reports that it is still closing and keeps the
    /// session's ownership instead of claiming quiescence.
    async fn shutdown(&mut self) -> CloseOutcome {
        self.closing = true;
        // Queued handoffs are not executions. Refuse them explicitly before
        // waiting for drives, so their receivers get a truthful non-start.
        self.spawns.close();
        while let Some(request) = self.spawns.recv().await {
            let _ = request.evidence.send(ToolOutcome::KnownFailure(
                "the action was not run: the session is closing".to_owned(),
            ));
        }
        for token in self.active.values() {
            token.cancel();
        }
        for (_, stop) in self.stops.values() {
            stop.request();
        }
        let deadline = Instant::now() + self.shared.limits.execution_join_grace();
        // Drives first: a drive joins the action it is waiting for, so it is
        // the one that decides whether its turn settles with evidence or with
        // an unresolved action.
        let mut drives = Vec::new();
        let drives_joined = timeout_at(deadline, async {
            while let Some(joined) = self.joins.join_next().await {
                drives.push(joined);
            }
        })
        .await
        .is_ok();
        for joined in drives {
            self.finish_drive(joined);
        }
        self.pending.clear();
        // Then the actions themselves; one that outlasts the grace keeps its
        // ownership here rather than being dropped.
        let mut actions = Vec::new();
        let actions_joined = timeout_at(deadline, async {
            while let Some(joined) = self.executions.join_next().await {
                actions.push(joined);
            }
        })
        .await
        .is_ok();
        for joined in actions {
            self.finish_execution(joined);
        }
        let pending = self.joins.len() + self.executions.len();
        if drives_joined && actions_joined && pending == 0 {
            CloseOutcome::Closed
        } else {
            CloseOutcome::StillClosing { pending }
        }
    }
}

/// Refuse an ordinary request once close has begun.
///
/// Closing stops admission and dispatch; a client that still holds a handle is
/// told the session is closing rather than being queued behind a close.
fn refuse(request: Request) {
    match request {
        Request::Submit { reply, .. } => {
            let _ = reply.send(Err(Error::Closed));
        }
        Request::Cancel { reply, .. } => {
            let _ = reply.send(Err(Error::Closed));
        }
        Request::Resume { reply, .. } => {
            let _ = reply.send(Err(Error::Closed));
        }
        Request::Resolve { reply, .. } => {
            let _ = reply.send(Err(Error::Closed));
        }
        Request::Configure { reply, .. } => {
            let _ = reply.send(Err(Error::Closed));
        }
        Request::Turn { reply, .. } => {
            let _ = reply.send(Err(Error::Closed));
        }
        Request::Conversation { reply, .. } => {
            let _ = reply.send(Err(Error::Closed));
        }
        Request::Entries { reply, .. } => {
            let _ = reply.send(Err(Error::Closed));
        }
        Request::ReadInput { reply, .. } => {
            let _ = reply.send(Err(Error::Closed));
        }
        Request::Withdraw { reply, .. } => {
            let _ = reply.send(Err(Error::Closed));
        }
        Request::Close { .. } => unreachable!("close is handled by the run loop"),
    }
}

/// The turn state machine.
async fn drive(shared: Arc<Shared>, turn: TurnId, token: CancellationToken) -> Result<()> {
    // Truthful reports a joined action produced after cancellation committed.
    // They are recorded by the terminal settlement, which preserves the fact
    // without authorizing another step.
    let mut evidence = Vec::new();
    loop {
        let view = match shared
            .db
            .run(ReadTurnView { turn })
            .await
            .map_err(StoreError::into_error)?
        {
            Some(view) => view,
            None => return Ok(()),
        };
        if let Some(outcome) = &view.turn.outcome {
            observe_terminal(&shared, turn, outcome).await;
            return Ok(());
        }
        // Cancellation is durable before it is local, so a drive that missed the
        // token still stops here.
        if view.turn.cancellation.requested {
            return finish(
                &shared,
                turn,
                PendingOutcome::Cancelled,
                std::mem::take(&mut evidence),
            )
            .await;
        }
        // The local token also interrupts work for close. Only durable user
        // cancellation authorizes a terminal Cancelled outcome; close leaves
        // the continuation and any recorded action evidence for explicit resume.
        if token.is_cancelled() {
            return Ok(());
        }
        if past_deadline(&view.turn) {
            return finish(
                &shared,
                turn,
                PendingOutcome::Failed(TurnFailure::Deadline),
                std::mem::take(&mut evidence),
            )
            .await;
        }
        match view.turn.phase {
            TurnPhase::Ready => begin_step(&shared, &view).await?,
            TurnPhase::ModelStep(step) => model_step(&shared, &view, step, &token).await?,
            TurnPhase::Tools(step) => {
                tools_step(&shared, &view, step, &token, &mut evidence).await?;
            }
            TurnPhase::Blocked { .. } => {
                publish_phase(&shared, turn, view.turn.phase).await;
                return Ok(());
            }
        }
    }
}

async fn begin_step(shared: &Shared, view: &TurnView) -> Result<()> {
    let conversation = view.turn.conversation;
    let Some(row) = shared
        .db
        .run(ReadConversation { conversation })
        .await
        .map_err(StoreError::into_error)?
    else {
        return fail(
            shared,
            view.turn.id,
            TurnFailure::Protocol {
                message: format!("conversation {conversation} no longer exists"),
            },
        )
        .await;
    };
    let Some(config) = row.config else {
        return fail(
            shared,
            view.turn.id,
            TurnFailure::Protocol {
                message: format!("conversation {conversation} has no configuration"),
            },
        )
        .await;
    };
    let specs = match shared.services.tools.specs(&config.config.tool_names) {
        Ok(specs) => specs,
        Err(missing) => {
            return fail(
                shared,
                view.turn.id,
                TurnFailure::Protocol {
                    message: missing.to_string(),
                },
            )
            .await;
        }
    };
    let ConversationConfig {
        model,
        instructions,
        controls,
        project_context,
        context,
        ..
    } = config.config;
    let started = shared
        .db
        .run(BeginStep {
            turn: view.turn.id,
            config_revision: config.revision,
            model,
            instructions,
            context: project_context,
            controls,
            tools: specs,
            max_request_bytes: context.max_request_bytes,
            limits: shared.limits,
        })
        .await
        .map_err(StoreError::into_error)?;
    match started {
        StepStart::Started(step) => {
            publish_phase(shared, view.turn.id, TurnPhase::ModelStep(step)).await;
            Ok(())
        }
        StepStart::Limit { setting } => {
            fail(
                shared,
                view.turn.id,
                TurnFailure::Limit {
                    setting: setting.to_owned(),
                },
            )
            .await
        }
    }
}

async fn model_step(
    shared: &Shared,
    view: &TurnView,
    step: crate::StepId,
    token: &CancellationToken,
) -> Result<()> {
    let Some(basis) = view.step.clone() else {
        return fail(
            shared,
            view.turn.id,
            TurnFailure::Protocol {
                message: "the turn names a model step that is not stored".to_owned(),
            },
        )
        .await;
    };
    let generation = view.turn.cancellation.generation;

    // Response-ready evidence is consumed, never re-requested: a crash between
    // the provider answer and the transcript must not cost a second call.
    if let Some(attempt) = view
        .attempts
        .iter()
        .rev()
        .find(|attempt| attempt.state == AttemptState::ResponseReady)
    {
        return settle_attempt(shared, &basis, attempt).await;
    }

    // An attempt whose dispatch outcome was never recorded is never repeated
    // silently. It is retired as unknown, and the retry that follows is a new
    // attempt with its own ordinal under the same step budget.
    if let Some(attempt) = view
        .attempts
        .last()
        .filter(|attempt| attempt.state == AttemptState::Dispatched)
    {
        shared
            .db
            .run(RetireAttempt {
                attempt: attempt.id,
            })
            .await
            .map_err(StoreError::into_error)?;
    }

    // An attempt this drive dispatched must not be reused if a retryable
    // failure sends the loop around again. The durable state has moved on, but
    // this drive's snapshot still lists the attempt as prepared, so reusing it
    // would dispatch one attempt twice and end the drive on a rejected
    // transition: a stall with no phase event rather than a retry.
    let mut dispatched: Option<crate::AttemptId> = None;
    loop {
        if token.is_cancelled() {
            return Ok(());
        }
        // The deadline bounds every attempt, not just the last wait: a retry
        // must not dispatch another request for a turn that has expired.
        if past_deadline(&view.turn) {
            return fail(shared, view.turn.id, TurnFailure::Deadline).await;
        }
        let reusable = view
            .attempts
            .iter()
            .find(|attempt| {
                attempt.state == AttemptState::Prepared && Some(attempt.id) != dispatched
            })
            .map(|attempt| attempt.id);
        let attempt = match reusable {
            Some(attempt) => attempt,
            None => {
                match shared
                    .db
                    .run(PrepareAttempt { step })
                    .await
                    .map_err(StoreError::into_error)?
                {
                    AttemptStart::Started(attempt) => attempt,
                    AttemptStart::Limit { setting } => {
                        return fail(
                            shared,
                            view.turn.id,
                            TurnFailure::Limit {
                                setting: setting.to_owned(),
                            },
                        )
                        .await;
                    }
                }
            }
        };
        let entries = shared
            .db
            .run(ReadEntriesUpTo {
                conversation: view.turn.conversation,
                cut: basis.cut,
            })
            .await
            .map_err(StoreError::into_error)?;
        let assembled = match assemble(&basis, &entries) {
            Ok(assembled) => assembled,
            Err(error) => return fail(shared, view.turn.id, protocol(error)).await,
        };
        if assembled.bytes > u64::from(basis.max_request_bytes) {
            return fail(
                shared,
                view.turn.id,
                TurnFailure::Limit {
                    setting: "max_request_bytes".to_owned(),
                },
            )
            .await;
        }
        shared
            .db
            .run(CommitDispatch {
                attempt,
                generation,
            })
            .await
            .map_err(StoreError::into_error)?;
        dispatched = Some(attempt);
        let request = ion_ai::ModelRequest {
            model: basis.model.clone(),
            instructions: Some(assembled.instructions).filter(|text| !text.is_empty()),
            messages: assembled.messages,
            tools: basis.tools.clone(),
            controls: basis.controls.clone(),
        };
        let collected = tokio::select! {
            biased;
            () = token.cancelled() => return Ok(()),
            result = collect_response(
                shared.services.model.as_ref(),
                request,
                u64::from(view.turn.limits.max_response_bytes),
            ) => result,
            // A provider that opens a stream and never completes it would
            // otherwise hold the turn open past its own deadline. A response
            // that is already ready is committed first: the answer is durable
            // truth, and the deadline is still honored at the loop top.
            () = wait_until(deadline_of(&view.turn)) => {
                return fail(shared, view.turn.id, TurnFailure::Deadline).await;
            }
        };
        match collected {
            Ok(response) => {
                match shared
                    .db
                    .run(CommitResponse {
                        attempt,
                        generation,
                        response,
                    })
                    .await
                {
                    // The record is durable; the next iteration settles it, so a
                    // crash here cannot lose the answer or repeat the call.
                    Ok(()) => return Ok(()),
                    // The generation moved: cancellation won and the response is
                    // dropped rather than settled.
                    Err(error) if error.is_rejected() => return Ok(()),
                    Err(error) => return Err(error.into_error()),
                }
            }
            Err(failure) => match failure {
                ModelFailure::Provider(error) if retryable(error.kind) => {
                    // Retry is bounded by the step's attempt ceiling and is
                    // visible in durable attempt evidence, so it is accounted
                    // rather than hidden.
                    continue;
                }
                ModelFailure::Provider(error) => {
                    return fail(
                        shared,
                        view.turn.id,
                        TurnFailure::Provider {
                            kind: error.kind,
                            message: error.message,
                        },
                    )
                    .await;
                }
                ModelFailure::Protocol(message) => {
                    return fail(shared, view.turn.id, TurnFailure::Protocol { message }).await;
                }
            },
        }
    }
}

async fn settle_attempt(shared: &Shared, basis: &ModelStep, attempt: &ModelAttempt) -> Result<()> {
    let Some(response) = attempt.response.clone() else {
        return fail(
            shared,
            basis.turn,
            TurnFailure::Protocol {
                message: "a response-ready attempt has no stored response".to_owned(),
            },
        )
        .await;
    };
    let calls = match admitted_calls(shared, basis, &response) {
        Ok(calls) => calls,
        Err(cause) => return fail(shared, basis.turn, cause).await,
    };
    shared
        .db
        .run(SettleAttempt {
            attempt: attempt.id,
            calls,
            limits: shared.limits,
        })
        .await
        .map_err(StoreError::into_error)?;
    Ok(())
}

async fn tools_step(
    shared: &Shared,
    view: &TurnView,
    step: crate::StepId,
    token: &CancellationToken,
    evidence: &mut Vec<(InvocationId, InvocationOutcome)>,
) -> Result<()> {
    let generation = view.turn.cancellation.generation;
    let Some(invocation) = view
        .invocations
        .iter()
        .find(|invocation| invocation.step == step && invocation.state == InvocationState::Prepared)
        .cloned()
    else {
        // A dispatch intent with no executor left means the process that owned
        // the action is gone, not that nothing happened. Nothing is repeated on
        // that evidence, so the exchange parks until a client decides.
        let dispatched = view
            .invocations
            .iter()
            .find(|invocation| {
                invocation.step == step && invocation.state == InvocationState::Dispatched
            })
            .cloned();
        if let Some(dispatched) = dispatched {
            return settle_invocation(
                shared,
                view.turn.id,
                &dispatched,
                InvocationOutcome::Indeterminate {
                    message:
                        "the action was dispatched but no executor remained to report its outcome"
                            .to_owned(),
                },
                evidence,
            )
            .await;
        }
        return Ok(());
    };

    // The recorded implementation identity is checked before dispatch: a tool
    // that merely shares a name is not allowed to reinterpret prepared data.
    let tool = shared
        .services
        .tools
        .get(&invocation.call.name)
        .filter(|tool| tool.identity() == invocation.implementation)
        .cloned();
    let Some(tool) = tool else {
        return settle_invocation(
            shared,
            view.turn.id,
            &invocation,
            InvocationOutcome::Failed {
                message: format!(
                    "no implementation {} is available for tool {:?}",
                    invocation.implementation, invocation.call.name
                ),
            },
            evidence,
        )
        .await;
    };

    // Dispatch intent is durable before any action can start. If the handoff
    // below never happens, the record still reads as "may have happened", which
    // is the conservative reading of an intent with no executor.
    shared
        .db
        .run(CommitInvocationDispatch {
            invocation: invocation.id,
            generation,
        })
        .await
        .map_err(StoreError::into_error)?;

    let stop = Stop::new();
    let (sender, mut receiver) = oneshot::channel();
    let handed = tokio::select! {
        biased;
        () = token.cancelled() => false,
        sent = shared.spawns.send(SpawnRequest {
            turn: view.turn.id,
            invocation: invocation.id,
            tool,
            call: invocation.call.clone(),
            stop: stop.clone(),
            evidence: sender,
        }) => sent.is_ok(),
    };
    if !handed {
        // Nothing accepted the action, so it did not run: a known fact even
        // though the dispatch intent is durable.
        return settle_invocation(
            shared,
            view.turn.id,
            &invocation,
            InvocationOutcome::Failed {
                message: "the action was not run: no executor accepted it".to_owned(),
            },
            evidence,
        )
        .await;
    }

    // Wait for the action's evidence. Cancellation asks it to stop and then
    // keeps waiting, bounded: the supervisor owns it either way, so a stopped
    // action's report is recorded instead of being dropped with the future that
    // awaited it.
    let joined = tokio::select! {
        biased;
        () = token.cancelled() => {
            stop.request();
            timeout(shared.limits.execution_join_grace(), &mut receiver)
                .await
                .ok()
                .and_then(std::result::Result::ok)
        }
        // A turn's deadline bounds its action too: otherwise an action that
        // never reports could hold a turn open past every limit.
        () = wait_until(deadline_of(&view.turn)) => {
            stop.request();
            timeout(shared.limits.execution_join_grace(), &mut receiver)
                .await
                .ok()
                .and_then(std::result::Result::ok)
        }
        received = &mut receiver => received.ok(),
    };
    let outcome = match joined {
        Some(ToolOutcome::Completed(result)) => InvocationOutcome::Succeeded { result },
        Some(ToolOutcome::KnownFailure(message)) => InvocationOutcome::Failed { message },
        Some(ToolOutcome::Indeterminate(message)) => InvocationOutcome::Indeterminate { message },
        // Nothing reported inside the grace. The action may still be running
        // and the supervisor still owns it; the turn settles without inventing
        // an outcome for it.
        None => InvocationOutcome::Indeterminate {
            message: "the action had not reported an outcome when its stop grace expired"
                .to_owned(),
        },
    };
    settle_invocation(shared, view.turn.id, &invocation, outcome, evidence).await
}

async fn settle_invocation(
    shared: &Shared,
    turn: TurnId,
    invocation: &crate::invocation::ToolInvocation,
    outcome: InvocationOutcome,
    evidence: &mut Vec<(InvocationId, InvocationOutcome)>,
) -> Result<()> {
    let generation = invocation.generation;
    match shared
        .db
        .run(CommitInvocationResult {
            invocation: invocation.id,
            generation,
            outcome: outcome.clone(),
            limits: shared.limits,
        })
        .await
    {
        Ok(progress) => {
            if progress.next.is_some() {
                publish_phase(shared, turn, TurnPhase::Tools(invocation.step)).await;
            }
            Ok(())
        }
        // Cancellation committed since dispatch, so this action may not settle
        // itself. The report is still a fact: it is carried to the terminal
        // settlement, which records it without resuming continuation.
        Err(error) if error.is_rejected() => {
            let current = shared
                .db
                .run(ReadTurnView { turn })
                .await
                .map_err(StoreError::into_error)?;
            if current.is_some_and(|view| view.turn.cancellation.requested) {
                evidence.push((invocation.id, outcome));
                Ok(())
            } else {
                // A rejected transition is not necessarily cancellation. Do
                // not retry an unchanged state forever or grow an evidence log.
                Err(error.into_error())
            }
        }
        Err(error) => Err(error.into_error()),
    }
}

async fn fail(shared: &Shared, turn: TurnId, cause: TurnFailure) -> Result<()> {
    finish(shared, turn, PendingOutcome::Failed(cause), Vec::new()).await
}

async fn finish(
    shared: &Shared,
    turn: TurnId,
    pending: PendingOutcome,
    evidence: Vec<(InvocationId, InvocationOutcome)>,
) -> Result<()> {
    let outcome = shared
        .db
        .run(FinishTurn {
            turn,
            outcome: pending,
            evidence,
            limits: shared.limits,
        })
        .await
        .map_err(StoreError::into_error)?;
    observe_terminal(shared, turn, &outcome).await;
    Ok(())
}

/// Publish a terminal outcome and hand the conversation back for its queue.
///
/// Both paths that end a turn go through here, because a queued input must
/// become a successor turn whether the turn finished normally, failed, or was
/// cancelled, and the supervisor is the only task that may start one.
async fn observe_terminal(shared: &Shared, turn: TurnId, outcome: &TurnOutcome) {
    publish_terminal(shared, turn, outcome).await;
    let conversation = shared
        .db
        .run(ReadTurnView { turn })
        .await
        .ok()
        .flatten()
        .map(|view| view.turn.conversation);
    if let Some(conversation) = conversation {
        let (reply, _receive) = oneshot::channel();
        let _ = shared
            .requests
            .send(Request::Resume {
                conversation,
                reply,
            })
            .await;
    }
}

async fn publish_phase(shared: &Shared, turn: TurnId, phase: TurnPhase) {
    let commit = current_commit(shared).await;
    let _ = shared.events.send(SessionEvent::TurnPhase {
        turn,
        phase,
        commit,
    });
}

async fn publish_terminal(shared: &Shared, turn: TurnId, outcome: &TurnOutcome) {
    let commit = current_commit(shared).await;
    let _ = shared.events.send(SessionEvent::TurnTerminal {
        turn,
        outcome: outcome.clone(),
        commit,
    });
}

async fn current_commit(shared: &Shared) -> Option<CommitSeq> {
    shared.db.run(ReadCommit).await.ok().flatten()
}

fn past_deadline(turn: &Turn) -> bool {
    turn.deadline_unix_ms()
        .is_some_and(|deadline| now_unix_ms() >= deadline)
}

/// When a turn's deadline falls, if it has one.
fn deadline_of(turn: &Turn) -> Option<Instant> {
    let deadline = turn.deadline_unix_ms()?;
    let remaining = u64::try_from(deadline.saturating_sub(now_unix_ms())).unwrap_or(0);
    Some(Instant::now() + Duration::from_millis(remaining))
}

/// Wait for a deadline, or forever when the turn has none.
async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

pub(crate) fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

fn protocol(error: RequestError) -> TurnFailure {
    TurnFailure::Protocol {
        message: error.to_string(),
    }
}

/// Whether a provider failure may be retried against the same frozen basis.
///
/// Only failures that establish no result are retried, and the attempt ceiling
/// bounds them. Authorization, request-shape and safety failures are not
/// repeated because the same request would fail the same way.
fn retryable(kind: ion_ai::ProviderErrorKind) -> bool {
    use ion_ai::ProviderErrorKind as Kind;
    matches!(
        kind,
        Kind::Transport | Kind::Timeout | Kind::Overloaded | Kind::Server | Kind::RateLimited
    )
}

enum ModelFailure {
    Provider(ion_ai::ProviderError),
    Protocol(String),
}

/// Drive one model stream to a validated response.
///
/// The terminal event ends the stream: EOF without one is an incomplete answer,
/// not a slow one, and content after the terminal event is refused.
async fn collect_response(
    service: &dyn ion_ai::ModelService,
    request: ion_ai::ModelRequest,
    max_bytes: u64,
) -> std::result::Result<ion_ai::ModelResponse, ModelFailure> {
    use ion_ai::ModelStreamEvent as Event;
    let mut stream = service
        .stream(request)
        .await
        .map_err(ModelFailure::Provider)?;
    let mut bytes = 0u64;
    let mut completed: Option<ion_ai::ModelResponse> = None;
    while let Some(event) = stream.next().await {
        let event = event.map_err(ModelFailure::Provider)?;
        bytes = bytes.saturating_add(event_size(&event));
        if bytes > max_bytes {
            return Err(ModelFailure::Protocol(
                "the provider response exceeded the configured byte budget".to_owned(),
            ));
        }
        match event {
            Event::Completed(response) => {
                if completed.is_some() {
                    return Err(ModelFailure::Protocol(
                        "the provider stream produced more than one terminal event".to_owned(),
                    ));
                }
                completed = Some(response);
            }
            Event::TextDelta(_) | Event::ToolCall(_) | Event::Usage(_) => {
                if completed.is_some() {
                    return Err(ModelFailure::Protocol(
                        "the provider stream continued after its terminal event".to_owned(),
                    ));
                }
            }
        }
    }
    let response = completed.ok_or_else(|| {
        ModelFailure::Protocol("the provider stream ended without a terminal event".to_owned())
    })?;
    validate_response(&response)?;
    Ok(response)
}

fn event_size(event: &ion_ai::ModelStreamEvent) -> u64 {
    serde_json::to_vec(event).map_or(0, |encoded| encoded.len() as u64)
}

fn validate_response(response: &ion_ai::ModelResponse) -> std::result::Result<(), ModelFailure> {
    if response.message.role != ion_ai::Role::Assistant {
        return Err(ModelFailure::Protocol(
            "the provider returned a non-assistant message".to_owned(),
        ));
    }
    if !response.is_complete() {
        return Err(ModelFailure::Protocol(
            "the provider reported an incomplete response".to_owned(),
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for block in &response.message.content {
        if let ion_ai::Content::ToolCall(call) = block {
            if call.id.is_empty() {
                return Err(ModelFailure::Protocol(
                    "a tool call has an empty identifier".to_owned(),
                ));
            }
            if !ids.insert(call.id.clone()) {
                return Err(ModelFailure::Protocol(format!(
                    "the provider repeated the tool call id {:?}",
                    call.id
                )));
            }
        }
    }
    Ok(())
}

/// Resolve the provider's calls against the frozen catalog and the live registry.
fn admitted_calls(
    shared: &Shared,
    basis: &ModelStep,
    response: &ion_ai::ModelResponse,
) -> std::result::Result<Vec<AdmittedCall>, TurnFailure> {
    let frozen: std::collections::BTreeSet<&str> =
        basis.tools.iter().map(|spec| spec.name.as_str()).collect();
    let mut calls = Vec::new();
    for block in &response.message.content {
        let ion_ai::Content::ToolCall(call) = block else {
            continue;
        };
        if !frozen.contains(call.name.as_str()) {
            return Err(TurnFailure::Protocol {
                message: format!(
                    "the provider called {:?}, which this request did not offer",
                    call.name
                ),
            });
        }
        let Some(tool) = shared.services.tools.get(&call.name) else {
            return Err(TurnFailure::Protocol {
                message: format!("no implementation is registered for tool {:?}", call.name),
            });
        };
        calls.push(AdmittedCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
            implementation: tool.identity(),
            repeat_safe: tool.repeat_safe(),
        });
    }
    Ok(calls)
}
