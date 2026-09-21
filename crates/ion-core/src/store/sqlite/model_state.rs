//! Durable ModelStep/ModelAttempt transitions for the replacement Turn runtime.

use ion_ai::{Content, Role};
use rusqlite::{Connection, OptionalExtension, params};

use super::super::{
    CreatedModelAttempt, CreatedModelStep, DriveBasis, FinishedTurn, RecordedModelAttempt,
    SelectedModelResponse, StoreError,
};
use super::semantic::{
    Sequence, advance_metadata, insert_entry, json_from, json_to, load_entry, load_turn,
};
use crate::{
    AttemptId, CommitReceipt, CommitSeq, Entry, EntryData, EntryId, InputId, ModelAttempt,
    ModelAttemptState, ModelAttemptTiming, ModelStep, RequestManifest, SessionChange,
    SessionUpdate, StepDisposition, StepId, StepPurpose, TranscriptContent, TranscriptMessage,
    TranscriptRole, TurnId, TurnOutcome, TurnPhase, TurnSettings,
};

pub(super) const MAX_DRIVE_ENTRIES: usize = 1024;
pub(super) const MAX_DRIVE_BASIS_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn drive_basis(
    connection: &Connection,
    turn_id: TurnId,
) -> Result<DriveBasis, StoreError> {
    let turn = load_turn(connection, turn_id)?;
    let entries = load_turn_entries(connection, &turn)?;
    let included_inputs = turn_input_ids(connection, turn_id)?;
    let used_providers = turn_provider_bindings(connection, &turn)?;

    let current_step = match &turn.phase {
        TurnPhase::Model(step) | TurnPhase::Tools(step) => Some(load_step(connection, *step)?),
        TurnPhase::Ready
        | TurnPhase::WaitingInteraction(_)
        | TurnPhase::BlockedEffect { .. }
        | TurnPhase::Parked(_) => None,
    };
    let attempts = current_step
        .as_ref()
        .map(|step| load_attempts(connection, step.id))
        .transpose()?
        .unwrap_or_default();

    Ok(DriveBasis {
        turn,
        entries,
        included_inputs,
        used_providers,
        current_step,
        attempts,
    })
}

pub(super) fn create_initial_step(
    connection: &mut Connection,
    turn_id: TurnId,
    manifest: RequestManifest,
) -> Result<CreatedModelStep, StoreError> {
    let transaction = connection.transaction()?;
    let mut turn = load_turn(&transaction, turn_id)?;
    if turn.is_terminal() {
        return Err(StoreError::InvalidState(format!(
            "turn {turn_id} is already terminal"
        )));
    }
    if turn.cancellation.requested {
        return Err(StoreError::Cancelled(turn_id));
    }
    if !matches!(turn.phase, TurnPhase::Ready) {
        return Err(StoreError::InvalidState(format!(
            "turn {turn_id} is not ready for a new model step"
        )));
    }
    if turn.budget.model_steps >= turn.environment.limits.max_model_steps {
        return Err(StoreError::Limit(format!(
            "turn {turn_id} exhausted its model-step limit"
        )));
    }
    validate_manifest_basis(&transaction, &turn, &turn.settings, &manifest)?;

    let mut sequence = Sequence::load(&transaction)?;
    let step_id: StepId = sequence.next()?;
    let commit: CommitSeq = sequence.next()?;
    let ordinal = turn
        .budget
        .model_steps
        .checked_add(1)
        .ok_or_else(|| StoreError::Limit("model-step ordinal overflow".to_owned()))?;
    let step = ModelStep {
        id: step_id,
        turn: turn_id,
        ordinal,
        purpose: StepPurpose::Generate,
        manifest,
        disposition: StepDisposition::Open,
    };

    insert_step(&transaction, &step)?;
    turn.budget.model_steps = ordinal;
    turn.phase = TurnPhase::Model(step_id);
    update_turn_runtime(&transaction, &turn)?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    Ok(CreatedModelStep {
        step: step.clone(),
        turn: turn.clone(),
        receipt: CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(vec![
                SessionChange::ModelStep(step),
                SessionChange::Turn(turn),
            ]),
        },
    })
}

