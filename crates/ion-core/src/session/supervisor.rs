//! The single session supervisor.
//!
//! One task owns admission decisions, turn scheduling and the `JoinSet` of
//! running turns. Work is bounded and joined explicitly, never detached: a
//! drive that panics parks its turn instead of restarting it, and a drive whose
//! generation has been superseded stops writing instead of racing the recovery
//! that replaced it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::{FutureExt, StreamExt};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use super::command::{AdmissionReceipt, CancelReceipt, Request, SessionEvent, SubmitRequest};
use super::handle::SessionHandle;
use crate::attempt::{AttemptState, ModelAttempt, ModelStep};
use crate::config::ConversationConfig;
use crate::error::Result;
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
use crate::tool::ToolRegistry;
use crate::turn::{PendingOutcome, Turn, TurnFailure, TurnOutcome, TurnPhase};
use crate::view::TurnView;
use crate::{CommitSeq, ConversationId, TurnId};

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
    handle: SessionHandle,
    active: BTreeMap<TurnId, CancellationToken>,
    joins: JoinSet<(TurnId, bool)>,
}

impl Supervisor {
    pub(crate) fn new(
        shared: Arc<Shared>,
        owner_db: Db,
        requests: mpsc::Receiver<Request>,
        handle: SessionHandle,
    ) -> Self {
        Self {
            shared,
            owner_db,
            requests,
            handle,
            active: BTreeMap::new(),
            joins: JoinSet::new(),
        }
    }

    /// Serve requests until the session is closed.
    ///
    /// The session is usable on open without resuming anything: `Session::open`
    /// starts this service, and work starts only when a client asks for it.
    pub(crate) async fn run(mut self) {
        loop {
            tokio::select! {
                biased;
                request = self.requests.recv() => {
                    match request {
                        None => break,
                        Some(Request::Close { reply }) => {
                            let _ = reply.send(());
                            break;
                        }
                        Some(request) => self.dispatch(request).await,
                    }
                }
                joined = self.joins.join_next(), if !self.joins.is_empty() => {
                    if let Some(joined) = joined {
                        self.finish_drive(joined);
                    }
                }
            }
        }
        self.shutdown().await;
        let _ = self.shared.events.send(SessionEvent::Closed);
        self.owner_db.close().await;
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
    /// invocation per step, not one per request.
    fn spawn_drive(&mut self, turn: TurnId) {
        if self.active.contains_key(&turn) {
            return;
        }
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
                    // unresolved.
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

    async fn shutdown(&mut self) {
        for token in self.active.values() {
            token.cancel();
        }
        while let Some(joined) = self.joins.join_next().await {
            if let Ok((turn, _)) = joined {
                self.active.remove(&turn);
            }
        }
    }
}

/// The turn state machine.
async fn drive(shared: Arc<Shared>, turn: TurnId, token: CancellationToken) -> Result<()> {
    loop {
        if token.is_cancelled() {
            return finish(&shared, turn, PendingOutcome::Cancelled).await;
        }
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
            return finish(&shared, turn, PendingOutcome::Cancelled).await;
        }
        if past_deadline(&view.turn) {
            return finish(&shared, turn, PendingOutcome::Failed(TurnFailure::Deadline)).await;
        }
        match view.turn.phase {
            TurnPhase::Ready => begin_step(&shared, &view).await?,
            TurnPhase::ModelStep(step) => model_step(&shared, &view, step, &token).await?,
            TurnPhase::Tools(step) => tools_step(&shared, &view, step, &token).await?,
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

    loop {
        if token.is_cancelled() {
            return Ok(());
        }
        let reusable = view
            .attempts
            .iter()
            .find(|attempt| attempt.state == AttemptState::Prepared)
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
) -> Result<()> {
    let generation = view.turn.cancellation.generation;
    let Some(invocation) = view
        .invocations
        .iter()
        .find(|invocation| invocation.step == step && invocation.state == InvocationState::Prepared)
        .cloned()
    else {
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
        )
        .await;
    };

    shared
        .db
        .run(CommitInvocationDispatch {
            invocation: invocation.id,
            generation,
        })
        .await
        .map_err(StoreError::into_error)?;
    let outcome = tokio::select! {
        biased;
        () = token.cancelled() => return Ok(()),
        outcome = tool.execute(&invocation.call) => outcome,
    };
    let outcome = match outcome {
        crate::tool::ToolOutcome::Completed(result) => InvocationOutcome::Succeeded { result },
        crate::tool::ToolOutcome::KnownFailure(message) => InvocationOutcome::Failed { message },
        crate::tool::ToolOutcome::Indeterminate(message) => {
            InvocationOutcome::Indeterminate { message }
        }
    };
    settle_invocation(shared, view.turn.id, &invocation, outcome).await
}

async fn settle_invocation(
    shared: &Shared,
    turn: TurnId,
    invocation: &crate::invocation::ToolInvocation,
    outcome: InvocationOutcome,
) -> Result<()> {
    let generation = invocation.generation;
    match shared
        .db
        .run(CommitInvocationResult {
            invocation: invocation.id,
            generation,
            outcome,
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
        // Superseded by cancellation: the result is not recorded as current
        // evidence, and the turn settles as cancelled with its invocation
        // unresolved.
        Err(error) if error.is_rejected() => Ok(()),
        Err(error) => Err(error.into_error()),
    }
}

async fn fail(shared: &Shared, turn: TurnId, cause: TurnFailure) -> Result<()> {
    finish(shared, turn, PendingOutcome::Failed(cause)).await
}

async fn finish(shared: &Shared, turn: TurnId, pending: PendingOutcome) -> Result<()> {
    let outcome = shared
        .db
        .run(FinishTurn {
            turn,
            outcome: pending,
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
