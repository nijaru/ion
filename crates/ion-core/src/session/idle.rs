//! Idle-conversation scheduling policy.
//!
//! Whether an admitted input starts a turn is one durable decision that depends
//! on the input's mode and on whether the conversation already has a live
//! foreground turn (`DESIGN.md` §11). Inputs admitted while the conversation is
//! busy are queued and drained when a settlement releases the slot, one input
//! per successor turn, without a coordinator task or a durable turn entity.
//!
//! The scheduler never invents a task kind: the shape of a conversation's turn
//! is configuration, so this module branches on no built-in name.

use serde_json::Value;

use super::{TaskDriver, TaskDriverError};
use crate::session::command::{SessionError, TaskRequest};
use crate::{ConversationId, InputMode, TaskId, TaskKindName};

/// The turn an idle conversation starts for a queued input.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnTemplate {
    pub kind: TaskKindName,
    pub schema_version: u32,
    pub input: Value,
}

impl TurnTemplate {
    #[must_use]
    pub fn new(kind: TaskKindName) -> Self {
        Self {
            kind,
            schema_version: 1,
            input: Value::Null,
        }
    }

    #[must_use]
    pub fn with_schema_version(mut self, schema_version: u32) -> Self {
        self.schema_version = schema_version;
        self
    }

    #[must_use]
    pub fn with_input(mut self, input: Value) -> Self {
        self.input = input;
        self
    }

    pub(super) fn request(&self, conversation_id: ConversationId) -> TaskRequest {
        TaskRequest {
            conversation_id,
            kind: self.kind.clone(),
            schema_version: self.schema_version,
            input: self.input.clone(),
            dependencies: Vec::new(),
        }
    }
}

/// Whether this mode starts a turn when the conversation is idle.
///
/// This is the eligibility rule for scheduling queued input: a notice is
/// retained without waking the conversation, and queue-only input waits for an
/// explicit turn.
pub(super) const fn starts_turn(mode: InputMode) -> bool {
    matches!(
        mode,
        InputMode::Submit | InputMode::Steer | InputMode::FollowUp
    )
}

/// What admission does with an input for a conversation's current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Admission {
    /// Admit the input and open the turn that answers it.
    StartTurn,
    /// Admit the input and leave it queued for a later turn.
    Queue,
    /// Refuse: this mode may not wait for the conversation to become free.
    Reject,
}

/// The mode/state admission policy.
///
/// A notice is retained without waking the conversation, and queue-only input
/// waits for an explicit turn. Steering a *busy* conversation is deferred to the
/// next turn boundary rather than injected mid-turn, which is not built yet.
pub(super) fn admission(mode: InputMode, busy: bool) -> Admission {
    match (busy, mode) {
        (false, _) if starts_turn(mode) => Admission::StartTurn,
        (false, _) => Admission::Queue,
        (true, InputMode::Submit) => Admission::Reject,
        (true, _) => Admission::Queue,
    }
}

impl TaskDriver {
    /// Whether this driver can actually run a turn of this shape.
    ///
    /// Scheduling must not bind input to a turn nobody can execute: a
    /// never-dispatched task with an unregistered kind settles `Unsupported`,
    /// which would leave the input assigned to a terminal task and unable to be
    /// answered again. The input stays queued so it can be scheduled once the
    /// kind is registered.
    fn runnable(&self, template: &TurnTemplate) -> bool {
        self.registry
            .read()
            .expect("task registry lock")
            .get(&template.kind, template.schema_version)
            .is_some()
    }

    /// Start and drive the next turn for `conversation_id` if it is idle and has
    /// a queued input whose mode starts one.
    ///
    /// The settlement path calls this after a turn releases its slot, so queued
    /// follow-ups continue the conversation without a client polling. It is also
    /// public for the cases a settlement cannot observe, such as a conversation
    /// reopened with inputs that were queued before the process stopped. Only one
    /// input is started per call; the successor turn's own settlement schedules
    /// the next.
    pub async fn schedule_next_turn(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Option<TaskId>, TaskDriverError> {
        let Some(template) = self.turn_template() else {
            return Ok(None);
        };
        if !self.runnable(&template) {
            return Ok(None);
        }
        let request = template.request(conversation_id);

        // The selection and the binding are separate writer acquisitions, so a
        // candidate can be answered by someone else in between. Reselecting then
        // cannot spin: a disposition only moves forward, so an invalidated
        // candidate is not selected again.
        loop {
            let queued = {
                let session = self.session.lock().await;
                session.ensure_open()?;
                session.next_schedulable_input(conversation_id)
            };
            let Some(input_id) = queued else {
                return Ok(None);
            };

            let task_id = {
                let mut session = self.session.lock().await;
                session.ensure_open()?;
                match session.bind_turn_for_input(input_id, request.clone()) {
                    Ok(task_id) => task_id,
                    // Another actor answered this input first; try the next one.
                    Err(
                        SessionError::InvalidInputDisposition(_) | SessionError::UnknownInput(_),
                    ) => {
                        continue;
                    }
                    // Another actor took the slot; there is nothing left to do.
                    Err(SessionError::ForegroundTurnBusy(_)) => return Ok(None),
                    Err(error) => return Err(error.into()),
                }
            };
            self.spawn_drive(task_id);
            return Ok(Some(task_id));
        }
    }
}