pub(super) fn create_fallback_step(
    connection: &mut Connection,
    predecessor_id: StepId,
    settings: TurnSettings,
    manifest: RequestManifest,
    reason: String,
) -> Result<CreatedModelStep, StoreError> {
    let transaction = connection.transaction()?;
    let mut predecessor = load_step(&transaction, predecessor_id)?;
    if !matches!(predecessor.disposition, StepDisposition::Open) {
        return Err(StoreError::InvalidState(format!(
            "model step {predecessor_id} is no longer open"
        )));
    }

    let mut turn = load_turn(&transaction, predecessor.turn)?;
    if turn.is_terminal() || turn.cancellation.requested {
        return Err(StoreError::Cancelled(turn.id));
    }
    if turn.phase != TurnPhase::Model(predecessor_id) {
        return Err(StoreError::InvalidState(format!(
            "model step {predecessor_id} is not the current turn step"
        )));
    }
    if turn.budget.model_steps >= turn.environment.limits.max_model_steps {
        return Err(StoreError::Limit(format!(
            "turn {} exhausted its model-step limit",
            turn.id
        )));
    }

    let attempts = load_attempts(&transaction, predecessor_id)?;
    if attempts.iter().any(|attempt| {
        !matches!(
            &attempt.state,
            ModelAttemptState::NotStarted { .. } | ModelAttemptState::Failed { .. }
        )
    }) {
        return Err(StoreError::InvalidState(format!(
            "model step {predecessor_id} still has selectable or unresolved attempt evidence"
        )));
    }

    let expected_revision = turn
        .settings
        .revision
        .checked_add(1)
        .ok_or_else(|| StoreError::Limit("turn settings revision overflow".to_owned()))?;
    if settings.revision != expected_revision {
        return Err(StoreError::InvalidState(format!(
            "fallback settings revision {} does not follow current revision {}",
            settings.revision, turn.settings.revision
        )));
    }
    if settings.provider == turn.settings.provider {
        return Err(StoreError::InvalidState(
            "fallback must select a different provider binding".to_owned(),
        ));
    }
    settings.validate(&turn.environment).map_err(|error| {
        StoreError::InvalidState(format!(
            "fallback settings are outside the frozen turn: {error}"
        ))
    })?;
    validate_manifest_basis(&transaction, &turn, &settings, &manifest)?;

    let mut sequence = Sequence::load(&transaction)?;
    let successor_id: StepId = sequence.next()?;
    let commit: CommitSeq = sequence.next()?;
    let ordinal = turn
        .budget
        .model_steps
        .checked_add(1)
        .ok_or_else(|| StoreError::Limit("model-step ordinal overflow".to_owned()))?;
    let successor = ModelStep {
        id: successor_id,
        turn: turn.id,
        ordinal,
        purpose: StepPurpose::Fallback {
            predecessor: predecessor_id,
        },
        manifest,
        disposition: StepDisposition::Open,
    };

    predecessor.disposition = StepDisposition::Superseded {
        reason,
        successor: Some(successor_id),
    };
    update_step_disposition(&transaction, &predecessor)?;
    insert_step(&transaction, &successor)?;

    turn.settings = settings;
    turn.budget.model_steps = ordinal;
    turn.phase = TurnPhase::Model(successor_id);
    update_turn_runtime(&transaction, &turn)?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    Ok(CreatedModelStep {
        step: successor.clone(),
        turn: turn.clone(),
        receipt: CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(vec![
                SessionChange::ModelStep(predecessor),
                SessionChange::ModelStep(successor),
                SessionChange::Turn(turn),
            ]),
        },
    })
}

