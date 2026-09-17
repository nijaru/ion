//! Turn rows: steps, attempts, invocations and the transitions between them.
//!
//! Every transition here is one transaction, and every one re-reads the state it
//! is transitioning from rather than trusting a caller's copy. That is what
//! makes a stale executor's late write a refusal instead of a corruption: it
//! cannot dispatch, settle or complete an attempt or an invocation whose
//! generation, phase or state has already moved on.

use rusqlite::{Connection, OptionalExtension, params};

use super::codec::{decode, decode_optional, encode, id_optional};
use super::sequence::reserve;
use super::{SqliteStore, StoreError, id_from};
use crate::attempt::{AttemptState, ModelAttempt, ModelStep};
use crate::entry::{ASSISTANT_ENTRY, EntryKind, TOOL_ENTRY};
use crate::error::Error;
use crate::input::InputMode;
use crate::invocation::{InvocationOutcome, InvocationState, Resolution, ToolInvocation};
use crate::limits::SessionLimits;
use crate::store::Command;
use crate::turn::{Cancellation, PendingOutcome, Turn, TurnOutcome, TurnPhase};
use crate::view::TurnView;
use crate::{AttemptId, ConversationId, EntryId, InputId, InvocationId, StepId, TurnId};

/// Where a step could not start because a durable budget was exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepStart {
    Started(StepId),
    Limit { setting: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttemptStart {
    Started(AttemptId),
    Limit { setting: &'static str },
}

/// What settling an attempt decided.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Settlement {
    /// The assistant entry is durable and these calls must run, in order.
    Tools {
        entry: EntryId,
        invocations: Vec<InvocationId>,
    },
    /// The response had no calls: the turn is complete.
    Completed { entry: EntryId },
}

/// Progress after one invocation settled.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct InvocationProgress {
    pub(crate) entry: EntryId,
    /// Invocations of this exchange that are still not settled.
    pub(crate) remaining: usize,
    /// The next invocation to run, if the exchange still has calls.
    pub(crate) next: Option<InvocationId>,
}

/// What a cancellation request found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancelOutcome {
    Marked(Cancellation),
    AlreadyTerminal,
}

/// Read the unfinished turn of a conversation, if any.
pub(crate) struct ReadUnfinishedTurn {
    pub(crate) conversation: ConversationId,
}

impl Command for ReadUnfinishedTurn {
    type Output = Option<Turn>;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let raw: Option<i64> = store
            .connection
            .query_row(
                "SELECT id FROM turns WHERE conversation_id = ?1 AND outcome IS NULL",
                [self.conversation.get()],
                |row| row.get(0),
            )
            .optional()?;
        match raw {
            Some(raw) => read_turn(&store.connection, id_from::<TurnId>(raw)?),
            None => Ok(None),
        }
    }
}

pub(crate) struct ReadTurnView {
    pub(crate) turn: TurnId,
}

impl Command for ReadTurnView {
    type Output = Option<TurnView>;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let Some(turn) = read_turn(&store.connection, self.turn)? else {
            return Ok(None);
        };
        let step = match turn.phase {
            TurnPhase::ModelStep(step)
            | TurnPhase::Tools(step)
            | TurnPhase::Blocked { step, .. } => read_step(&store.connection, step)?,
            // A turn between steps, or already finished, is still inspectable:
            // the latest basis is what explains how it got here.
            TurnPhase::Ready => latest_step(&store.connection, self.turn)?,
        };
        let attempts = match &step {
            Some(step) => read_attempts(&store.connection, step.id)?,
            None => Vec::new(),
        };
        let invocations = read_invocations_for_turn(&store.connection, self.turn)?;
        Ok(Some(TurnView {
            turn,
            step,
            attempts,
            invocations,
        }))
    }
}

/// Freeze a new request basis and charge one logical step.
pub(crate) struct BeginStep {
    pub(crate) turn: TurnId,
    pub(crate) config_revision: crate::CommitSeq,
    pub(crate) model: ion_ai::ModelRef,
    pub(crate) instructions: String,
    pub(crate) context: Vec<ion_ai::Message>,
    pub(crate) controls: ion_ai::GenerationControls,
    pub(crate) tools: Vec<ion_ai::ToolSpec>,
    pub(crate) max_request_bytes: u32,
    pub(crate) limits: SessionLimits,
}

