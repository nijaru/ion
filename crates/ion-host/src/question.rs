//! Live question custody shared by a tool executor and one interactive consumer.
//!
//! This module owns no Session facts or question history. It only carries one
//! pending request between an admitted Core tool invocation and the controlling
//! frontend. Dropping the consumer lease cancels the original Turn before any
//! dependent tool dispatch can continue.
use std::sync::{Arc, Mutex, MutexGuard};

use ion_core::{CodingToolOutput as ToolOutput, ToolOrigin};
use serde::Serialize;
use thiserror::Error;
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const MAX_QUESTIONS: usize = 4;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 6;
pub const MAX_QUESTION_BYTES: usize = 4096;
pub const MAX_OPTION_LABEL_BYTES: usize = 120;
pub const MAX_OPTION_DESCRIPTION_BYTES: usize = 1024;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_ANSWER_BYTES: usize = 4096;
pub const MAX_ANSWER_BATCH_BYTES: usize = 48 * 1024;
pub const SYNTHETIC_TEXT_LABEL: &str = "Type something.";
pub(crate) const RESERVED_LABELS: [&str; 2] = ["Other", SYNTHETIC_TEXT_LABEL];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Question {
    pub question: String,
    pub options: Vec<QuestionOption>,
}

/// Opaque live request identity. A responder must present both its lease and
/// this exact nonce; Session history or repeated provider call IDs cannot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct QuestionRequestId(pub Uuid);