pub(super) fn commit_attempt_intent(
    connection: &mut Connection,
    step_id: StepId,
    generation: u64,
    timing: ModelAttemptTiming,
) -> Result<CreatedModelAttempt, StoreError> {
    let transaction = connection.transaction()?;
    let step = load_step(&transaction, step_id)?;
    if !matches!(step.disposition, StepDisposition::Open) {
        return Err(StoreError::InvalidState(format!(
            "model step {step_id} is no longer open"
        )));
    }
    let mut turn = load_turn(&transaction, step.turn)?;
    if turn.is_terminal() {
        return Err(StoreError::Cancelled(turn.id));
    }
    if turn.cancellation.requested || turn.cancellation.generation != generation {
        return Err(StoreError::Cancelled(turn.id));
    }
    if turn.phase != TurnPhase::Model(step_id) {
        return Err(StoreError::InvalidState(format!(
            "model step {step_id} is not the current turn step"
        )));
    }

    let previous_attempts: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM model_attempts WHERE step_id = ?1",
        [step_id.get()],
        |row| row.get(0),
    )?;
    let previous_attempts = u32::try_from(previous_attempts).map_err(|_| {
        StoreError::Corrupt(format!("model step {step_id} has an invalid attempt count"))
    })?;
    if previous_attempts >= turn.environment.limits.max_model_attempts_per_step {
        return Err(StoreError::Limit(format!(
            "model step {step_id} exhausted its attempt limit"
        )));
    }
    let ordinal = previous_attempts
        .checked_add(1)
        .ok_or_else(|| StoreError::Limit("model-attempt ordinal overflow".to_owned()))?;

    let mut sequence = Sequence::load(&transaction)?;
    let attempt_id: AttemptId = sequence.next()?;
    let commit: CommitSeq = sequence.next()?;
    let attempt = ModelAttempt {
        id: attempt_id,
        step: step_id,
        ordinal,
        generation,
        timing,
        cost_quote: None,
        state: ModelAttemptState::IntentCommitted {
            start_receipt: None,
        },
    };
    insert_attempt(&transaction, &attempt)?;
    turn.budget.model_attempts = turn
        .budget
        .model_attempts
        .checked_add(1)
        .ok_or_else(|| StoreError::Limit("turn model-attempt budget overflow".to_owned()))?;
    update_turn_runtime(&transaction, &turn)?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    Ok(CreatedModelAttempt {
        attempt: attempt.clone(),
        turn: turn.clone(),
        receipt: CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(vec![
                SessionChange::ModelAttempt(attempt),
                SessionChange::Turn(turn),
            ]),
        },
    })
}

pub(super) fn record_start_receipt(
    connection: &mut Connection,
    attempt_id: AttemptId,
    receipt: crate::ProviderStartReceipt,
) -> Result<RecordedModelAttempt, StoreError> {
    let transaction = connection.transaction()?;
    let mut attempt = load_attempt(&transaction, attempt_id)?;
    let next = attach_start_receipt(&attempt.state, &receipt).map_err(|message| {
        StoreError::InvalidState(format!("model attempt {attempt_id} {message}"))
    })?;
    let Some(state) = next else {
        return Ok(RecordedModelAttempt::Unchanged(attempt));
    };

    attempt.state = state;
    update_attempt_state(&transaction, &attempt)?;
    let mut sequence = Sequence::load(&transaction)?;
    let commit: CommitSeq = sequence.next()?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    let receipt = CommitReceipt {
        seq: commit,
        update: SessionUpdate::new(vec![SessionChange::ModelAttempt(attempt.clone())]),
    };
    Ok(RecordedModelAttempt::Committed { attempt, receipt })
}

pub(super) fn settle_attempt(
    connection: &mut Connection,
    attempt_id: AttemptId,
    state: ModelAttemptState,
) -> Result<RecordedModelAttempt, StoreError> {
    if matches!(state, ModelAttemptState::IntentCommitted { .. }) {
        return Err(StoreError::InvalidRequest(
            "settle_attempt requires terminal evidence".to_owned(),
        ));
    }

    let transaction = connection.transaction()?;
    let mut attempt = load_attempt(&transaction, attempt_id)?;
    if attempt.state == state {
        return Ok(RecordedModelAttempt::Unchanged(attempt));
    }
    if !attempt_state_refines(&attempt.state, &state) {
        return Err(StoreError::InvalidState(format!(
            "model attempt {attempt_id} evidence would move backward or conflict"
        )));
    }

    attempt.state = state;
    update_attempt_state(&transaction, &attempt)?;
    let mut sequence = Sequence::load(&transaction)?;
    let commit: CommitSeq = sequence.next()?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    let receipt = CommitReceipt {
        seq: commit,
        update: SessionUpdate::new(vec![SessionChange::ModelAttempt(attempt.clone())]),
    };
    Ok(RecordedModelAttempt::Committed { attempt, receipt })
}