impl Command for BeginStep {
    type Output = StepStart;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let turn = read_turn(&transaction, self.turn)?.ok_or_else(|| {
            StoreError::Rejected(Error::Invalid(format!("turn {} does not exist", self.turn)))
        })?;
        if turn.is_terminal() {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "turn {} is already terminal",
                self.turn
            ))));
        }
        if turn.cancellation.requested {
            return Err(StoreError::Rejected(Error::Invalid(
                "cancellation is already marked for this turn".to_owned(),
            )));
        }
        let used: i64 = transaction.query_row(
            "SELECT steps_used FROM turns WHERE id = ?1",
            [self.turn.get()],
            |row| row.get(0),
        )?;
        let used = u32::try_from(used)
            .map_err(|_| StoreError::Rejected(Error::Corrupt("negative step count".to_owned())))?;
        if used >= turn.limits.max_model_steps {
            return Ok(StepStart::Limit {
                setting: "max_model_steps",
            });
        }

        // Place every steering input admitted since the last boundary. This is
        // the turn's complete-exchange boundary: the placed entries are inside
        // the basis captured below, so steering is answered by this request.
        let steering = {
            let mut statement = transaction.prepare(
                "SELECT id, mode FROM inputs \
                 WHERE conversation_id = ?1 AND disposition = 'queued' ORDER BY id ASC",
            )?;
            let mut rows = statement.query([turn.conversation.get()])?;
            let mut steering = Vec::new();
            while let Some(row) = rows.next()? {
                let id = id_from::<InputId>(row.get(0)?)?;
                let mode: InputMode = decode(&row.get::<_, String>(1)?)?;
                if mode == InputMode::Steer {
                    steering.push(id);
                }
            }
            steering
        };
        for input in steering {
            let body = super::input::read_body(&transaction, input)?;
            let mut reserved = reserve(&transaction, 1, true)?;
            let entry: EntryId = reserved.next()?;
            let _ = reserved.commit()?;
            let placement = body.placement();
            super::conversation::insert_entry(
                &transaction,
                super::conversation::NewEntry {
                    id: entry,
                    conversation: turn.conversation,
                    kind: &placement.kind,
                    data: &placement.data,
                    projection: &placement.projection,
                },
                self.limits,
                false,
            )?;
            transaction.execute(
                "UPDATE inputs SET disposition = 'placed', placed_entry = ?2, placed_turn = ?3 \
                 WHERE id = ?1",
                params![input.get(), entry.get(), self.turn.get()],
            )?;
        }

        let cut: Option<i64> = transaction
            .query_row(
                "SELECT MAX(id) FROM entries WHERE conversation_id = ?1",
                [turn.conversation.get()],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let cut = id_optional::<EntryId>(cut)?;

        let ordinal: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM steps WHERE turn_id = ?1",
            [self.turn.get()],
            |row| row.get(0),
        )?;
        let mut reserved = reserve(&transaction, 1, true)?;
        let step: StepId = reserved.next()?;
        let _ = reserved.commit()?;
        transaction.execute(
            "INSERT INTO steps \
             (id, turn_id, ordinal, cut, config_revision, model, instructions, context, \
              controls, tools, max_request_bytes) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                step.get(),
                self.turn.get(),
                ordinal,
                cut.map(EntryId::get),
                self.config_revision.get(),
                encode(&self.model)?,
                self.instructions,
                encode(&self.context)?,
                encode(&self.controls)?,
                encode(&self.tools)?,
                i64::from(self.max_request_bytes),
            ],
        )?;
        transaction.execute(
            "UPDATE turns SET phase = 'model_step', step = ?2, steps_used = ?3, \
             blocked_invocation = NULL WHERE id = ?1",
            params![self.turn.get(), step.get(), used + 1],
        )?;
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(StepStart::Started(step))
    }
}

/// Create the next physical attempt for a step.
pub(crate) struct PrepareAttempt {
    pub(crate) step: StepId,
}

impl Command for PrepareAttempt {
    type Output = AttemptStart;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let step = read_step(&transaction, self.step)?.ok_or_else(|| {
            StoreError::Rejected(Error::Invalid(format!("step {} does not exist", self.step)))
        })?;
        let turn = read_turn(&transaction, step.turn)?.ok_or_else(|| {
            StoreError::Rejected(Error::Invalid(format!("turn {} does not exist", step.turn)))
        })?;
        let used: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM attempts WHERE step_id = ?1",
            [self.step.get()],
            |row| row.get(0),
        )?;
        let used = u32::try_from(used).unwrap_or(u32::MAX);
        if used >= turn.limits.max_attempts_per_step {
            return Ok(AttemptStart::Limit {
                setting: "max_attempts_per_step",
            });
        }
        let ordinal: i64 = transaction.query_row(
            "SELECT COALESCE(MAX(ordinal), -1) + 1 FROM attempts WHERE step_id = ?1",
            [self.step.get()],
            |row| row.get(0),
        )?;
        let mut reserved = reserve(&transaction, 1, true)?;
        let attempt: AttemptId = reserved.next()?;
        let _ = reserved.commit()?;
        transaction.execute(
            "INSERT INTO attempts (id, step_id, ordinal, generation, state) \
             VALUES (?1, ?2, ?3, ?4, 'prepared')",
            params![
                attempt.get(),
                self.step.get(),
                ordinal,
                super::codec::as_i64(turn.cancellation.generation)?
            ],
        )?;
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(AttemptStart::Started(attempt))
    }
}

/// Commit dispatch intent: the provider call may now leave this process.
pub(crate) struct CommitDispatch {
    pub(crate) attempt: AttemptId,
    pub(crate) generation: u64,
}

impl Command for CommitDispatch {
    type Output = ();
    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE attempts SET state = 'dispatched', generation = ?2 \
             WHERE id = ?1 AND state = 'prepared'",
            params![self.attempt.get(), super::codec::as_i64(self.generation)?],
        )?;
        if changed != 1 {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "attempt {} is not prepared",
                self.attempt
            ))));
        }
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(())
    }
}

/// Retire an attempt whose dispatch outcome was never recorded.
///
/// Only the state transition is durable here. What the provider may have
/// charged is preserved as unknown cost, and a retry is a *new* attempt with
/// its own ordinal, charged against the same step budget.
pub(crate) struct RetireAttempt {
    pub(crate) attempt: AttemptId,
}

