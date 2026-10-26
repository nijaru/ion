//! Atomic user-steering and automated-readiness intake at complete boundaries.
use super::*;
use crate::{CompletionInbox, CompletionNotice};

/// Host-owned prepared steering. Automated readiness retains independent,
/// receiving-owned custody, shared across this receiver's operations.
pub struct SteeringInbox<M = ()> {
    pub(super) pending: Mutex<VecDeque<crate::AcceptedInput<M>>>,
    budget: crate::InputBudget,
    limits: AgentLimits,
    pub(super) completions: Option<Arc<CompletionInbox>>,
}

#[derive(Default)]
pub(super) struct CommittedInputs {
    pub steering: Vec<Message>,
    pub completions: Vec<CompletionNotice>,
}
impl CommittedInputs {
    pub(super) fn observe(self, turn: u64, observe: &mut impl FnMut(AgentEvent)) {
        for input in self.steering {
            observe(AgentEvent::SteeringCommitted { turn, input });
        }
        if !self.completions.is_empty() {
            observe(AgentEvent::CompletionCommitted {
                turn,
                notices: self.completions,
                new_turn: false,
            });
        }
    }
}

impl<M> SteeringInbox<M> {
    pub fn new(limits: AgentLimits, budget: crate::InputBudget) -> Self {
        Self {
            pending: Mutex::new(VecDeque::new()),
            budget,
            limits,
            completions: None,
        }
    }
    pub fn with_completions(mut self, completions: Arc<CompletionInbox>) -> Self {
        self.completions = Some(completions);
        self
    }
    pub fn push_message(&self, input: Message, metadata: M) -> Result<(), AgentError>
    where
        M: serde::Serialize,
    {
        let input = self.budget.admit(input, metadata, self.limits)?;
        self.push_admitted(input);
        Ok(())
    }
    pub fn push_admitted(&self, input: crate::AcceptedInput<M>) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(input);
    }
    pub fn take_uncommitted(&self) -> Vec<crate::AcceptedInput<M>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect()
    }
    pub(super) fn record_pending(
        &self,
        session: &Session,
        turn: u64,
        limits: AgentLimits,
    ) -> Result<CommittedInputs, AgentError> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_steering(&pending, limits)?;
        let completion = self
            .completions
            .as_ref()
            .map(|inbox| inbox.batch(session, Some(turn), true))
            .transpose()?;
        let notices = completion
            .as_ref()
            .map_or_else(Vec::new, |batch| batch.notices());
        session.record_inputs(
            turn,
            pending
                .iter()
                .map(|input| input.message().clone())
                .collect(),
            notices.clone(),
        )?;
        if let Some(batch) = completion {
            batch.commit(turn);
        }
        let accepted = pending.drain(..).collect::<Vec<_>>();
        drop(pending);
        // User metadata may execute Drop code; never release it under a lock.
        Ok(CommittedInputs {
            steering: accepted
                .into_iter()
                .map(crate::AcceptedInput::into_message)
                .collect(),
            completions: notices,
        })
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "one atomic assistant/input transaction"
    )]
    pub(super) fn record_assistant(
        &self,
        session: &Session,
        turn: u64,
        message: Message,
        tool_activities: Vec<StoredToolActivity>,
        execution: ModelExecution,
        usage: ion_ai::Usage,
        limits: AgentLimits,
    ) -> Result<(bool, CommittedInputs), AgentError> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        validate_steering(&pending, limits)?;
        let completion = self
            .completions
            .as_ref()
            .map(|inbox| inbox.batch(session, Some(turn), !pending.is_empty()))
            .transpose()?;
        let notices = completion
            .as_ref()
            .map_or_else(Vec::new, |batch| batch.notices());
        let complete = session.record_assistant_with_inputs(
            turn,
            message,
            tool_activities,
            execution,
            usage,
            pending
                .iter()
                .map(|input| input.message().clone())
                .collect(),
            notices.clone(),
        )?;
        if let Some(batch) = completion {
            batch.commit(turn);
        }
        let accepted = pending.drain(..).collect::<Vec<_>>();
        drop(pending);
        Ok((
            complete,
            CommittedInputs {
                steering: accepted
                    .into_iter()
                    .map(crate::AcceptedInput::into_message)
                    .collect(),
                completions: notices,
            },
        ))
    }
}
impl SteeringInbox<()> {
    pub fn push(&self, prompt: String) -> Result<(), AgentError> {
        self.push_message(user_text(prompt), ())
    }
}
fn validate_steering<M>(
    pending: &VecDeque<crate::AcceptedInput<M>>,
    limits: AgentLimits,
) -> Result<(), AgentError> {
    for input in pending {
        crate::input::validate_input(input.message(), limits)?;
    }
    Ok(())
}