fn attach_start_receipt(
    state: &ModelAttemptState,
    receipt: &crate::ProviderStartReceipt,
) -> Result<Option<ModelAttemptState>, &'static str> {
    match state {
        ModelAttemptState::IntentCommitted { start_receipt }
        | ModelAttemptState::Indeterminate { start_receipt, .. }
        | ModelAttemptState::Failed { start_receipt, .. }
        | ModelAttemptState::ResponseReady { start_receipt, .. } => {
            if let Some(existing) = start_receipt {
                return if existing == receipt {
                    Ok(None)
                } else {
                    Err("already has a different start receipt")
                };
            }
        }
        ModelAttemptState::NotStarted { .. } => {
            return Err("is known not-started and cannot gain a start receipt");
        }
    }

    let next = match state {
        ModelAttemptState::IntentCommitted { .. } => ModelAttemptState::IntentCommitted {
            start_receipt: Some(receipt.clone()),
        },
        ModelAttemptState::Indeterminate { reason, usage, .. } => {
            ModelAttemptState::Indeterminate {
                reason: reason.clone(),
                usage: *usage,
                start_receipt: Some(receipt.clone()),
            }
        }
        ModelAttemptState::Failed { failure, .. } => ModelAttemptState::Failed {
            failure: failure.clone(),
            start_receipt: Some(receipt.clone()),
        },
        ModelAttemptState::ResponseReady { response, .. } => ModelAttemptState::ResponseReady {
            response: response.clone(),
            start_receipt: Some(receipt.clone()),
        },
        ModelAttemptState::NotStarted { .. } => unreachable!("handled above"),
    };
    Ok(Some(next))
}

fn attempt_state_refines(old: &ModelAttemptState, new: &ModelAttemptState) -> bool {
    match old {
        ModelAttemptState::IntentCommitted { start_receipt } => {
            evidence_preserves_receipt(start_receipt.as_ref(), new)
        }
        ModelAttemptState::Indeterminate {
            usage,
            start_receipt,
            ..
        } => match new {
            ModelAttemptState::NotStarted { .. } => {
                start_receipt.is_none()
                    && usage.input_tokens.is_none()
                    && usage.output_tokens.is_none()
            }
            ModelAttemptState::Failed {
                failure,
                start_receipt: next_receipt,
            } => {
                receipt_refines(start_receipt.as_ref(), next_receipt.as_ref())
                    && usage_refines(*usage, failure.usage)
            }
            ModelAttemptState::Indeterminate {
                usage: next_usage,
                start_receipt: next_receipt,
                ..
            } => {
                receipt_refines(start_receipt.as_ref(), next_receipt.as_ref())
                    && usage_refines(*usage, *next_usage)
            }
            ModelAttemptState::ResponseReady {
                response,
                start_receipt: next_receipt,
            } => {
                receipt_refines(start_receipt.as_ref(), next_receipt.as_ref())
                    && usage_refines(*usage, response.usage)
            }
            ModelAttemptState::IntentCommitted { .. } => false,
        },
        ModelAttemptState::NotStarted { .. }
        | ModelAttemptState::Failed { .. }
        | ModelAttemptState::ResponseReady { .. } => false,
    }
}

fn evidence_preserves_receipt(
    old: Option<&crate::ProviderStartReceipt>,
    new: &ModelAttemptState,
) -> bool {
    match new {
        ModelAttemptState::NotStarted { .. } => old.is_none(),
        ModelAttemptState::Failed { start_receipt, .. }
        | ModelAttemptState::Indeterminate { start_receipt, .. }
        | ModelAttemptState::ResponseReady { start_receipt, .. } => {
            receipt_refines(old, start_receipt.as_ref())
        }
        ModelAttemptState::IntentCommitted { .. } => false,
    }
}

fn receipt_refines(
    old: Option<&crate::ProviderStartReceipt>,
    new: Option<&crate::ProviderStartReceipt>,
) -> bool {
    old.is_none_or(|old| new == Some(old))
}