impl Command for RetireAttempt {
    type Output = bool;
    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE attempts SET state = 'indeterminate' \
             WHERE id = ?1 AND state = 'dispatched'",
            [self.attempt.get()],
        )?;
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(changed == 1)
    }
}

/// Record a validated response. No transcript or tool decision is made yet.
pub(crate) struct CommitResponse {
    pub(crate) attempt: AttemptId,
    pub(crate) generation: u64,
    pub(crate) response: ion_ai::ModelResponse,
}

impl Command for CommitResponse {
    type Output = ();
    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE attempts SET state = 'response_ready', response = ?2 \
             WHERE id = ?1 AND state = 'dispatched' AND generation = ?3",
            params![
                self.attempt.get(),
                encode(&self.response)?,
                super::codec::as_i64(self.generation)?
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "attempt {} is no longer the current dispatch",
                self.attempt
            ))));
        }
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(())
    }
}

/// One admitted call, with the implementation facts the host resolved.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AdmittedCall {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) arguments: serde_json::Value,
    pub(crate) implementation: String,
    pub(crate) repeat_safe: bool,
}

/// Consume response-ready evidence: append the assistant entry and admit calls.
pub(crate) struct SettleAttempt {
    pub(crate) attempt: AttemptId,
    pub(crate) calls: Vec<AdmittedCall>,
    pub(crate) limits: SessionLimits,
}

impl Command for SettleAttempt {
    type Output = Settlement;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let attempt = read_attempt(&transaction, self.attempt)?.ok_or_else(|| {
            StoreError::Rejected(Error::Invalid(format!(
                "attempt {} does not exist",
                self.attempt
            )))
        })?;
        if attempt.state != AttemptState::ResponseReady {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "attempt {} is not response-ready",
                self.attempt
            ))));
        }
        let step = read_step(&transaction, attempt.step)?.ok_or_else(|| {
            StoreError::Rejected(Error::Corrupt(format!(
                "attempt {} references a missing step",
                self.attempt
            )))
        })?;
        let turn = read_turn(&transaction, step.turn)?.ok_or_else(|| {
            StoreError::Rejected(Error::Corrupt(format!(
                "step {} references a missing turn",
                step.id
            )))
        })?;
        let response = attempt.response.clone().ok_or_else(|| {
            StoreError::Rejected(Error::Corrupt(format!(
                "attempt {} is response-ready without a response",
                self.attempt
            )))
        })?;

        let mut reserved = reserve(&transaction, 1, true)?;
        let entry: EntryId = reserved.next()?;
        let _ = reserved.commit()?;
        let kind = EntryKind::builtin(ASSISTANT_ENTRY);
        let data = serde_json::json!({
            "termination": response.termination,
            "usage": response.usage,
        });
        super::conversation::insert_entry(
            &transaction,
            super::conversation::NewEntry {
                id: entry,
                conversation: turn.conversation,
                kind: &kind,
                data: &data,
                projection: std::slice::from_ref(&response.message),
            },
            self.limits,
            // The answer is settlement evidence: ordinary admission growth must
            // not be able to leave an accepted turn with nowhere to record what
            // the provider already returned.
            true,
        )?;
        transaction.execute(
            "UPDATE attempts SET state = 'settled', response = NULL WHERE id = ?1",
            [self.attempt.get()],
        )?;

        let mut invocations = Vec::with_capacity(self.calls.len());
        for (index, call) in self.calls.iter().enumerate() {
            let mut reserved = reserve(&transaction, 1, true)?;
            let invocation: InvocationId = reserved.next()?;
            let _ = reserved.commit()?;
            transaction.execute(
                "INSERT INTO invocations \
                 (id, step_id, entry_id, call_index, call_id, name, arguments, \
                  implementation, repeat_safe, generation, state) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'prepared')",
                params![
                    invocation.get(),
                    step.id.get(),
                    entry.get(),
                    i64::try_from(index).unwrap_or(i64::MAX),
                    call.id,
                    call.name,
                    encode(&call.arguments)?,
                    call.implementation,
                    i64::from(call.repeat_safe),
                    super::codec::as_i64(turn.cancellation.generation)?,
                ],
            )?;
            invocations.push(invocation);
        }

        let settlement = if invocations.is_empty() {
            transaction.execute(
                "UPDATE turns SET outcome = ?2, phase = 'ready', step = NULL WHERE id = ?1",
                params![turn.id.get(), encode(&TurnOutcome::Completed { entry })?],
            )?;
            Settlement::Completed { entry }
        } else {
            transaction.execute(
                "UPDATE turns SET phase = 'tools', step = ?2 WHERE id = ?1",
                params![turn.id.get(), step.id.get()],
            )?;
            Settlement::Tools { entry, invocations }
        };
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(settlement)
    }
}

/// Commit dispatch intent for one invocation.
pub(crate) struct CommitInvocationDispatch {
    pub(crate) invocation: InvocationId,
    pub(crate) generation: u64,
}

