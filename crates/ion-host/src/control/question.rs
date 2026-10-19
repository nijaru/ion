//! Shared-control access to the one live question consumer lease.
use crate::{
    PendingQuestion, QuestionError, QuestionResponder,
    question::{TurnStopBinding, TurnStopScope},
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

impl<M: Serialize + Send + 'static> super::SessionControl<M> {
    /// Attach the single interactive consumer. A configured custody without a
    /// consumer leaves the model tool unavailable rather than auto-answering.
    pub fn attach_question_responder(&self) -> Result<Option<QuestionResponder>, QuestionError> {
        if self.closed {
            return Err(QuestionError::Closed);
        }
        self.setup
            .questions
            .as_ref()
            .map(|questions| questions.attach(self.owner))
            .transpose()
    }

    pub(super) fn take_question_change(&mut self) -> bool {
        let Some(questions) = &self.setup.questions else {
            return false;
        };
        let generation = questions.generation();
        if generation == self.question_seen {
            return false;
        }
        self.question_seen = generation;
        true
    }

    pub(super) fn bind_turn_stop(&self, stop: CancellationToken) -> Option<TurnStopBinding> {
        self.setup.questions.as_ref().and_then(|questions| {
            questions.bind_turn_stop(self.owner, self.setup.session.id(), stop)
        })
    }

    pub(super) fn restore_turn_stop(&self, binding: Option<TurnStopBinding>) {
        if let (Some(questions), Some(binding)) = (&self.setup.questions, binding) {
            questions.restore_turn_stop(self.owner, self.setup.session.id(), binding);
        }
    }

    pub(super) fn clear_turn_stop(&self, scope: Option<TurnStopScope>) {
        if let (Some(questions), Some(scope)) = (&self.setup.questions, scope) {
            questions.clear_turn_stop(self.owner, self.setup.session.id(), scope);
        }
    }

    /// Privileged pull of the live request without the consumer lease. This is
    /// presentation/snapshot data, not answer authority.
    pub fn pending_question(&self) -> Option<PendingQuestion> {
        self.setup
            .questions
            .as_ref()
            .and_then(|questions| questions.pending_for(self.owner, self.setup.session.id()))
    }
}