fn usage_refines(old: ion_ai::Usage, new: ion_ai::Usage) -> bool {
    old.input_tokens
        .is_none_or(|value| new.input_tokens == Some(value))
        && old
            .output_tokens
            .is_none_or(|value| new.output_tokens == Some(value))
}

pub(super) fn select_final_response(
    connection: &mut Connection,
    attempt_id: AttemptId,
) -> Result<SelectedModelResponse, StoreError> {
    let transaction = connection.transaction()?;
    let attempt = load_attempt(&transaction, attempt_id)?;
    let response = match &attempt.state {
        ModelAttemptState::ResponseReady { response, .. } => response,
        _ => {
            return Err(StoreError::InvalidState(format!(
                "model attempt {attempt_id} has no response ready"
            )));
        }
    };
    if !response.is_complete() {
        return Err(StoreError::InvalidState(format!(
            "model attempt {attempt_id} response is incomplete"
        )));
    }
    let projection = final_response_projection(response)?;

    let mut step = load_step(&transaction, attempt.step)?;
    if !matches!(step.disposition, StepDisposition::Open) {
        return Err(StoreError::InvalidState(format!(
            "model step {} is no longer open",
            step.id
        )));
    }
    let mut turn = load_turn(&transaction, step.turn)?;
    if turn.is_terminal()
        || turn.cancellation.requested
        || turn.cancellation.generation != attempt.generation
    {
        return Err(StoreError::Cancelled(turn.id));
    }
    if turn.phase != TurnPhase::Model(step.id) {
        return Err(StoreError::InvalidState(format!(
            "model step {} is not current",
            step.id
        )));
    }

    let mut sequence = Sequence::load(&transaction)?;
    let entry_id: EntryId = sequence.next()?;
    let commit: CommitSeq = sequence.next()?;
    let entry = Entry {
        id: entry_id,
        conversation: turn.conversation,
        data: EntryData::Assistant { step: step.id },
        projection: vec![projection],
    };
    insert_entry(&transaction, &entry, commit)?;
    step.disposition = StepDisposition::Selected(attempt_id);
    update_step_disposition(&transaction, &step)?;
    turn.outcome = Some(TurnOutcome::Completed { entry: entry_id });
    update_turn_runtime(&transaction, &turn)?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    Ok(SelectedModelResponse {
        entry: entry.clone(),
        step: step.clone(),
        turn: turn.clone(),
        receipt: CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(vec![
                SessionChange::Entry(entry),
                SessionChange::ModelStep(step),
                SessionChange::Turn(turn),
            ]),
        },
    })
}

pub(super) fn finish_cancelled_turn(
    connection: &mut Connection,
    turn_id: TurnId,
) -> Result<FinishedTurn, StoreError> {
    let transaction = connection.transaction()?;
    let mut turn = load_turn(&transaction, turn_id)?;
    if turn.is_terminal() {
        return Err(StoreError::InvalidState(format!(
            "turn {turn_id} is already terminal"
        )));
    }
    if !turn.cancellation.requested {
        return Err(StoreError::InvalidState(format!(
            "turn {turn_id} has no durable cancellation request"
        )));
    }

    let mut statement = transaction.prepare(
        "SELECT ma.id
         FROM model_attempts ma
         JOIN model_steps ms ON ms.id = ma.step_id
         WHERE ms.turn_id = ?1
         ORDER BY ma.id",
    )?;
    let rows = statement.query_map([turn_id.get()], |row| row.get::<_, i64>(0))?;
    let mut unresolved_attempts = Vec::new();
    for row in rows {
        let attempt_id = id::<AttemptId>(row?, "cancelled model attempt")?;
        let attempt = load_attempt(&transaction, attempt_id)?;
        if matches!(
            attempt.state,
            ModelAttemptState::IntentCommitted { .. } | ModelAttemptState::Indeterminate { .. }
        ) {
            unresolved_attempts.push(attempt_id);
        }
    }
    drop(statement);

    turn.outcome = Some(TurnOutcome::Cancelled {
        unresolved_attempts,
    });
    update_turn_runtime(&transaction, &turn)?;

    let mut sequence = Sequence::load(&transaction)?;
    let commit: CommitSeq = sequence.next()?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    Ok(FinishedTurn {
        turn: turn.clone(),
        receipt: CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(vec![SessionChange::Turn(turn)]),
        },
    })
}