impl Command for CommitInvocationDispatch {
    type Output = ();
    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let changed = transaction.execute(
            "UPDATE invocations SET state = 'dispatched', generation = ?2 \
             WHERE id = ?1 AND state = 'prepared'",
            params![
                self.invocation.get(),
                super::codec::as_i64(self.generation)?
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "invocation {} is not prepared",
                self.invocation
            ))));
        }
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(())
    }
}

/// Record one invocation's truthful result and advance the turn.
pub(crate) struct CommitInvocationResult {
    pub(crate) invocation: InvocationId,
    pub(crate) generation: u64,
    pub(crate) outcome: InvocationOutcome,
    pub(crate) limits: SessionLimits,
}

impl Command for CommitInvocationResult {
    type Output = InvocationProgress;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let invocation = read_invocation(&transaction, self.invocation)?.ok_or_else(|| {
            StoreError::Rejected(Error::Invalid(format!(
                "invocation {} does not exist",
                self.invocation
            )))
        })?;
        // Before dispatch only a known refusal can be recorded; success or
        // uncertainty requires dispatch intent. Settled calls cannot be revised.
        if !(invocation.state == InvocationState::Dispatched
            || (invocation.state == InvocationState::Prepared
                && matches!(self.outcome, InvocationOutcome::Failed { .. })))
        {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "invocation {} in state {} cannot accept this result",
                self.invocation,
                invocation.state.as_str(),
            ))));
        }
        if invocation.generation != self.generation {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "invocation {} belongs to generation {}, not {}",
                self.invocation, invocation.generation, self.generation
            ))));
        }
        // An invocation's stored generation is the turn's cancellation
        // generation at dispatch time. A cancellation that committed since then
        // supersedes this action's right to settle itself: a joined action's
        // truthful report is recorded by the turn's terminal settlement, which
        // preserves the fact without authorizing another step.
        let current: i64 = transaction.query_row(
            "SELECT t.generation FROM turns t JOIN steps s ON s.turn_id = t.id WHERE s.id = ?1",
            [invocation.step.get()],
            |row| row.get(0),
        )?;
        let current = u64::try_from(current).map_err(|_| {
            StoreError::Rejected(Error::Corrupt(format!(
                "the turn owning invocation {} has a negative generation",
                self.invocation
            )))
        })?;
        if current != self.generation {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "invocation {} was dispatched under generation {} and its turn has moved to {current}",
                self.invocation, self.generation
            ))));
        }
        settle_invocation(&transaction, &invocation, self.outcome, self.limits)?;
        let progress = invocation_progress(&transaction, invocation.step, invocation.entry)?;
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(progress)
    }
}

/// Resolve an invocation whose outcome is unknown, after explicit client input.
pub(crate) struct ResolveInvocation {
    pub(crate) turn: TurnId,
    pub(crate) invocation: InvocationId,
    pub(crate) resolution: Resolution,
    pub(crate) limits: SessionLimits,
}

impl Command for ResolveInvocation {
    type Output = InvocationProgress;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let turn = read_turn(&transaction, self.turn)?.ok_or_else(|| {
            StoreError::Rejected(Error::Invalid(format!("turn {} does not exist", self.turn)))
        })?;
        if turn.is_terminal() {
            return Err(StoreError::Rejected(Error::Invalid(format!(
                "turn {} is already terminal",
                self.turn
            ))));
        }
        let invocation = read_invocation(&transaction, self.invocation)?.ok_or(
            StoreError::Rejected(Error::UnknownInvocation {
                turn: self.turn,
                invocation: self.invocation,
            }),
        )?;
        let (step, blocked) = match turn.phase {
            TurnPhase::Blocked { step, invocation } => (step, invocation),
            _ => {
                return Err(StoreError::Rejected(Error::InvocationNotResolvable(
                    self.invocation,
                )));
            }
        };
        if blocked != self.invocation {
            return Err(StoreError::Rejected(Error::InvocationNotResolvable(
                self.invocation,
            )));
        }
        let progress = match self.resolution {
            Resolution::Repeat => {
                if !invocation.repeat_safe {
                    return Err(StoreError::Rejected(Error::NotRepeatSafe(self.invocation)));
                }
                transaction.execute(
                    "UPDATE invocations SET state = 'prepared', generation = ?2, \
                     result = NULL, message = NULL WHERE id = ?1",
                    params![
                        self.invocation.get(),
                        super::codec::as_i64(turn.cancellation.generation)?
                    ],
                )?;
                transaction.execute(
                    "UPDATE turns SET phase = 'tools', step = ?2, blocked_invocation = NULL WHERE id = ?1",
                    params![self.turn.get(), step.get()],
                )?;
                invocation_progress(&transaction, step, invocation.entry)?
            }
            Resolution::Indeterminate => {
                // A call that was dispatched with no result gets one now. A
                // call that already recorded an indeterminate result keeps it:
                // appending a second result for the same call would rewrite the
                // exchange instead of settling it.
                if invocation.state == InvocationState::Dispatched {
                    settle_invocation(
                        &transaction,
                        &invocation,
                        InvocationOutcome::Indeterminate {
                            message:
                                "the action may have happened; its outcome was not established"
                                    .to_owned(),
                        },
                        self.limits,
                    )?;
                }
                // A resolved uncertainty is recorded, not rewritten as a known
                // outcome, and it stops blocking the exchange.
                transaction.execute(
                    "UPDATE invocations SET state = 'unknown' WHERE id = ?1",
                    [self.invocation.get()],
                )?;
                invocation_progress(&transaction, step, invocation.entry)?
            }
        };
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(progress)
    }
}

