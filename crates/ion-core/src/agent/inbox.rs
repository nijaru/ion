//! Atomic user-steering and automated-readiness intake at complete boundaries.
use super::*;
use crate::session::AssistantRecord;
use crate::{CommittedInput, CompletionInbox, CompletionNotice, InputCommit, PreparedInput};

/// Prepared steering and retained, ordered commit acknowledgements. Automated
/// readiness has independent receiving-owned custody across operations.
pub struct SteeringInbox<M = ()> {
    pub(super) state: Mutex<IntakeState<M>>,
    budget: crate::InputBudget,
    limits: AgentLimits,
    pub(super) completions: Option<Arc<CompletionInbox>>,
}

pub(super) struct IntakeState<M> {
    pending: VecDeque<PreparedInput<M>>,
    committed: VecDeque<CommittedInput>,
}

#[derive(Default)]
pub(super) struct CommittedInputs {
    pub steering: Vec<Message>,
    pub entries: crate::session::CommittedInputEntries,
    pub completions: Vec<CompletionNotice>,
}
impl CommittedInputs {
    pub(super) fn observe(self, turn: u64, observe: &mut impl FnMut(AgentEvent)) {
        assert_eq!(
            self.steering.len() as u64,
            self.entries.steering.end - self.entries.steering.start
        );
        for (entry, input) in self.entries.steering.zip(self.steering) {
            observe(AgentEvent::SteeringCommitted { turn, entry, input });
        }
        if !self.completions.is_empty() {
            observe(AgentEvent::CompletionCommitted {
                turn,
                entry: self
                    .entries
                    .completion
                    .expect("committed completion has a coordinate"),
                notices: self.completions,
                new_turn: false,
            });
        }
    }
}

impl<M> SteeringInbox<M> {
    pub fn new(limits: AgentLimits, budget: crate::InputBudget) -> Self {
        Self {
            state: Mutex::new(IntakeState {
                pending: VecDeque::new(),
                committed: VecDeque::new(),
            }),
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
        let input = self.budget.prepare(input, metadata, self.limits)?;
        self.push_prepared(input);
        Ok(())
    }
    pub fn push_prepared(&self, input: PreparedInput<M>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .push_back(input);
    }
    /// Snapshot identities still in the inbox, excluding the frozen commit batch.
    /// Membership can change immediately; only taking custody confirms withdrawal.
    pub fn pending_receipts(&self) -> Vec<crate::InputReceipt> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .iter()
            .map(PreparedInput::receipt)
            .collect()
    }

    /// Withdraw unclaimed prepared custody. A frozen commit owns its inputs
    /// outside this queue: absence, stop, or an abandoned wait proves nothing
    /// about their acceptance. Resolve those inputs through their receipts.
    pub fn take_uncommitted(&self) -> Vec<PreparedInput<M>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .drain(..)
            .collect()
    }
    /// Positive acknowledgements in durable steering order. Taking these
    /// releases their retained budget when the returned owners are dropped.
    pub fn take_committed(&self) -> Vec<CommittedInput> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .committed
            .drain(..)
            .collect()
    }
    fn freeze(&self, slot_open: bool) -> FrozenInput<'_, M> {
        let input = if slot_open {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pending
                .pop_front()
        } else {
            None
        };
        FrozenInput {
            inbox: self,
            input,
            returnable: true,
        }
    }
    // These blocking operators run inside the Turn's retained Session I/O lane.
    pub(super) fn record_pending(
        &self,
        session: &Session,
        turn: u64,
        limits: AgentLimits,
    ) -> Result<CommittedInputs, AgentError> {
        self.record_boundary(session, turn, None, limits)
            .map(|(_, inputs)| inputs)
    }
    pub(super) fn record_assistant(
        &self,
        session: &Session,
        turn: u64,
        assistant: AssistantRecord,
        limits: AgentLimits,
    ) -> Result<(crate::session::CommittedAssistant, CommittedInputs), AgentError> {
        let (committed, inputs) = self.record_boundary(session, turn, Some(assistant), limits)?;
        Ok((committed.expect("assistant was committed"), inputs))
    }
    fn record_boundary(
        &self,
        session: &Session,
        turn: u64,
        assistant: Option<AssistantRecord>,
        limits: AgentLimits,
    ) -> Result<(Option<crate::session::CommittedAssistant>, CommittedInputs), AgentError> {
        // A text-only assistant commits before the next steer in this same
        // transaction, so it opens the next slot. At request intake, use the
        // committed Session state: rebuilding a request does not open a slot.
        let mut inputs = self.freeze(assistant.is_some() || session.steering_slot_open(turn)?);
        if let Some(input) = &inputs.input {
            crate::input::validate_input(input.message(), limits)?;
        }
        let mut completion = self
            .completions
            .as_ref()
            .map(|inbox| {
                inbox.batch(
                    session,
                    Some(turn),
                    assistant.is_none() || inputs.input.is_some(),
                )
            })
            .transpose()?;
        let notices = completion
            .as_ref()
            .map_or_else(Vec::new, |batch| batch.notices());
        let messages = inputs
            .input
            .iter()
            .map(|input| input.message().clone())
            .collect();
        inputs.begin();
        if let Some(batch) = &mut completion {
            batch.begin();
        }
        let result = if let Some(assistant) = assistant {
            session
                .commit_assistant(turn, assistant, messages, notices.clone())
                .map(|(committed, entries)| (Some(committed), entries))
        } else {
            session
                .record_inputs(turn, messages, notices.clone())
                .map(|entries| (None, entries))
        };
        let (committed, entries) = match result {
            Ok(result) => result,
            Err(error) => {
                if !error.is_commit_uncertain() {
                    inputs.not_committed();
                    if let Some(batch) = completion {
                        batch.reject();
                    }
                }
                return Err(error.into());
            }
        };
        if let Some(batch) = completion {
            batch.commit(turn);
        }
        let steering = inputs.publish(session.id(), turn, entries.steering.clone());
        Ok((
            committed,
            CommittedInputs {
                steering,
                entries,
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

/// A claimed steer owns its input, not an inbox mutex. Known failures restore
/// it ahead of later arrivals; uncertain work is never replayed.
struct FrozenInput<'a, M> {
    inbox: &'a SteeringInbox<M>,
    input: Option<PreparedInput<M>>,
    returnable: bool,
}

impl<M> FrozenInput<'_, M> {
    fn begin(&mut self) {
        self.returnable = false;
        if let Some(input) = &self.input {
            input.begin();
        }
    }

    fn not_committed(&mut self) {
        if let Some(input) = &self.input {
            input.not_committed();
        }
        self.returnable = true;
    }

    fn publish(
        mut self,
        session: uuid::Uuid,
        turn: u64,
        entries: std::ops::Range<u64>,
    ) -> Vec<Message> {
        assert_eq!(u64::from(self.input.is_some()), entries.end - entries.start);
        let Some(input) = self.input.take() else {
            return Vec::new();
        };
        // Receipt wakeups and arbitrary metadata destruction must happen
        // outside the inbox lock.
        let (message, metadata, acknowledgement) = input.committed(InputCommit {
            session,
            turn,
            entry: entries.start,
        });
        if let Some(acknowledgement) = acknowledgement {
            self.inbox
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .committed
                .push_back(acknowledgement);
        }
        drop(metadata);
        vec![message]
    }
}

impl<M> Drop for FrozenInput<'_, M> {
    fn drop(&mut self) {
        if self.returnable
            && let Some(input) = self.input.take()
        {
            self.inbox
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pending
                .push_front(input);
        }
    }
}