pub(super) fn load_step(connection: &Connection, step_id: StepId) -> Result<ModelStep, StoreError> {
    let row = connection
        .query_row(
            "SELECT turn_id, ordinal, purpose, manifest, disposition
             FROM model_steps WHERE id = ?1",
            [step_id.get()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound {
            kind: "model step",
            id: step_id.get(),
        })?;
    let ordinal = u32::try_from(row.1).map_err(|_| {
        StoreError::Corrupt(format!(
            "model step {step_id} has invalid ordinal {}",
            row.1
        ))
    })?;
    if ordinal == 0 {
        return Err(StoreError::Corrupt(format!(
            "model step {step_id} has zero ordinal"
        )));
    }
    Ok(ModelStep {
        id: step_id,
        turn: id::<TurnId>(row.0, "model step turn")?,
        ordinal,
        purpose: json_from(&row.2, "model step purpose")?,
        manifest: json_from(&row.3, "model step manifest")?,
        disposition: json_from(&row.4, "model step disposition")?,
    })
}

pub(super) fn load_attempts(
    connection: &Connection,
    step_id: StepId,
) -> Result<Vec<ModelAttempt>, StoreError> {
    let mut statement =
        connection.prepare("SELECT id FROM model_attempts WHERE step_id = ?1 ORDER BY ordinal")?;
    let rows = statement.query_map([step_id.get()], |row| row.get::<_, i64>(0))?;
    let mut attempts = Vec::new();
    for row in rows {
        attempts.push(load_attempt(
            connection,
            id::<AttemptId>(row?, "model attempt")?,
        )?);
    }
    Ok(attempts)
}

pub(super) fn load_attempt(
    connection: &Connection,
    attempt_id: AttemptId,
) -> Result<ModelAttempt, StoreError> {
    let row = connection
        .query_row(
            "SELECT step_id, ordinal, generation, timing, cost_quote, state
             FROM model_attempts WHERE id = ?1",
            [attempt_id.get()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NotFound {
            kind: "model attempt",
            id: attempt_id.get(),
        })?;
    let ordinal = u32::try_from(row.1).map_err(|_| {
        StoreError::Corrupt(format!(
            "model attempt {attempt_id} has invalid ordinal {}",
            row.1
        ))
    })?;
    if ordinal == 0 {
        return Err(StoreError::Corrupt(format!(
            "model attempt {attempt_id} has zero ordinal"
        )));
    }
    let generation = u64::try_from(row.2).map_err(|_| {
        StoreError::Corrupt(format!(
            "model attempt {attempt_id} has negative generation {}",
            row.2
        ))
    })?;
    Ok(ModelAttempt {
        id: attempt_id,
        step: id::<StepId>(row.0, "model attempt step")?,
        ordinal,
        generation,
        timing: json_from(&row.3, "model attempt timing")?,
        cost_quote: row
            .4
            .as_deref()
            .map(|raw| json_from(raw, "model attempt cost quote"))
            .transpose()?,
        state: json_from(&row.5, "model attempt state")?,
    })
}

fn validate_manifest_basis(
    connection: &Connection,
    turn: &crate::Turn,
    settings: &TurnSettings,
    manifest: &RequestManifest,
) -> Result<(), StoreError> {
    let environment_digest = turn.environment.digest().map_err(|error| {
        StoreError::Corrupt(format!(
            "turn {} environment cannot be hashed: {error}",
            turn.id
        ))
    })?;
    if manifest.environment_digest != environment_digest {
        return Err(StoreError::InvalidState(
            "model manifest environment digest does not match the current turn".to_owned(),
        ));
    }
    if manifest.settings != *settings {
        return Err(StoreError::InvalidState(
            "model manifest settings do not match the step settings".to_owned(),
        ));
    }
    if manifest.provider_fingerprint.encoding
        != turn
            .environment
            .provider(&manifest.settings.provider)
            .ok_or_else(|| {
                StoreError::Corrupt(format!(
                    "turn {} settings reference a missing provider",
                    turn.id
                ))
            })?
            .request_encoding
    {
        return Err(StoreError::InvalidState(
            "model manifest provider encoding does not match the frozen binding".to_owned(),
        ));
    }
    let cutoff = latest_entry_id(connection, turn.conversation)?;
    if manifest.cutoff != cutoff {
        return Err(StoreError::InvalidState(
            "model manifest cutoff is stale".to_owned(),
        ));
    }
    let inputs = turn_input_ids(connection, turn.id)?;
    if manifest.included_inputs != inputs {
        return Err(StoreError::InvalidState(
            "model manifest input provenance is stale".to_owned(),
        ));
    }
    Ok(())
}

fn load_turn_entries(
    connection: &Connection,
    turn: &crate::Turn,
) -> Result<Vec<Entry>, StoreError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM entries WHERE conversation_id = ?1",
        [turn.conversation.get()],
        |row| row.get(0),
    )?;
    let count = usize::try_from(count)
        .map_err(|_| StoreError::Corrupt("negative transcript entry count".to_owned()))?;
    if count > MAX_DRIVE_ENTRIES {
        return Err(StoreError::ContextCapacity(format!(
            "turn {} requires {count} transcript entries; bounded drive limit is {MAX_DRIVE_ENTRIES}",
            turn.id
        )));
    }

    let bytes: i64 = connection.query_row(
        "SELECT COALESCE(SUM(length(data) + length(projection)), 0)
         FROM entries WHERE conversation_id = ?1",
        [turn.conversation.get()],
        |row| row.get(0),
    )?;
    let bytes = usize::try_from(bytes)
        .map_err(|_| StoreError::Corrupt("negative transcript byte count".to_owned()))?;
    if bytes > MAX_DRIVE_BASIS_BYTES {
        return Err(StoreError::ContextCapacity(format!(
            "turn {} transcript basis is {bytes} bytes; bounded drive limit is {MAX_DRIVE_BASIS_BYTES}",
            turn.id
        )));
    }

    let mut statement =
        connection.prepare("SELECT id FROM entries WHERE conversation_id = ?1 ORDER BY id")?;
    let rows = statement.query_map([turn.conversation.get()], |row| row.get::<_, i64>(0))?;
    let mut entries = Vec::with_capacity(count);
    for row in rows {
        entries.push(load_entry(
            connection,
            id::<EntryId>(row?, "turn transcript entry")?,
        )?);
    }
    Ok(entries)
}