/// Mark cancellation durably. This is intent, not a claim that anything stopped.
pub(crate) struct CancelTurn {
    pub(crate) turn: TurnId,
}

impl Command for CancelTurn {
    type Output = CancelOutcome;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let turn = read_turn(&transaction, self.turn)?.ok_or_else(|| {
            StoreError::Rejected(Error::Invalid(format!("turn {} does not exist", self.turn)))
        })?;
        if turn.is_terminal() {
            return Ok(CancelOutcome::AlreadyTerminal);
        }
        transaction.execute(
            "UPDATE turns SET cancel_requested = 1, generation = generation + 1 WHERE id = ?1",
            [self.turn.get()],
        )?;
        let marked = Cancellation {
            requested: true,
            generation: turn.cancellation.generation + 1,
        };
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(CancelOutcome::Marked(marked))
    }
}

/// Finish a turn, settling anything still unresolved truthfully.
///
/// Cancellation and abandonment do not rewrite actions as failed. A call whose
/// side effect may have happened becomes indeterminate with a truthful result;
/// a call that was admitted but never dispatched records that it did not run.
pub(crate) struct FinishTurn {
    pub(crate) turn: TurnId,
    pub(crate) outcome: PendingOutcome,
    /// Truthful outcomes a joined action reported after cancellation committed.
    ///
    /// The generation fence keeps a superseded action from settling itself, so
    /// this evidence reaches durable state here instead: it is a fact about an
    /// action, and recording it does not resume the cancelled continuation.
    pub(crate) evidence: Vec<(InvocationId, InvocationOutcome)>,
    pub(crate) limits: SessionLimits,
}

impl Command for FinishTurn {
    type Output = TurnOutcome;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let turn = read_turn(&transaction, self.turn)?.ok_or_else(|| {
            StoreError::Rejected(Error::Invalid(format!("turn {} does not exist", self.turn)))
        })?;
        if let Some(outcome) = &turn.outcome {
            return Ok(outcome.clone());
        }
        // A joined action's report is applied only to an invocation that is
        // still waiting for one. A settled or never-dispatched invocation is
        // never rewritten, and this turn is guaranteed non-terminal above.
        for (invocation, reported) in self.evidence {
            let Some(dispatched) = read_invocation(&transaction, invocation)? else {
                continue;
            };
            if dispatched.state != InvocationState::Dispatched {
                continue;
            }
            settle_invocation(&transaction, &dispatched, reported, self.limits)?;
        }
        let invocations = read_invocations_for_turn(&transaction, self.turn)?;
        let mut unresolved = Vec::new();
        for invocation in &invocations {
            match invocation.state {
                InvocationState::Succeeded { .. } | InvocationState::Failed { .. } => {}
                InvocationState::Indeterminate | InvocationState::Unknown => {
                    unresolved.push(invocation.id);
                }
                InvocationState::Dispatched => {
                    settle_invocation(
                        &transaction,
                        invocation,
                        InvocationOutcome::Indeterminate {
                            message:
                                "the action may have happened; its outcome was not established"
                                    .to_owned(),
                        },
                        self.limits,
                    )?;
                    unresolved.push(invocation.id);
                }
                InvocationState::Prepared => {
                    settle_invocation(
                        &transaction,
                        invocation,
                        InvocationOutcome::Failed {
                            message: "the action was not run".to_owned(),
                        },
                        self.limits,
                    )?;
                }
            }
        }
        let outcome = self.outcome.finalize(unresolved);
        transaction.execute(
            "UPDATE turns SET outcome = ?2, phase = 'ready', step = NULL, blocked_invocation = NULL \
             WHERE id = ?1",
            params![self.turn.get(), encode(&outcome)?],
        )?;
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(outcome)
    }
}

/// Start a successor turn for a queued input after the previous turn closed.
pub(crate) struct StartSuccessor {
    pub(crate) conversation: ConversationId,
    pub(crate) input: InputId,
    pub(crate) limits: SessionLimits,
    pub(crate) now_unix_ms: i64,
}