impl QuestionRequestId {
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl std::fmt::Display for QuestionRequestId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PendingQuestion {
    pub request: QuestionRequestId,
    pub questions: Vec<Question>,
    /// Core invocation provenance retained for correlation, not authority.
    pub origin: ToolOrigin,
}

/// A typed user answer. Option labels are derived from the immutable request;
/// custom text is preserved exactly after nonempty/byte validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionAnswer {
    Option { index: usize },
    Text { text: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CompletedAnswer {
    Option {
        question: usize,
        option: usize,
        label: String,
    },
    Text {
        question: usize,
        text: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum QuestionResult {
    Answered { answers: Vec<CompletedAnswer> },
    Cancelled { reason: String },
}

impl QuestionResult {
    fn answered(answers: Vec<CompletedAnswer>) -> ToolOutput {
        ToolOutput {
            value: serde_json::to_value(Self::Answered { answers })
                .expect("typed question result serializes"),
            images: Vec::new(),
            is_error: false,
        }
    }

    fn cancelled(reason: impl Into<String>) -> ToolOutput {
        ToolOutput {
            value: serde_json::to_value(Self::Cancelled {
                reason: reason.into(),
            })
            .expect("typed question cancellation serializes"),
            images: Vec::new(),
            is_error: false,
        }
    }
}

pub(crate) fn answered_output(answers: Vec<CompletedAnswer>) -> ToolOutput {
    QuestionResult::answered(answers)
}

pub(crate) fn cancelled_output(reason: impl Into<String>) -> ToolOutput {
    QuestionResult::cancelled(reason)
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum QuestionError {
    #[error("question responder is not attached")]
    NotAttached,
    #[error("question responder is already attached")]
    AlreadyAttached,
    #[error("question responder belongs to another control")]
    ForeignResponder,
    #[error("question control is closed")]
    Closed,
    #[error("question custody already belongs to another setup")]
    AlreadyBound,
    #[error("another question batch is already pending")]
    Busy,
    #[error("question request is stale or unknown")]
    StaleRequest,
    #[error("question request was already stopped")]
    Stopped,
    #[error("question request belongs to another Session")]
    ForeignSession,
    #[error("question tool requires a live Core invocation")]
    Untracked,
    #[error("{0}")]
    InvalidAnswer(&'static str),
    #[error("question answers exceed the serialized batch limit")]
    AnswerBatchTooLarge,
}

struct Binding {
    owner: Uuid,
    session: Uuid,
}

struct Consumer {
    lease: Uuid,
    owner: Uuid,
    session: Uuid,
}

struct TurnStop {
    scope: TurnStopScope,
    owner: Uuid,
    session: Uuid,
    stop: CancellationToken,
}

/// Identity of one Host-owned Turn stop binding. Clearing an old scope cannot
/// remove a newer operation's authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TurnStopScope(u64);

pub(crate) struct TurnStopBinding {
    scope: TurnStopScope,
    previous: Option<TurnStop>,
}

impl TurnStopBinding {
    pub(crate) fn scope(&self) -> TurnStopScope {
        self.scope
    }
}

struct PendingState {
    id: QuestionRequestId,
    owner: Uuid,
    session: Uuid,
    origin: ToolOrigin,
    stop: CancellationToken,
    /// Root Turn stop captured when this request opened, distinct from the
    /// invocation/guest stop above. Cancelling it ends the owning Turn.
    turn_stop: Option<CancellationToken>,
    questions: Vec<Question>,
    reply: Option<oneshot::Sender<QuestionDecision>>,
}

pub(crate) enum QuestionDecision {
    Answered(Vec<CompletedAnswer>),
    Cancelled(String),
}

#[derive(Default)]
struct State {
    generation: u64,
    binding: Option<Binding>,
    consumer: Option<Consumer>,
    pending: Option<PendingState>,
    turn_stop: Option<TurnStop>,
    next_turn_stop: u64,
}

impl State {
    fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}

pub struct QuestionCustody {
    state: Mutex<State>,
    changed: watch::Sender<u64>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Default for QuestionCustody {
    fn default() -> Self {
        Self::new()
    }
}

impl QuestionCustody {
    pub fn new() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            state: Mutex::new(State::default()),
            changed,
        }
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    pub(crate) fn generation(&self) -> u64 {
        lock(&self.state).generation
    }

    fn notify(&self) {
        let generation = lock(&self.state).generation;
        // Keep the latest generation even when no control is currently
        // subscribed, so a later subscriber can reconcile without a wake gap.
        self.changed.send_replace(generation);
    }

    pub(crate) fn bind_control(&self, owner: Uuid, session: Uuid) -> Result<(), QuestionError> {
        let mut state = lock(&self.state);
        if state.binding.is_some() {
            return Err(QuestionError::AlreadyBound);
        }
        state.bump();
        state.binding = Some(Binding { owner, session });
        state.consumer = None;
        drop(state);
        self.notify();
        Ok(())
    }

    pub(crate) fn rebind_session(&self, owner: Uuid, session: Uuid) {
        let cancelled = {
            let mut state = lock(&self.state);
            if state
                .binding
                .as_ref()
                .is_none_or(|binding| binding.owner != owner)
            {
                return;
            }
            state.bump();
            state.binding = Some(Binding { owner, session });
            if let Some(consumer) = &mut state.consumer {
                if consumer.owner == owner {
                    consumer.session = session;
                } else {
                    state.consumer = None;
                }
            }
            state.pending.take()
        };
        if let Some(pending) = cancelled {
            cancel_pending_stops(&pending);
            if let Some(reply) = pending.reply {
                let _ = reply.send(QuestionDecision::Cancelled("binding_replaced".into()));
            }
        }
        self.notify();
    }

    pub(crate) fn close(&self, owner: Uuid, reason: &'static str) {
        let pending = {
            let mut state = lock(&self.state);
            if state
                .binding
                .as_ref()
                .is_none_or(|binding| binding.owner != owner)
            {
                return;
            }
            state.bump();
            if state
                .consumer
                .as_ref()
                .is_some_and(|consumer| consumer.owner == owner)
            {
                state.consumer = None;
            }
            state.pending.take()
        };
        if let Some(pending) = pending {
            cancel_pending_stops(&pending);
            if let Some(reply) = pending.reply {
                let _ = reply.send(QuestionDecision::Cancelled(reason.into()));
            }
        }
        self.notify();
    }

    /// Retain the exact root stop Core receives for the Turn that is starting.
    /// This is Host operation metadata, not a second durable execution record.
    pub(crate) fn bind_turn_stop(
        &self,
        owner: Uuid,
        session: Uuid,
        stop: CancellationToken,
    ) -> Option<TurnStopBinding> {
        let mut state = lock(&self.state);
        let binding = state.binding.as_ref()?;
        if binding.owner != owner || binding.session != session {
            return None;
        }
        state.next_turn_stop = state
            .next_turn_stop
            .checked_add(1)
            .expect("question turn stop scope exhausted");
        let scope = TurnStopScope(state.next_turn_stop);
        let previous = state.turn_stop.take();
        state.turn_stop = Some(TurnStop {
            scope,
            owner,
            session,
            stop,
        });
        Some(TurnStopBinding { scope, previous })
    }

    /// Failed or reconciled admission did not start this Turn; put back any
    /// authority it provisionally covered before Core rejected the submission.
    pub(crate) fn restore_turn_stop(&self, owner: Uuid, session: Uuid, binding: TurnStopBinding) {
        let mut state = lock(&self.state);
        if state.turn_stop.as_ref().is_some_and(|turn| {
            turn.scope == binding.scope && turn.owner == owner && turn.session == session
        }) {
            state.turn_stop = binding.previous;
        }
    }

    /// Clear only this operation's binding; a stale scope cannot revoke the
    /// authority retained by a replacement operation.
    pub(crate) fn clear_turn_stop(&self, owner: Uuid, session: Uuid, scope: TurnStopScope) {
        let mut state = lock(&self.state);
        if state.turn_stop.as_ref().is_some_and(|turn| {
            turn.scope == scope && turn.owner == owner && turn.session == session
        }) {
            state.turn_stop = None;
        }
    }

    pub(crate) fn cancel_pending(&self, owner: Uuid, reason: &'static str) {
        let pending = {
            let mut state = lock(&self.state);
            if state
                .pending
                .as_ref()
                .is_none_or(|pending| pending.owner != owner)
            {
                return;
            }
            state.bump();
            state.pending.take()
        };
        if let Some(pending) = pending {
            cancel_pending_stops(&pending);
            if let Some(reply) = pending.reply {
                let _ = reply.send(QuestionDecision::Cancelled(reason.into()));
            }
        }
        self.notify();
    }

    pub(crate) fn attach(
        self: &Arc<Self>,
        owner: Uuid,
    ) -> Result<QuestionResponder, QuestionError> {
        let lease = {
            let mut state = lock(&self.state);
            let binding = state.binding.as_ref().ok_or(QuestionError::NotAttached)?;
            if binding.owner != owner {
                return Err(QuestionError::ForeignResponder);
            }
            if state.consumer.is_some() {
                return Err(QuestionError::AlreadyAttached);
            }
            let lease = Uuid::now_v7();
            state.consumer = Some(Consumer {
                lease,
                owner,
                session: binding.session,
            });
            lease
        };
        Ok(QuestionResponder {
            custody: self.clone(),
            lease,
        })
    }

    pub(crate) fn pending_for(&self, owner: Uuid, session: Uuid) -> Option<PendingQuestion> {
        let state = lock(&self.state);
        let pending = state.pending.as_ref()?;
        (pending.owner == owner && pending.session == session).then(|| PendingQuestion {
            request: pending.id,
            questions: pending.questions.clone(),
            origin: pending.origin,
        })
    }

    fn check_lease(
        state: &State,
        lease: Uuid,
        request: QuestionRequestId,
    ) -> Result<(), QuestionError> {
        let consumer = state
            .consumer
            .as_ref()
            .filter(|consumer| consumer.lease == lease)
            .ok_or(QuestionError::ForeignResponder)?;
        let binding = state.binding.as_ref().ok_or(QuestionError::Closed)?;
        if binding.owner != consumer.owner || binding.session != consumer.session {
            return Err(QuestionError::ForeignResponder);
        }
        let pending = state.pending.as_ref().ok_or(QuestionError::StaleRequest)?;
        if pending.id != request
            || pending.owner != consumer.owner
            || pending.session != consumer.session
        {
            return Err(QuestionError::StaleRequest);
        }
        Ok(())
    }

    fn answer_inner(
        &self,
        lease: Uuid,
        request: QuestionRequestId,
        answers: Vec<QuestionAnswer>,
    ) -> Result<(), QuestionError> {
        let completed = {
            let state = lock(&self.state);
            Self::check_lease(&state, lease, request)?;
            let pending = state.pending.as_ref().expect("checked pending");
            if pending.stop.is_cancelled() {
                return Err(QuestionError::Stopped);
            }
            validate_answers(&pending.questions, &answers)?
        };
        let pending = {
            let mut state = lock(&self.state);
            Self::check_lease(&state, lease, request)?;
            if state
                .pending
                .as_ref()
                .expect("checked pending")
                .stop
                .is_cancelled()
            {
                return Err(QuestionError::Stopped);
            }
            let pending = state.pending.take().expect("checked pending");
            state.bump();
            pending
        };
        if let Some(reply) = pending.reply {
            let _ = reply.send(QuestionDecision::Answered(completed));
        }
        self.notify();
        Ok(())
    }

    fn cancel_inner(
        &self,
        lease: Uuid,
        request: QuestionRequestId,
        reason: &'static str,
    ) -> Result<(), QuestionError> {
        let pending = {
            let mut state = lock(&self.state);
            Self::check_lease(&state, lease, request)?;
            if state
                .pending
                .as_ref()
                .expect("checked pending")
                .stop
                .is_cancelled()
            {
                return Err(QuestionError::Stopped);
            }
            let pending = state.pending.take().expect("checked pending");
            state.bump();
            pending
        };
        // Cancelling the batch is intentionally not a Turn stop. The tool's
        // observed cancellation result lets the model continue normally.
        if let Some(reply) = pending.reply {
            let _ = reply.send(QuestionDecision::Cancelled(reason.into()));
        }
        self.notify();
        Ok(())
    }

    fn revoke(&self, lease: Uuid) {
        let (pending, removed) = {
            let mut state = lock(&self.state);
            let matches = state
                .consumer
                .as_ref()
                .is_some_and(|consumer| consumer.lease == lease);
            if !matches {
                return;
            }
            state.consumer = None;
            let pending = state.pending.take();
            if pending.is_some() {
                state.bump();
            }
            (pending, true)
        };
        if removed {
            if let Some(pending) = pending {
                // Ordering is part of the contract: revoke the original Turn
                // before waking the model/tool with a cancellation result.
                cancel_pending_stops(&pending);
                if let Some(reply) = pending.reply {
                    let _ = reply.send(QuestionDecision::Cancelled("responder_lost".into()));
                }
            }
            self.notify();
        }
    }

    pub(crate) fn open(
        &self,
        origin: ToolOrigin,
        questions: Vec<Question>,
        stop: CancellationToken,
    ) -> Result<(QuestionRequestId, oneshot::Receiver<QuestionDecision>), QuestionError> {
        if stop.is_cancelled() {
            return Err(QuestionError::Stopped);
        }
        let mut state = lock(&self.state);
        let binding = state.binding.as_ref().ok_or(QuestionError::NotAttached)?;
        if origin.turn() == 0 || origin.occurrence().assistant_entry == 0 {
            return Err(QuestionError::Untracked);
        }
        if origin.session() != binding.session {
            return Err(QuestionError::ForeignSession);
        }
        let consumer = state
            .consumer
            .as_ref()
            .filter(|consumer| {
                consumer.owner == binding.owner && consumer.session == binding.session
            })
            .ok_or(QuestionError::NotAttached)?;
        if state.pending.is_some() {
            return Err(QuestionError::Busy);
        }
        let turn_stop = state
            .turn_stop
            .as_ref()
            .filter(|turn| turn.owner == consumer.owner && turn.session == consumer.session)
            .map(|turn| turn.stop.clone());
        if turn_stop
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(QuestionError::Stopped);
        }
        let id = QuestionRequestId(Uuid::now_v7());
        let (reply, receiver) = oneshot::channel();
        state.pending = Some(PendingState {
            id,
            owner: consumer.owner,
            session: consumer.session,
            origin,
            stop,
            turn_stop,
            questions,
            reply: Some(reply),
        });
        state.bump();
        drop(state);
        self.notify();
        Ok((id, receiver))
    }

    /// The tool waiter removes its own request when its future is dropped or
    /// observes a stop. This leaves no stale UI request that could be answered.
    pub(crate) fn withdraw(&self, id: QuestionRequestId) {
        let removed = {
            let mut state = lock(&self.state);
            if state
                .pending
                .as_ref()
                .is_none_or(|pending| pending.id != id)
            {
                return;
            }
            state.bump();
            state.pending.take()
        };
        drop(removed);
        self.notify();
    }
}

/// A non-clone live consumer lease. Dropping it revokes the pending batch and
/// cancels the original invocation's stop before waking the tool.
pub struct QuestionResponder {
    custody: Arc<QuestionCustody>,
    lease: Uuid,
}

impl QuestionResponder {
    pub fn pending(&self) -> Result<Option<PendingQuestion>, QuestionError> {
        let state = lock(&self.custody.state);
        let consumer = state
            .consumer
            .as_ref()
            .filter(|consumer| consumer.lease == self.lease)
            .ok_or(QuestionError::ForeignResponder)?;
        let binding = state.binding.as_ref().ok_or(QuestionError::Closed)?;
        if binding.owner != consumer.owner || binding.session != consumer.session {
            return Err(QuestionError::ForeignResponder);
        }
        Ok(state.pending.as_ref().map(|pending| PendingQuestion {
            request: pending.id,
            questions: pending.questions.clone(),
            origin: pending.origin,
        }))
    }

    pub fn answer(
        &self,
        request: QuestionRequestId,
        answers: Vec<QuestionAnswer>,
    ) -> Result<(), QuestionError> {
        self.custody.answer_inner(self.lease, request, answers)
    }

    /// Escape cancels only this batch. It never stops the Turn.
    pub fn cancel(&self, request: QuestionRequestId) -> Result<(), QuestionError> {
        self.custody
            .cancel_inner(self.lease, request, "user_cancelled")
    }
}

impl Drop for QuestionResponder {
    fn drop(&mut self) {
        self.custody.revoke(self.lease);
    }
}

fn cancel_pending_stops(pending: &PendingState) {
    if let Some(turn_stop) = &pending.turn_stop {
        turn_stop.cancel();
    }
    pending.stop.cancel();
}

fn validate_answers(
    questions: &[Question],
    answers: &[QuestionAnswer],
) -> Result<Vec<CompletedAnswer>, QuestionError> {
    if questions.len() != answers.len() {
        return Err(QuestionError::InvalidAnswer(
            "answer every question in the batch",
        ));
    }
    let mut completed = Vec::with_capacity(answers.len());
    for (index, (question, answer)) in questions.iter().zip(answers).enumerate() {
        let answer = match answer {
            QuestionAnswer::Option {
                index: option_index,
            } => {
                let Some(option) = question.options.get(*option_index) else {
                    return Err(QuestionError::InvalidAnswer(
                        "option index is outside this question",
                    ));
                };
                CompletedAnswer::Option {
                    question: index + 1,
                    option: *option_index,
                    label: option.label.clone(),
                }
            }
            QuestionAnswer::Text { text } => {
                if text.trim().is_empty() {
                    return Err(QuestionError::InvalidAnswer(
                        "a custom answer must not be empty",
                    ));
                }
                if text.len() > MAX_ANSWER_BYTES {
                    return Err(QuestionError::InvalidAnswer(
                        "a custom answer exceeds 4096 UTF-8 bytes",
                    ));
                }
                CompletedAnswer::Text {
                    question: index + 1,
                    text: text.clone(),
                }
            }
        };
        completed.push(answer);
    }
    // The serialized batch leaves headroom for result metadata. The input is
    // bounded per answer, but the encoded check is the actual contract.
    let encoded = serde_json::to_vec(&completed).expect("typed answers serialize");
    if encoded.len() > MAX_ANSWER_BATCH_BYTES {
        return Err(QuestionError::AnswerBatchTooLarge);
    }
    Ok(completed)
}

#[cfg(test)]
mod tests;