fn turn_provider_bindings(
    connection: &Connection,
    turn: &crate::Turn,
) -> Result<Vec<crate::ProviderBindingId>, StoreError> {
    let maximum = usize::try_from(turn.environment.limits.max_model_steps)
        .map_err(|_| StoreError::Corrupt("model-step limit does not fit usize".to_owned()))?;
    let sql_limit = i64::try_from(maximum.saturating_add(1))
        .map_err(|_| StoreError::Corrupt("model-step limit does not fit SQLite".to_owned()))?;
    let mut statement = connection.prepare(
        "SELECT manifest FROM model_steps
         WHERE turn_id = ?1
         ORDER BY ordinal
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![turn.id.get(), sql_limit], |row| {
        row.get::<_, String>(0)
    })?;
    let mut providers = Vec::new();
    for row in rows {
        let manifest: RequestManifest = json_from(&row?, "model step manifest")?;
        providers.push(manifest.settings.provider);
    }
    if providers.len() > maximum {
        return Err(StoreError::Corrupt(format!(
            "turn {} exceeds its persisted model-step limit",
            turn.id
        )));
    }
    Ok(providers)
}

fn turn_input_ids(connection: &Connection, turn_id: TurnId) -> Result<Vec<InputId>, StoreError> {
    let mut statement =
        connection.prepare("SELECT id FROM inputs WHERE placed_turn = ?1 ORDER BY id")?;
    let rows = statement.query_map([turn_id.get()], |row| row.get::<_, i64>(0))?;
    let mut inputs = Vec::new();
    for row in rows {
        inputs.push(id::<InputId>(row?, "turn input")?);
    }
    if inputs.len() > crate::MAX_SNAPSHOT_INPUTS {
        return Err(StoreError::ContextCapacity(format!(
            "turn {turn_id} has too many retained input identities"
        )));
    }
    Ok(inputs)
}