impl Command for StartSuccessor {
    type Output = TurnId;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError> {
        let connection = &mut store.connection;
        let transaction = connection.transaction()?;
        let config = super::conversation::read_config(&transaction, self.conversation)?
            .ok_or_else(|| {
                StoreError::Rejected(Error::Invalid(format!(
                    "conversation {} has no installed configuration",
                    self.conversation
                )))
            })?;
        let unfinished: Option<i64> = transaction
            .query_row(
                "SELECT id FROM turns WHERE conversation_id = ?1 AND outcome IS NULL",
                [self.conversation.get()],
                |row| row.get(0),
            )
            .optional()?;
        if unfinished.is_some() {
            return Err(StoreError::Rejected(Error::Busy(self.conversation)));
        }
        let disposition: Option<String> = transaction
            .query_row(
                "SELECT disposition FROM inputs WHERE id = ?1",
                [self.input.get()],
                |row| row.get(0),
            )
            .optional()?;
        if disposition.as_deref() != Some("queued") {
            return Err(StoreError::Rejected(Error::NoQueuedInput));
        }
        let body = super::input::read_body(&transaction, self.input)?;
        let mut reserved = reserve(&transaction, 2, true)?;
        let turn: TurnId = reserved.next()?;
        let entry: EntryId = reserved.next()?;
        let _ = reserved.commit()?;
        let placement = body.placement();
        super::conversation::insert_entry(
            &transaction,
            super::conversation::NewEntry {
                id: entry,
                conversation: self.conversation,
                kind: &placement.kind,
                data: &placement.data,
                projection: &placement.projection,
            },
            self.limits,
            false,
        )?;
        transaction.execute(
            "UPDATE inputs SET disposition = 'placed', placed_entry = ?2, placed_turn = ?3 WHERE id = ?1",
            params![self.input.get(), entry.get(), turn.get()],
        )?;
        transaction.execute(
            "INSERT INTO turns \
             (id, conversation_id, phase, generation, cancel_requested, limits, admitted_at, steps_used) \
             VALUES (?1, ?2, 'ready', 0, 0, ?3, ?4, 0)",
            params![
                turn.get(),
                self.conversation.get(),
                encode(&config.config.limits)?,
                self.now_unix_ms
            ],
        )?;
        #[cfg(test)]
        super::check_fault(&store.fault)?;
        transaction.commit()?;
        Ok(turn)
    }
}

