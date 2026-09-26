//! Durable ModelStep/ModelAttempt transitions for the replacement Turn runtime.

use ion_ai::{Content, Role};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeSet;

use super::super::{
    CreatedModelAttempt, CreatedModelStep, DriveBasis, FinishedTurn, RecordedModelAttempt,
    SelectedModelResponse, StoreError,
};
use super::semantic::{
    Sequence, advance_metadata, insert_entry, json_from, json_to, load_entry, load_input, load_turn,
};
use crate::{
    AttemptId, CommitReceipt, CommitSeq, ContextBoundary, ContinuationCheckpoint, CostQuote, Entry,
    EntryData, EntryId, EntryRange, InputBody, InputDisposition, InputId, ModelAttempt,
    ModelAttemptState, ModelAttemptTiming, ModelStep, RequestManifest, SemanticCompatibilityId,
    SessionChange, SessionUpdate, StepDisposition, StepId, StepPurpose, TranscriptContent,
    TranscriptMessage, TranscriptRole, TurnId, TurnOutcome, TurnPhase, TurnSettings,
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
        .map(|step| {
            load_attempts(
                connection,
                step.id,
                turn.environment.limits.max_model_attempts_per_step,
            )
        })
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
    purpose: StepPurpose,
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
    if !matches!(purpose, StepPurpose::Generate | StepPurpose::Compact) {
        return Err(StoreError::InvalidState(
            "initial model step has invalid purpose".into(),
        ));
    }
    if matches!(purpose, StepPurpose::Compact)
        && !turn
            .environment
            .compaction_route
            .contains(&manifest.settings.provider)
    {
        return Err(StoreError::InvalidState(
            "compaction provider is outside the frozen route".into(),
        ));
    }
    let expected_settings = if matches!(purpose, StepPurpose::Compact) {
        crate::request::compaction_settings(&turn.settings)
    } else {
        turn.settings.clone()
    };
    validate_manifest_basis(&transaction, &turn, &expected_settings, &manifest)?;

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
        purpose,
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
    if matches!(predecessor.purpose, StepPurpose::Compact) {
        return Err(StoreError::InvalidState(
            "compaction cannot fall back to a generation step".into(),
        ));
    }
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

    let attempts = load_attempts(
        &transaction,
        predecessor_id,
        turn.environment.limits.max_model_attempts_per_step,
    )?;
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
    cost_quote: Option<CostQuote>,
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

    if turn.environment.limits.max_cost_microusd.is_some() && cost_quote.is_none() {
        return Err(StoreError::CostQuoteUnavailable);
    }
    let amount = match &cost_quote {
        Some(quote) => {
            if quote.revision.is_empty()
                || quote.revision.len() > 160
                || !quote.revision.bytes().all(|byte| byte.is_ascii_graphic())
            {
                return Err(StoreError::InvalidRequest(
                    "invalid host cost quote revision".to_owned(),
                ));
            }
            quote.reserved_microusd
        }
        None => 0,
    };
    let reserved = reserve_cost(
        turn.budget.reserved_cost_microusd,
        amount,
        turn.environment.limits.max_cost_microusd,
    )?;

    let mut sequence = Sequence::load(&transaction)?;
    let attempt_id: AttemptId = sequence.next()?;
    let commit: CommitSeq = sequence.next()?;
    let attempt = ModelAttempt {
        id: attempt_id,
        step: step_id,
        ordinal,
        generation,
        timing,
        cost_quote,
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
    turn.budget.reserved_cost_microusd = reserved;
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

    // NotStarted is the sole terminal evidence that proves no physical provider
    // effect. Release its exact captured quote atomically with that evidence;
    // success, failure, unknown usage and cancellation retain their bounds.
    let released_turn = if matches!(state, ModelAttemptState::NotStarted { .. }) {
        attempt
            .cost_quote
            .as_ref()
            .map(|quote| {
                let step = load_step(&transaction, attempt.step)?;
                let mut turn = load_turn(&transaction, step.turn)?;
                turn.budget.reserved_cost_microusd =
                    release_cost(turn.budget.reserved_cost_microusd, quote.reserved_microusd)?;
                Ok::<_, StoreError>(turn)
            })
            .transpose()?
    } else {
        None
    };
    attempt.state = state;
    update_attempt_state(&transaction, &attempt)?;
    if let Some(turn) = &released_turn {
        update_turn_runtime(&transaction, turn)?;
    }
    let mut sequence = Sequence::load(&transaction)?;
    let commit: CommitSeq = sequence.next()?;
    advance_metadata(&transaction, &sequence, commit, None)?;
    transaction.commit()?;

    let mut changes = vec![SessionChange::ModelAttempt(attempt.clone())];
    if let Some(turn) = released_turn {
        changes.push(SessionChange::Turn(turn));
    }
    let receipt = CommitReceipt {
        seq: commit,
        update: SessionUpdate::new(changes),
    };
    Ok(RecordedModelAttempt::Committed { attempt, receipt })
}

fn reserve_cost(previous: u64, bound: u64, ceiling: Option<u64>) -> Result<u64, StoreError> {
    let reserved = previous
        .checked_add(bound)
        .ok_or(StoreError::MonetaryCapacity)?;
    if ceiling.is_some_and(|maximum| reserved > maximum) {
        return Err(StoreError::MonetaryCapacity);
    }
    Ok(reserved)
}

fn release_cost(previous: u64, bound: u64) -> Result<u64, StoreError> {
    previous
        .checked_sub(bound)
        .ok_or_else(|| StoreError::Corrupt("model cost reservation underflow".to_owned()))
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
    if matches!(step.purpose, StepPurpose::Compact) {
        return Err(StoreError::InvalidState(
            "compaction response cannot settle a user turn".into(),
        ));
    }
    if !step.manifest.settings.permits_tool_response(response) {
        return Err(StoreError::InvalidState(
            "response violates frozen tool-choice controls".into(),
        ));
    }
    if !matches!(step.disposition, StepDisposition::Open) {
        return Err(StoreError::InvalidState(format!(
            "model step {} is no longer open",
            step.id
        )));
    }
    let mut turn = load_turn(&transaction, step.turn)?;
    if !turn
        .environment
        .provider(&step.manifest.settings.provider)
        .is_some_and(|binding| binding.permits_returned_model(response.returned_model.as_deref()))
    {
        return Err(StoreError::InvalidState(
            "returned model is outside the frozen binding".into(),
        ));
    }
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

pub(super) fn select_compaction_response(
    connection: &mut Connection,
    attempt_id: AttemptId,
) -> Result<SelectedModelResponse, StoreError> {
    let transaction = connection.transaction()?;
    let attempt = load_attempt(&transaction, attempt_id)?;
    let response = match &attempt.state {
        ModelAttemptState::ResponseReady { response, .. } if response.is_complete() => response,
        _ => {
            return Err(StoreError::InvalidState(format!(
                "model attempt {attempt_id} has no complete checkpoint response"
            )));
        }
    };
    let mut step = load_step(&transaction, attempt.step)?;
    if !matches!(step.purpose, StepPurpose::Compact)
        || !matches!(step.disposition, StepDisposition::Open)
    {
        return Err(StoreError::InvalidState(
            "model step is not an open compaction".into(),
        ));
    }
    let mut turn = load_turn(&transaction, step.turn)?;
    if turn.is_terminal()
        || turn.cancellation.requested
        || turn.cancellation.generation != attempt.generation
    {
        return Err(StoreError::Cancelled(turn.id));
    }
    if turn.phase != TurnPhase::Model(step.id) {
        return Err(StoreError::InvalidState(
            "compaction step is not current".into(),
        ));
    }
    if !turn
        .environment
        .provider(&step.manifest.settings.provider)
        .is_some_and(|binding| binding.permits_returned_model(response.returned_model.as_deref()))
    {
        return Err(StoreError::InvalidState(
            "compaction returned model is outside the frozen binding".into(),
        ));
    }
    if latest_entry_id(&transaction, turn.conversation)? != step.manifest.cutoff
        || turn_input_ids(&transaction, turn.id)? != step.manifest.included_inputs
    {
        return Err(StoreError::InvalidState(
            "compaction source cutoff or retained inputs changed".into(),
        ));
    }
    let mut text = String::new();
    for content in &response.message.content {
        let Content::Text(piece) = content else {
            return Err(StoreError::InvalidCheckpoint(
                "response contains non-text content".into(),
            ));
        };
        text.push_str(piece);
    }
    let bound = turn.environment.context.max_checkpoint_bytes.min(
        turn.environment
            .context
            .max_request_bytes
            .min(turn.environment.context.max_input_tokens)
            / 4,
    ) as usize;
    if text.is_empty() || text.len() > bound {
        return Err(StoreError::InvalidCheckpoint(format!(
            "checkpoint text must be between 1 and {bound} bytes"
        )));
    }
    let checkpoint: ContinuationCheckpoint = serde_json::from_str(&text)
        .map_err(|error| StoreError::InvalidCheckpoint(error.to_string()))?;
    if !checkpoint.evidence.is_empty() {
        return Err(StoreError::InvalidCheckpoint(
            "checkpoint cites evidence IDs absent from the compactor request".into(),
        ));
    }
    let boundary = ContextBoundary {
        source_cutoff: step.manifest.cutoff,
        retained_inputs: step.manifest.included_inputs.clone(),
        checkpoint,
        raw_tail: select_raw_tail(&transaction, &turn, step.manifest.cutoff)?,
        checkpoint_schema: SemanticCompatibilityId::new("ion-checkpoint-v1")
            .expect("static checkpoint schema is valid"),
        compactor: step.manifest.settings.provider.clone(),
        opaque_provider_artifact: None,
    };
    let mut sequence = Sequence::load(&transaction)?;
    let entry_id: EntryId = sequence.next()?;
    let commit: CommitSeq = sequence.next()?;
    let entry = Entry {
        id: entry_id,
        conversation: turn.conversation,
        data: EntryData::ContextBoundary(Box::new(boundary)),
        projection: Vec::new(),
    };
    insert_entry(&transaction, &entry, commit)?;
    step.disposition = StepDisposition::Selected(attempt_id);
    update_step_disposition(&transaction, &step)?;
    turn.phase = TurnPhase::Ready;
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

fn select_raw_tail(
    connection: &Connection,
    turn: &crate::Turn,
    cutoff: Option<EntryId>,
) -> Result<EntryRange, StoreError> {
    let empty = EntryRange {
        start: None,
        end: None,
    };
    let Some(cutoff) = cutoff else {
        return Ok(empty);
    };
    let previous_boundary: Option<i64> = connection
        .query_row(
            "SELECT id FROM entries WHERE conversation_id = ?1 AND kind = 'context_boundary'
             ORDER BY id DESC LIMIT 1",
            [turn.conversation.get()],
            |row| row.get(0),
        )
        .optional()?;
    let start_after = previous_boundary.unwrap_or(0);
    let mut statement = connection.prepare(
        "SELECT id FROM entries WHERE conversation_id = ?1 AND id > ?2 AND id <= ?3 ORDER BY id",
    )?;
    let rows = statement.query_map(
        params![turn.conversation.get(), start_after, cutoff.get()],
        |row| row.get::<_, i64>(0),
    )?;
    let mut entries = Vec::new();
    for row in rows {
        if entries.len() >= MAX_DRIVE_ENTRIES {
            return Err(StoreError::ContextCapacity(
                "compaction source exceeds bounded entry count".into(),
            ));
        }
        entries.push(load_entry(
            connection,
            id::<EntryId>(row?, "compaction tail source")?,
        )?);
    }
    let provider = turn
        .environment
        .provider(&turn.settings.provider)
        .ok_or_else(|| StoreError::Corrupt("compaction provider binding disappeared".into()))?;
    let request_cap = turn
        .environment
        .context
        .max_request_bytes
        .min(turn.environment.context.max_input_tokens)
        .min(provider.capabilities.max_input_tokens);
    let max_bytes = usize::try_from(turn.environment.context.max_tail_bytes.min(request_cap / 4))
        .map_err(|_| StoreError::Limit("context tail bound overflow".into()))?;
    let mut bytes = 0usize;
    let mut chosen = None;
    for start in (0..entries.len()).rev() {
        bytes = bytes.saturating_add(json_to(&entries[start])?.len());
        if bytes > max_bytes {
            break;
        }
        if complete_exchange_suffix(&entries[start..]) {
            chosen = Some(entries[start].id);
        }
    }
    Ok(match chosen {
        Some(start) => EntryRange {
            start: Some(start),
            end: Some(cutoff),
        },
        None => empty,
    })
}

fn complete_exchange_suffix(entries: &[Entry]) -> bool {
    let mut pending = BTreeSet::new();
    for entry in entries {
        for message in &entry.projection {
            match message.role {
                TranscriptRole::Assistant => {
                    if !pending.is_empty() {
                        return false;
                    }
                    for content in &message.content {
                        if let TranscriptContent::ToolCall { invocation, .. } = content
                            && !pending.insert(*invocation)
                        {
                            return false;
                        }
                    }
                }
                TranscriptRole::Tool => {
                    for content in &message.content {
                        if let TranscriptContent::ToolResult { invocation, .. } = content
                            && !pending.remove(invocation)
                        {
                            return false;
                        }
                    }
                }
                TranscriptRole::User if !pending.is_empty() => return false,
                TranscriptRole::User => {}
            }
        }
    }
    pending.is_empty()
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

    let mut statement = transaction.prepare(
        "SELECT ta.id FROM tool_attempts ta JOIN tool_invocations ti ON ti.id=ta.invocation_id JOIN model_steps ms ON ms.id=ti.step_id WHERE ms.turn_id=?1 AND (json_type(ta.state, '$.IntentCommitted') IS NOT NULL OR json_type(ta.state, '$.Indeterminate') IS NOT NULL) ORDER BY ta.id",
    )?;
    let rows = statement.query_map([turn_id.get()], |row| row.get::<_, i64>(0))?;
    for row in rows {
        unresolved_attempts.push(id::<AttemptId>(row?, "cancelled tool attempt")?);
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
    maximum: u32,
) -> Result<Vec<ModelAttempt>, StoreError> {
    let limit = i64::from(maximum)
        .checked_add(1)
        .ok_or_else(|| StoreError::Corrupt("model-attempt limit overflow".to_owned()))?;
    let mut statement = connection.prepare(
        "SELECT id FROM model_attempts
         WHERE step_id = ?1
         ORDER BY ordinal
         LIMIT ?2",
    )?;
    let rows = statement.query_map(params![step_id.get(), limit], |row| row.get::<_, i64>(0))?;
    let mut attempts = Vec::new();
    for row in rows {
        attempts.push(load_attempt(
            connection,
            id::<AttemptId>(row?, "model attempt")?,
        )?);
    }
    if attempts.len() > maximum as usize {
        return Err(StoreError::Corrupt(format!(
            "model step {step_id} exceeds its frozen attempt limit"
        )));
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
    let boundary: Option<i64> = connection
        .query_row(
            "SELECT id FROM entries WHERE conversation_id = ?1 AND kind = 'context_boundary'
             ORDER BY id DESC LIMIT 1",
            [turn.conversation.get()],
            |row| row.get(0),
        )
        .optional()?;
    if manifest.context_boundary.map(EntryId::get) != boundary {
        return Err(StoreError::InvalidState(
            "model manifest context boundary is stale".into(),
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

pub(super) fn load_turn_entries(
    connection: &Connection,
    turn: &crate::Turn,
) -> Result<Vec<Entry>, StoreError> {
    let boundary_id: Option<i64> = connection
        .query_row(
            "SELECT id FROM entries WHERE conversation_id = ?1 AND kind = 'context_boundary'
             ORDER BY id DESC LIMIT 1",
            [turn.conversation.get()],
            |row| row.get(0),
        )
        .optional()?;
    let start = boundary_id.unwrap_or(0);
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM entries WHERE conversation_id = ?1 AND id >= ?2",
        params![turn.conversation.get(), start],
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
         FROM entries WHERE conversation_id = ?1 AND id >= ?2",
        params![turn.conversation.get(), start],
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

    let mut statement = connection
        .prepare("SELECT id FROM entries WHERE conversation_id = ?1 AND id >= ?2 ORDER BY id")?;
    let rows = statement.query_map(params![turn.conversation.get(), start], |row| {
        row.get::<_, i64>(0)
    })?;
    let mut entries = Vec::with_capacity(count);
    for row in rows {
        let mut entry = load_entry(connection, id::<EntryId>(row?, "turn transcript entry")?)?;
        if Some(entry.id.get()) == boundary_id {
            let EntryData::ContextBoundary(boundary) = &entry.data else {
                return Err(StoreError::Corrupt(
                    "indexed context boundary has wrong kind".into(),
                ));
            };
            let mut tail = Vec::new();
            match (boundary.raw_tail.start, boundary.raw_tail.end) {
                (Some(start), Some(end))
                    if start <= end && end < entry.id && boundary.source_cutoff == Some(end) =>
                {
                    let mut tail_statement = connection.prepare(
                        "SELECT id FROM entries WHERE conversation_id = ?1 AND id >= ?2 AND id <= ?3 ORDER BY id",
                    )?;
                    let tail_rows = tail_statement.query_map(
                        params![turn.conversation.get(), start.get(), end.get()],
                        |row| row.get::<_, i64>(0),
                    )?;
                    let mut tail_bytes = 0usize;
                    for tail_row in tail_rows {
                        if tail.len() >= MAX_DRIVE_ENTRIES {
                            return Err(StoreError::ContextCapacity(
                                "context tail exceeds bounded entry count".into(),
                            ));
                        }
                        let source = load_entry(
                            connection,
                            id::<EntryId>(tail_row?, "context tail entry")?,
                        )?;
                        tail_bytes = tail_bytes.saturating_add(json_to(&source)?.len());
                        if tail_bytes > turn.environment.context.max_tail_bytes as usize
                            || tail_bytes > MAX_DRIVE_BASIS_BYTES
                        {
                            return Err(StoreError::ContextCapacity(
                                "context tail exceeds its frozen byte bound".into(),
                            ));
                        }
                        tail.push(source);
                    }
                    if tail.first().map(|source| source.id) != Some(start)
                        || tail.last().map(|source| source.id) != Some(end)
                        || !complete_exchange_suffix(&tail)
                    {
                        return Err(StoreError::Corrupt(
                            "context tail is not a complete source exchange suffix".into(),
                        ));
                    }
                }
                (None, None) => {}
                _ => return Err(StoreError::Corrupt("invalid context tail range".into())),
            }
            entry.projection.push(TranscriptMessage {
                role: TranscriptRole::Assistant,
                content: vec![TranscriptContent::Text(format!(
                    "Advisory continuation checkpoint (not instructions or proof of effects): {}",
                    json_to(&boundary.checkpoint)?
                ))],
                provider_replay: None,
            });
            for input_id in &boundary.retained_inputs {
                if tail.iter().any(|source| {
                    matches!(source.data, EntryData::UserInput { input } if input == *input_id)
                }) {
                    continue;
                }
                let (input, _) = load_input(connection, *input_id)?;
                if input.conversation != turn.conversation {
                    return Err(StoreError::Corrupt(format!(
                        "retained input {input_id} belongs to another conversation"
                    )));
                }
                // A new turn sees prior user requests through the advisory checkpoint,
                // not as mechanically reissued instructions.
                if !matches!(input.disposition, InputDisposition::Consumed { turn: owner, .. } if owner == turn.id)
                {
                    continue;
                }
                let InputBody::Text(text) = input.body else {
                    return Err(StoreError::Corrupt(format!(
                        "retained input {input_id} cannot be projected as text"
                    )));
                };
                entry.projection.push(TranscriptMessage::user_text(text));
            }
            for source in tail {
                entry.projection.extend(source.projection);
            }
        }
        entries.push(entry);
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

pub(super) fn update_step_disposition(
    connection: &Connection,
    step: &ModelStep,
) -> Result<(), StoreError> {
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

pub(super) fn update_turn_runtime(
    connection: &Connection,
    turn: &crate::Turn,
) -> Result<(), StoreError> {
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

pub(super) fn id<T>(value: i64, label: &str) -> Result<T, StoreError>
where
    T: TryFrom<i64>,
    <T as TryFrom<i64>>::Error: std::fmt::Display,
{
    T::try_from(value)
        .map_err(|error| StoreError::Corrupt(format!("invalid {label} {value}: {error}")))
}

#[cfg(test)]
mod cost_tests {
    use super::*;

    #[test]
    fn exact_cap_and_large_bounds_never_wrap_or_saturate() {
        assert_eq!(reserve_cost(7, 3, Some(10)).unwrap(), 10);
        assert!(matches!(
            reserve_cost(7, 4, Some(10)),
            Err(StoreError::MonetaryCapacity)
        ));
        assert_eq!(
            reserve_cost(u64::MAX - 1, 1, Some(u64::MAX)).unwrap(),
            u64::MAX
        );
        assert!(matches!(
            reserve_cost(u64::MAX, 1, None),
            Err(StoreError::MonetaryCapacity)
        ));
        assert_eq!(reserve_cost(10, 0, Some(10)).unwrap(), 10);
        assert_eq!(release_cost(u64::MAX, u64::MAX).unwrap(), 0);
        assert!(matches!(release_cost(0, 1), Err(StoreError::Corrupt(_))));
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn raw_tail_starts_at_complete_exchange_not_an_orphan_result() {
        let conversation = crate::ConversationId::new(1).unwrap();
        let invocation = crate::InvocationId::new(2).unwrap();
        let call = Entry {
            id: EntryId::new(3).unwrap(),
            conversation,
            data: EntryData::Assistant {
                step: StepId::new(4).unwrap(),
            },
            projection: vec![TranscriptMessage {
                role: TranscriptRole::Assistant,
                content: vec![TranscriptContent::ToolCall {
                    invocation,
                    name: "exec".into(),
                    arguments: serde_json::json!({"command":"make test"}),
                    origin_provider_id: None,
                }],
                provider_replay: None,
            }],
        };
        let result = Entry {
            id: EntryId::new(5).unwrap(),
            conversation,
            data: EntryData::ToolResult { invocation },
            projection: vec![TranscriptMessage {
                role: TranscriptRole::Tool,
                content: vec![TranscriptContent::ToolResult {
                    invocation,
                    name: "exec".into(),
                    result: serde_json::json!({"exit_code":0}),
                }],
                provider_replay: None,
            }],
        };
        assert!(complete_exchange_suffix(&[call.clone(), result.clone()]));
        assert!(!complete_exchange_suffix(&[call]));
        assert!(!complete_exchange_suffix(&[result]));
    }
}