fn latest_entry_id(
    connection: &Connection,
    conversation: crate::ConversationId,
) -> Result<Option<EntryId>, StoreError> {
    connection
        .query_row(
            "SELECT MAX(id) FROM entries WHERE conversation_id = ?1",
            [conversation.get()],
            |row| row.get::<_, Option<i64>>(0),
        )?
        .map(|raw| id::<EntryId>(raw, "latest entry"))
        .transpose()
}

fn insert_step(connection: &Connection, step: &ModelStep) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO model_steps (id, turn_id, ordinal, purpose, manifest, disposition)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            step.id.get(),
            step.turn.get(),
            i64::from(step.ordinal),
            json_to(&step.purpose)?,
            json_to(&step.manifest)?,
            json_to(&step.disposition)?,
        ],
    )?;
    Ok(())
}

fn insert_attempt(connection: &Connection, attempt: &ModelAttempt) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO model_attempts
         (id, step_id, ordinal, generation, timing, cost_quote, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            attempt.id.get(),
            attempt.step.get(),
            i64::from(attempt.ordinal),
            i64::try_from(attempt.generation).map_err(|_| StoreError::Limit(
                "attempt generation exceeds SQLite range".to_owned()
            ))?,
            json_to(&attempt.timing)?,
            attempt.cost_quote.as_ref().map(json_to).transpose()?,
            json_to(&attempt.state)?,
        ],
    )?;
    Ok(())
}

fn update_attempt_state(connection: &Connection, attempt: &ModelAttempt) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE model_attempts SET state = ?2 WHERE id = ?1",
        params![attempt.id.get(), json_to(&attempt.state)?],
    )?;
    if updated != 1 {
        return Err(StoreError::Corrupt(format!(
            "model attempt {} disappeared while updating evidence",
            attempt.id
        )));
    }
    Ok(())
}

fn update_step_disposition(connection: &Connection, step: &ModelStep) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE model_steps SET disposition = ?2 WHERE id = ?1",
        params![step.id.get(), json_to(&step.disposition)?],
    )?;
    if updated != 1 {
        return Err(StoreError::Corrupt(format!(
            "model step {} disappeared while updating disposition",
            step.id
        )));
    }
    Ok(())
}

fn update_turn_runtime(connection: &Connection, turn: &crate::Turn) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE turns
         SET settings_revision = ?2, settings = ?3, phase = ?4, budget = ?5, outcome = ?6
         WHERE id = ?1",
        params![
            turn.id.get(),
            i64::from(turn.settings.revision),
            json_to(&turn.settings)?,
            json_to(&turn.phase)?,
            json_to(&turn.budget)?,
            turn.outcome.as_ref().map(json_to).transpose()?,
        ],
    )?;
    if updated != 1 {
        return Err(StoreError::Corrupt(format!(
            "turn {} disappeared while updating runtime state",
            turn.id
        )));
    }
    Ok(())
}

fn final_response_projection(
    response: &ion_ai::ModelResponse,
) -> Result<TranscriptMessage, StoreError> {
    if response.message.role != Role::Assistant {
        return Err(StoreError::InvalidState(
            "model response is not an assistant message".to_owned(),
        ));
    }
    let mut content = Vec::with_capacity(response.message.content.len());
    for item in &response.message.content {
        match item {
            Content::Text(text) => content.push(TranscriptContent::Text(text.clone())),
            Content::ToolCall(_) => {
                return Err(StoreError::ToolsPending);
            }
            Content::ToolResult(_) => {
                return Err(StoreError::InvalidState(
                    "provider returned a tool result in an assistant response".to_owned(),
                ));
            }
        }
    }
    Ok(TranscriptMessage {
        role: TranscriptRole::Assistant,
        content,
        provider_replay: response.message.provider_replay.clone(),
    })
}

fn id<T>(value: i64, label: &str) -> Result<T, StoreError>
where
    T: TryFrom<i64>,
    <T as TryFrom<i64>>::Error: std::fmt::Display,
{
    T::try_from(value)
        .map_err(|error| StoreError::Corrupt(format!("invalid {label} {value}: {error}")))
}