fn read_turn(connection: &Connection, id: TurnId) -> Result<Option<Turn>, StoreError> {
    type Row = (
        i64,
        String,
        Option<i64>,
        Option<i64>,
        i64,
        i64,
        String,
        i64,
        Option<String>,
    );
    let row: Option<Row> = connection
        .query_row(
            "SELECT conversation_id, phase, step, blocked_invocation, generation, \
             cancel_requested, limits, admitted_at, outcome FROM turns WHERE id = ?1",
            [id.get()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )
        .optional()?;
    let Some((
        conversation,
        phase,
        step,
        blocked,
        generation,
        cancel_requested,
        limits,
        admitted_at,
        outcome,
    )) = row
    else {
        return Ok(None);
    };
    let step = id_optional::<StepId>(step)?;
    let blocked = id_optional::<InvocationId>(blocked)?;
    let phase = match phase.as_str() {
        "ready" => TurnPhase::Ready,
        "model_step" => TurnPhase::ModelStep(missing(step, id, "a model step")?),
        "tools" => TurnPhase::Tools(missing(step, id, "running tools")?),
        "blocked" => TurnPhase::Blocked {
            step: missing(step, id, "blocked")?,
            invocation: blocked.ok_or_else(|| {
                StoreError::Rejected(Error::Corrupt(format!(
                    "turn {id} is blocked without an invocation"
                )))
            })?,
        },
        other => {
            return Err(StoreError::Rejected(Error::Corrupt(format!(
                "turn {id} has unknown phase {other:?}"
            ))));
        }
    };
    Ok(Some(Turn {
        id,
        conversation: id_from::<ConversationId>(conversation)?,
        phase,
        cancellation: Cancellation {
            requested: cancel_requested != 0,
            generation: u64::try_from(generation).map_err(|_| {
                StoreError::Rejected(Error::Corrupt(format!(
                    "turn {id} has a negative generation"
                )))
            })?,
        },
        limits: decode(&limits)?,
        admitted_at_unix_ms: admitted_at,
        outcome: decode_optional::<TurnOutcome>(outcome)?,
    }))
}

fn missing(step: Option<StepId>, turn: TurnId, what: &str) -> Result<StepId, StoreError> {
    step.ok_or_else(|| {
        StoreError::Rejected(Error::Corrupt(format!(
            "turn {turn} is {what} without a step"
        )))
    })
}

fn read_step(connection: &Connection, id: StepId) -> Result<Option<ModelStep>, StoreError> {
    type Row = (
        i64,
        i64,
        Option<i64>,
        i64,
        String,
        String,
        String,
        String,
        String,
        i64,
    );
    let row: Option<Row> = connection
        .query_row(
            "SELECT turn_id, ordinal, cut, config_revision, model, instructions, context, \
             controls, tools, max_request_bytes FROM steps WHERE id = ?1",
            [id.get()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .optional()?;
    let Some((
        turn,
        ordinal,
        cut,
        revision,
        model,
        instructions,
        context,
        controls,
        tools,
        max_request_bytes,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(ModelStep {
        id,
        turn: id_from::<TurnId>(turn)?,
        ordinal: u32::try_from(ordinal).map_err(|_| {
            StoreError::Rejected(Error::Corrupt(format!("step {id} has a negative ordinal")))
        })?,
        cut: id_optional::<EntryId>(cut)?,
        config_revision: id_from::<crate::CommitSeq>(revision)?,
        model: decode(&model)?,
        instructions,
        context: decode(&context)?,
        controls: decode(&controls)?,
        tools: decode(&tools)?,
        max_request_bytes: u32::try_from(max_request_bytes).map_err(|_| {
            StoreError::Rejected(Error::Corrupt(format!(
                "step {id} has an invalid request byte budget"
            )))
        })?,
    }))
}

fn read_attempt(
    connection: &Connection,
    id: AttemptId,
) -> Result<Option<ModelAttempt>, StoreError> {
    type Row = (i64, i64, i64, String, Option<String>);
    let row: Option<Row> = connection
        .query_row(
            "SELECT step_id, ordinal, generation, state, response FROM attempts WHERE id = ?1",
            [id.get()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let Some((step, ordinal, generation, state, response)) = row else {
        return Ok(None);
    };
    Ok(Some(ModelAttempt {
        id,
        step: id_from::<StepId>(step)?,
        ordinal: u32::try_from(ordinal).map_err(|_| {
            StoreError::Rejected(Error::Corrupt(format!(
                "attempt {id} has a negative ordinal"
            )))
        })?,
        generation: u64::try_from(generation).map_err(|_| {
            StoreError::Rejected(Error::Corrupt(format!(
                "attempt {id} has a negative generation"
            )))
        })?,
        state: AttemptState::parse(&state).ok_or_else(|| {
            StoreError::Rejected(Error::Corrupt(format!(
                "attempt {id} has unknown state {state:?}"
            )))
        })?,
        response: decode_optional::<ion_ai::ModelResponse>(response)?,
    }))
}

fn latest_step(connection: &Connection, turn: TurnId) -> Result<Option<ModelStep>, StoreError> {
    let raw: Option<Option<i64>> = connection
        .query_row(
            "SELECT MAX(id) FROM steps WHERE turn_id = ?1",
            [turn.get()],
            |row| row.get(0),
        )
        .optional()?;
    match raw.flatten() {
        Some(raw) => read_step(connection, id_from::<StepId>(raw)?),
        None => Ok(None),
    }
}

fn read_attempts(connection: &Connection, step: StepId) -> Result<Vec<ModelAttempt>, StoreError> {
    let ids = collect_ids(
        connection,
        "SELECT id FROM attempts WHERE step_id = ?1 ORDER BY ordinal ASC",
        step.get(),
    )?;
    let mut attempts = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(attempt) = read_attempt(connection, id_from::<AttemptId>(id)?)? {
            attempts.push(attempt);
        }
    }
    Ok(attempts)
}

fn read_invocation(
    connection: &Connection,
    id: InvocationId,
) -> Result<Option<ToolInvocation>, StoreError> {
    type Row = (
        i64,
        i64,
        i64,
        String,
        String,
        String,
        String,
        i64,
        i64,
        String,
        Option<String>,
        Option<String>,
    );
    let row: Option<Row> = connection
        .query_row(
            "SELECT step_id, entry_id, call_index, call_id, name, arguments, implementation, \
             repeat_safe, generation, state, result, message FROM invocations WHERE id = ?1",
            [id.get()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                ))
            },
        )
        .optional()?;
    let Some((
        step,
        entry,
        call_index,
        call_id,
        name,
        arguments,
        implementation,
        repeat_safe,
        generation,
        state,
        result,
        message,
    )) = row
    else {
        return Ok(None);
    };
    let state = match state.as_str() {
        "prepared" => InvocationState::Prepared,
        "dispatched" => InvocationState::Dispatched,
        "succeeded" => InvocationState::Succeeded {
            result: decode::<serde_json::Value>(result.as_deref().ok_or_else(|| {
                StoreError::Rejected(Error::Corrupt(format!(
                    "invocation {id} succeeded without a result"
                )))
            })?)?,
        },
        "failed" => InvocationState::Failed {
            message: message.clone().unwrap_or_default(),
        },
        "indeterminate" => InvocationState::Indeterminate,
        "unknown" => InvocationState::Unknown,
        other => {
            return Err(StoreError::Rejected(Error::Corrupt(format!(
                "invocation {id} has unknown state {other:?}"
            ))));
        }
    };
    Ok(Some(ToolInvocation {
        id,
        step: id_from::<StepId>(step)?,
        entry: id_from::<EntryId>(entry)?,
        call_index: u32::try_from(call_index).map_err(|_| {
            StoreError::Rejected(Error::Corrupt(format!(
                "invocation {id} has a negative call index"
            )))
        })?,
        call: ion_ai::ToolCall {
            id: call_id,
            name,
            arguments: decode(&arguments)?,
        },
        implementation,
        repeat_safe: repeat_safe != 0,
        generation: u64::try_from(generation).map_err(|_| {
            StoreError::Rejected(Error::Corrupt(format!(
                "invocation {id} has a negative generation"
            )))
        })?,
        state,
    }))
}

fn read_invocations_for_entry(
    connection: &Connection,
    entry: EntryId,
) -> Result<Vec<ToolInvocation>, StoreError> {
    let ids = collect_ids(
        connection,
        "SELECT id FROM invocations WHERE entry_id = ?1 ORDER BY call_index ASC",
        entry.get(),
    )?;
    let mut invocations = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(invocation) = read_invocation(connection, id_from::<InvocationId>(id)?)? {
            invocations.push(invocation);
        }
    }
    Ok(invocations)
}

fn read_invocations_for_turn(
    connection: &Connection,
    turn: TurnId,
) -> Result<Vec<ToolInvocation>, StoreError> {
    let ids = collect_ids(
        connection,
        "SELECT id FROM invocations WHERE step_id IN \
         (SELECT id FROM steps WHERE turn_id = ?1) ORDER BY id ASC",
        turn.get(),
    )?;
    let mut invocations = Vec::with_capacity(ids.len());
    for id in ids {
        if let Some(invocation) = read_invocation(connection, id_from::<InvocationId>(id)?)? {
            invocations.push(invocation);
        }
    }
    Ok(invocations)
}

fn collect_ids(connection: &Connection, sql: &str, param: i64) -> Result<Vec<i64>, StoreError> {
    let mut statement = connection.prepare(sql)?;
    let mut rows = statement.query([param])?;
    let mut ids = Vec::new();
    while let Some(row) = rows.next()? {
        ids.push(row.get(0)?);
    }
    Ok(ids)
}

/// Append a truthful tool result and mark the invocation settled.
fn settle_invocation(
    connection: &Connection,
    invocation: &ToolInvocation,
    outcome: InvocationOutcome,
    limits: SessionLimits,
) -> Result<EntryId, StoreError> {
    let conversation = conversation_of_entry(connection, invocation.entry)?;
    let mut reserved = reserve(connection, 1, true)?;
    let entry: EntryId = reserved.next()?;
    let _ = reserved.commit()?;
    let result = outcome.result_value(&invocation.call);
    let projection = vec![ion_ai::Message {
        role: ion_ai::Role::Tool,
        content: vec![ion_ai::Content::ToolResult(ion_ai::ToolResult {
            call_id: invocation.call.id.clone(),
            name: invocation.call.name.clone(),
            result: result.clone(),
        })],
        provider_replay: None,
    }];
    let kind = EntryKind::builtin(TOOL_ENTRY);
    let data = serde_json::json!({"call": invocation.call.id, "index": invocation.call_index});
    super::conversation::insert_entry(
        connection,
        super::conversation::NewEntry {
            id: entry,
            conversation,
            kind: &kind,
            data: &data,
            projection: &projection,
        },
        limits,
        true,
    )?;
    match &outcome.state() {
        InvocationState::Succeeded { result } => {
            connection.execute(
                "UPDATE invocations SET state = 'succeeded', result = ?2, message = NULL WHERE id = ?1",
                params![invocation.id.get(), encode(result)?],
            )?;
        }
        InvocationState::Failed { message } => {
            connection.execute(
                "UPDATE invocations SET state = 'failed', message = ?2, result = NULL WHERE id = ?1",
                params![invocation.id.get(), message],
            )?;
        }
        InvocationState::Indeterminate => {
            connection.execute(
                "UPDATE invocations SET state = 'indeterminate', result = NULL, \
                 message = 'the action may have happened; its outcome was not established' \
                 WHERE id = ?1",
                [invocation.id.get()],
            )?;
        }
        InvocationState::Prepared | InvocationState::Dispatched | InvocationState::Unknown => {
            return Err(StoreError::Failed(
                "a settlement produced a non-terminal invocation state".to_owned(),
            ));
        }
    }
    Ok(entry)
}

fn conversation_of_entry(
    connection: &Connection,
    entry: EntryId,
) -> Result<ConversationId, StoreError> {
    let raw: i64 = connection.query_row(
        "SELECT conversation_id FROM entries WHERE id = ?1",
        [entry.get()],
        |row| row.get(0),
    )?;
    id_from::<ConversationId>(raw)
}

/// Advance the turn once one invocation of an assistant message has settled.
fn invocation_progress(
    connection: &Connection,
    step: StepId,
    entry: EntryId,
) -> Result<InvocationProgress, StoreError> {
    let invocations = read_invocations_for_entry(connection, entry)?;
    let next = invocations
        .iter()
        .find(|invocation| invocation.state == InvocationState::Prepared)
        .map(|invocation| invocation.id);
    // A call that may have run with no recorded outcome blocks the whole
    // exchange: dispatching anything after it would silently build on a fact
    // nobody established.
    let pending = invocations
        .iter()
        .filter(|invocation| {
            matches!(
                invocation.state,
                InvocationState::Prepared
                    | InvocationState::Dispatched
                    | InvocationState::Indeterminate
            )
        })
        .count();
    let blocked = invocations
        .iter()
        .find(|invocation| {
            matches!(
                invocation.state,
                InvocationState::Dispatched | InvocationState::Indeterminate
            )
        })
        .map(|invocation| invocation.id);

    let turn: Option<i64> = connection
        .query_row(
            "SELECT id FROM turns WHERE id = (SELECT turn_id FROM steps WHERE id = ?1) \
             AND outcome IS NULL",
            [step.get()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(turn) = turn {
        let turn = id_from::<TurnId>(turn)?;
        match (pending, blocked) {
            (0, _) => {
                connection.execute(
                    "UPDATE turns SET phase = 'ready', step = NULL, blocked_invocation = NULL \
                     WHERE id = ?1",
                    [turn.get()],
                )?;
            }
            (_, Some(blocked)) => {
                connection.execute(
                    "UPDATE turns SET phase = 'blocked', step = ?2, blocked_invocation = ?3 \
                     WHERE id = ?1",
                    params![turn.get(), step.get(), blocked.get()],
                )?;
            }
            (_, None) => {
                connection.execute(
                    "UPDATE turns SET phase = 'tools', step = ?2, blocked_invocation = NULL \
                     WHERE id = ?1",
                    params![turn.get(), step.get()],
                )?;
            }
        }
    }
    Ok(InvocationProgress {
        entry,
        remaining: pending,
        next,
    })
}
