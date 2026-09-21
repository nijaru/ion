//! Explicit Turn drive and provider effect admission.
//!
//! Opening a Session never reaches this module. Only explicit resume starts
//! reconciliation or new provider work.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use ion_ai::{Content, ModelStreamEvent, ProviderError, ProviderErrorKind, Usage};
use tokio::time::{Instant, timeout, timeout_at};

use crate::session::SessionInner;
use crate::store::{DriveBasis, FinishedTurn, RecordedModelAttempt, StoreError};
use crate::{
    ModelAttempt, ModelAttemptState, ModelAttemptTiming, ModelBoundaries, ModelBoundary,
    ModelStart, ParkReason, ProviderFailureEvidence, ProviderFingerprint, ProviderStartReceipt,
    RequestManifest, SemanticRequest, SessionHealth, StartReceiptCapability, StartReconciliation,
    StepDisposition, TurnId, TurnOutcome, TurnSettings, assemble,
    semantic_request_assembly_revision,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrivePolicy {
    pub model_timing: ModelAttemptTiming,
}

impl Default for DrivePolicy {
    fn default() -> Self {
        Self {
            model_timing: ModelAttemptTiming {
                connect_timeout_ms: 30_000,
                response_timeout_ms: 120_000,
                deferred_poll_timeout_ms: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DriveExit {
    Settled(TurnOutcome),
    Parked(ParkReason),
    Stopped { turn: TurnId },
    Faulted { turn: TurnId, message: String },
}

pub(crate) async fn run(
    inner: Arc<SessionInner>,
    turn_id: TurnId,
    boundaries: ModelBoundaries,
    policy: DrivePolicy,
) -> DriveExit {
    loop {
        if !matches!(inner.health(), SessionHealth::Open) {
            return DriveExit::Stopped { turn: turn_id };
        }

        let basis = match inner.observe_store(inner.store().drive_basis(turn_id).await) {
            Ok(basis) => basis,
            Err(error) => return store_exit(turn_id, error),
        };
        if let Some(outcome) = &basis.turn.outcome {
            return DriveExit::Settled(outcome.clone());
        }
        if basis.turn.cancellation.requested {
            return finish_cancelled(&inner, turn_id).await;
        }

        if !basis.turn.settings.active_tools.is_empty() {
            // R1C supplies the exact frozen tool execution boundary. Until then,
            // advertising a tool without an executable compatible binding would
            // violate the TurnEnvironment contract, so stop before provider I/O.
            return DriveExit::Parked(ParkReason::ToolUnavailable);
        }

        match &basis.current_step {
            None => {
                let prepared = match prepare_initial(&basis, &boundaries) {
                    Ok(prepared) => prepared,
                    Err(exit) => return exit.with_turn(turn_id),
                };
                if let Err(error) = inner.observe_store(
                    inner
                        .store()
                        .create_initial_model_step(turn_id, prepared.manifest)
                        .await,
                ) {
                    return store_exit(turn_id, error);
                }
            }
            Some(step) => {
                if !matches!(step.disposition, StepDisposition::Open) {
                    return DriveExit::Faulted {
                        turn: turn_id,
                        message: format!("current model step {} is not open", step.id),
                    };
                }

                let prepared = match prepare_existing(&basis, &boundaries) {
                    Ok(prepared) => prepared,
                    Err(PrepareExit::Parked(ParkReason::ProviderUnavailable)) => {
                        match commit_fallback(
                            &inner,
                            &basis,
                            &boundaries,
                            "current provider binding is unavailable".to_owned(),
                        )
                        .await
                        {
                            Ok(true) => continue,
                            Ok(false) => {
                                return DriveExit::Parked(ParkReason::ProviderUnavailable);
                            }
                            Err(exit) => return exit,
                        }
                    }
                    Err(exit) => return exit.with_turn(turn_id),
                };

                if let Some(attempt) = basis.attempts.last() {
                    match &attempt.state {
                        ModelAttemptState::IntentCommitted { .. }
                        | ModelAttemptState::Indeterminate { .. } => {
                            match reconcile_intent(&inner, &basis, attempt, &prepared).await {
                                ReconcileAction::Continue => continue,
                                ReconcileAction::Exit(exit) => return exit,
                            }
                        }
                        ModelAttemptState::ResponseReady { response, .. } => {
                            return select_ready(&inner, turn_id, attempt, response).await;
                        }
                        ModelAttemptState::Failed { failure, .. } => {
                            let retry_exhausted = basis.attempts.len()
                                >= basis.turn.environment.limits.max_model_attempts_per_step
                                    as usize;
                            if !retryable_provider_failure(failure.kind) || retry_exhausted {
                                if !provider_failure_permits_fallback(failure.kind) {
                                    return DriveExit::Parked(ParkReason::ProviderUnavailable);
                                }
                                let reason = format!(
                                    "provider {:?} failure on model step {}",
                                    failure.kind, step.id
                                );
                                match commit_fallback(&inner, &basis, &boundaries, reason).await {
                                    Ok(true) => continue,
                                    Ok(false) => {
                                        return DriveExit::Parked(ParkReason::ProviderUnavailable);
                                    }
                                    Err(exit) => return exit,
                                }
                            }
                        }
                        ModelAttemptState::NotStarted { .. } => {
                            if basis.attempts.len()
                                >= basis.turn.environment.limits.max_model_attempts_per_step
                                    as usize
                            {
                                match commit_fallback(
                                    &inner,
                                    &basis,
                                    &boundaries,
                                    format!(
                                        "provider did not start after {} attempts",
                                        basis.attempts.len()
                                    ),
                                )
                                .await
                                {
                                    Ok(true) => continue,
                                    Ok(false) => {
                                        return DriveExit::Parked(ParkReason::ProviderUnavailable);
                                    }
                                    Err(exit) => return exit,
                                }
                            }
                        }
                    }
                }

                match dispatch(&inner, &basis, &prepared, policy.model_timing.clone()).await {
                    DispatchAction::Continue => {}
                    DispatchAction::Exit(exit) => return exit,
                }
            }
        }
    }
}

struct PreparedDrive {
    request: SemanticRequest,
    boundary: Arc<dyn ModelBoundary>,
    manifest: RequestManifest,
    response_limit: u32,
}

enum PrepareExit {
    Parked(ParkReason),
    Faulted(String),
}

impl PrepareExit {
    fn with_turn(self, turn: TurnId) -> DriveExit {
        match self {
            Self::Parked(reason) => DriveExit::Parked(reason),
            Self::Faulted(message) => DriveExit::Faulted { turn, message },
        }
    }
}

fn prepare_initial(
    basis: &DriveBasis,
    boundaries: &ModelBoundaries,
) -> Result<PreparedDrive, PrepareExit> {
    let assembled = assemble(
        &basis.turn.environment,
        &basis.turn.settings,
        &basis.entries,
        basis.entries.last().map(|entry| entry.id),
    )
    .map_err(map_request_error)?;
    let binding = basis
        .turn
        .environment
        .provider(&basis.turn.settings.provider)
        .ok_or(PrepareExit::Parked(ParkReason::ProviderUnavailable))?;
    let boundary = boundaries
        .resolve(binding)
        .map_err(|_| PrepareExit::Parked(ParkReason::ProviderUnavailable))?;
    let provider_digest = boundary.fingerprint(&assembled.request).map_err(|error| {
        PrepareExit::Faulted(format!("provider request preparation failed: {error}"))
    })?;
    let manifest = RequestManifest {
        environment_digest: basis.turn.environment.digest().map_err(|error| {
            PrepareExit::Faulted(format!("turn environment digest failed: {error}"))
        })?,
        settings: basis.turn.settings.clone(),
        context_boundary: None,
        cutoff: basis.entries.last().map(|entry| entry.id),
        included_inputs: basis.included_inputs.clone(),
        assembly: semantic_request_assembly_revision(),
        semantic_digest: assembled.semantic_digest,
        provider_fingerprint: ProviderFingerprint {
            encoding: binding.request_encoding.clone(),
            digest: provider_digest,
        },
    };
    Ok(PreparedDrive {
        request: assembled.request,
        boundary,
        manifest,
        response_limit: basis.turn.environment.limits.max_response_bytes,
    })
}

fn prepare_existing(
    basis: &DriveBasis,
    boundaries: &ModelBoundaries,
) -> Result<PreparedDrive, PrepareExit> {
    let step = basis.current_step.as_ref().ok_or_else(|| {
        PrepareExit::Faulted("current model step disappeared from drive basis".to_owned())
    })?;
    if step.manifest.assembly != semantic_request_assembly_revision() {
        return Err(PrepareExit::Parked(ParkReason::RecoveryRequired));
    }
    let environment_digest = basis.turn.environment.digest().map_err(|error| {
        PrepareExit::Faulted(format!("turn environment digest failed: {error}"))
    })?;
    if step.manifest.environment_digest != environment_digest
        || step.manifest.included_inputs != basis.included_inputs
        || step.manifest.cutoff != basis.entries.last().map(|entry| entry.id)
    {
        return Err(PrepareExit::Parked(ParkReason::RecoveryRequired));
    }

    let assembled = assemble(
        &basis.turn.environment,
        &step.manifest.settings,
        &basis.entries,
        step.manifest.cutoff,
    )
    .map_err(map_request_error)?;
    if assembled.semantic_digest != step.manifest.semantic_digest {
        return Err(PrepareExit::Parked(ParkReason::RecoveryRequired));
    }

    let binding = basis
        .turn
        .environment
        .provider(&step.manifest.settings.provider)
        .ok_or(PrepareExit::Parked(ParkReason::RecoveryRequired))?;
    let boundary = boundaries
        .resolve(binding)
        .map_err(|_| PrepareExit::Parked(ParkReason::ProviderUnavailable))?;
    let provider_digest = boundary
        .fingerprint(&assembled.request)
        .map_err(|_| PrepareExit::Parked(ParkReason::RecoveryRequired))?;
    if step.manifest.provider_fingerprint.encoding != binding.request_encoding
        || step.manifest.provider_fingerprint.digest != provider_digest
    {
        return Err(PrepareExit::Parked(ParkReason::RecoveryRequired));
    }

    Ok(PreparedDrive {
        request: assembled.request,
        boundary,
        manifest: step.manifest.clone(),
        response_limit: basis.turn.environment.limits.max_response_bytes,
    })
}

struct PreparedFallback {
    settings: TurnSettings,
    manifest: RequestManifest,
}

fn prepare_fallback(
    basis: &DriveBasis,
    boundaries: &ModelBoundaries,
) -> Result<Option<PreparedFallback>, PrepareExit> {
    let revision = basis
        .turn
        .settings
        .revision
        .checked_add(1)
        .ok_or_else(|| PrepareExit::Faulted("turn settings revision overflow".to_owned()))?;

    for provider in &basis.turn.environment.fallback_route {
        if basis.used_providers.contains(provider) {
            continue;
        }
        let Some(binding) = basis.turn.environment.provider(provider) else {
            return Err(PrepareExit::Faulted(format!(
                "frozen fallback route references missing provider binding {provider:?}"
            )));
        };
        let Ok(boundary) = boundaries.resolve(binding) else {
            continue;
        };

        let mut settings = basis.turn.settings.clone();
        settings.revision = revision;
        settings.provider = provider.clone();
        if settings.validate(&basis.turn.environment).is_err() {
            continue;
        }

        let assembled = match assemble(
            &basis.turn.environment,
            &settings,
            &basis.entries,
            basis.entries.last().map(|entry| entry.id),
        ) {
            Ok(assembled) => assembled,
            Err(crate::RequestError::TooLarge { .. })
            | Err(crate::RequestError::MissingProvider(_))
            | Err(crate::RequestError::MissingTool(_))
            | Err(crate::RequestError::Unsupported(_))
            | Err(crate::RequestError::Configuration(_)) => continue,
            Err(error) => return Err(map_request_error(error)),
        };
        let provider_digest = match boundary.fingerprint(&assembled.request) {
            Ok(digest) => digest,
            Err(_) => continue,
        };
        let manifest = RequestManifest {
            environment_digest: basis.turn.environment.digest().map_err(|error| {
                PrepareExit::Faulted(format!("turn environment digest failed: {error}"))
            })?,
            settings: settings.clone(),
            context_boundary: basis
                .current_step
                .as_ref()
                .and_then(|step| step.manifest.context_boundary),
            cutoff: basis.entries.last().map(|entry| entry.id),
            included_inputs: basis.included_inputs.clone(),
            assembly: semantic_request_assembly_revision(),
            semantic_digest: assembled.semantic_digest,
            provider_fingerprint: ProviderFingerprint {
                encoding: binding.request_encoding.clone(),
                digest: provider_digest,
            },
        };
        return Ok(Some(PreparedFallback { settings, manifest }));
    }

    Ok(None)
}

async fn commit_fallback(
    inner: &Arc<SessionInner>,
    basis: &DriveBasis,
    boundaries: &ModelBoundaries,
    reason: String,
) -> Result<bool, DriveExit> {
    let Some(predecessor) = basis.current_step.as_ref() else {
        return Ok(false);
    };
    let fallback = match prepare_fallback(basis, boundaries) {
        Ok(Some(fallback)) => fallback,
        Ok(None) => return Ok(false),
        Err(exit) => return Err(exit.with_turn(basis.turn.id)),
    };

    match inner.observe_store(
        inner
            .store()
            .create_fallback_model_step(
                predecessor.id,
                fallback.settings,
                fallback.manifest,
                reason,
            )
            .await,
    ) {
        Ok(_) => Ok(true),
        Err(StoreError::Cancelled(_)) => Err(finish_cancelled(inner, basis.turn.id).await),
        Err(error) => Err(store_exit(basis.turn.id, error)),
    }
}

fn map_request_error(error: crate::RequestError) -> PrepareExit {
    match error {
        crate::RequestError::TooLarge { .. } => PrepareExit::Parked(ParkReason::ContextCapacity),
        crate::RequestError::MissingProvider(_) => {
            PrepareExit::Parked(ParkReason::ProviderUnavailable)
        }
        crate::RequestError::MissingTool(_) => PrepareExit::Parked(ParkReason::ToolUnavailable),
        other => PrepareExit::Faulted(format!("semantic request assembly failed: {other}")),
    }
}

enum ReconcileAction {
    Continue,
    Exit(DriveExit),
}

async fn reconcile_intent(
    inner: &Arc<SessionInner>,
    basis: &DriveBasis,
    attempt: &ModelAttempt,
    prepared: &PreparedDrive,
) -> ReconcileAction {
    let (existing_receipt, existing_usage, was_intent) = match &attempt.state {
        ModelAttemptState::IntentCommitted { start_receipt } => {
            (start_receipt.clone(), Usage::unknown(), true)
        }
        ModelAttemptState::Indeterminate {
            usage,
            start_receipt,
            ..
        } => (start_receipt.clone(), *usage, false),
        _ => return ReconcileAction::Continue,
    };
    let effect_key = effect_key(inner, attempt.step);

    if prepared.boundary.start_receipts() != StartReceiptCapability::Authoritative {
        if was_intent {
            let state = ModelAttemptState::Indeterminate {
                reason:
                    "provider dispatch intent survived without authoritative start reconciliation"
                        .to_owned(),
                usage: existing_usage,
                start_receipt: existing_receipt,
            };
            if let Err(error) = persist_attempt(inner, attempt.id, state).await {
                return ReconcileAction::Exit(store_exit(basis.turn.id, error));
            }
        }
        return ReconcileAction::Exit(DriveExit::Parked(ParkReason::RecoveryRequired));
    }

    let reconciliation = prepared
        .boundary
        .reconcile_start(attempt.id, effect_key)
        .await;
    match reconciliation {
        StartReconciliation::NotStarted { reason } => {
            if let Err(error) =
                persist_attempt(inner, attempt.id, ModelAttemptState::NotStarted { reason }).await
            {
                return ReconcileAction::Exit(store_exit(basis.turn.id, error));
            }
            if basis.turn.cancellation.requested {
                return ReconcileAction::Exit(finish_cancelled(inner, basis.turn.id).await);
            }
            ReconcileAction::Continue
        }
        StartReconciliation::Started(receipt) => {
            if existing_receipt.as_ref() != Some(&receipt)
                && let Err(error) = record_start_receipt(inner, attempt.id, receipt.clone()).await
            {
                return ReconcileAction::Exit(store_exit(basis.turn.id, error));
            }
            let state = ModelAttemptState::Indeterminate {
                reason:
                    "provider start was recovered but no terminal response evidence is available"
                        .to_owned(),
                usage: existing_usage,
                start_receipt: Some(receipt),
            };
            if let Err(error) = persist_attempt(inner, attempt.id, state).await {
                return ReconcileAction::Exit(store_exit(basis.turn.id, error));
            }
            ReconcileAction::Exit(DriveExit::Parked(ParkReason::RecoveryRequired))
        }
        StartReconciliation::Unknown { reason } => {
            let state = ModelAttemptState::Indeterminate {
                reason,
                usage: existing_usage,
                start_receipt: existing_receipt,
            };
            if let Err(error) = persist_attempt(inner, attempt.id, state).await {
                return ReconcileAction::Exit(store_exit(basis.turn.id, error));
            }
            ReconcileAction::Exit(DriveExit::Parked(ParkReason::RecoveryRequired))
        }
    }
}

enum DispatchAction {
    Continue,
    Exit(DriveExit),
}

async fn dispatch(
    inner: &Arc<SessionInner>,
    basis: &DriveBasis,
    prepared: &PreparedDrive,
    timing: ModelAttemptTiming,
) -> DispatchAction {
    let step = basis
        .current_step
        .as_ref()
        .expect("dispatch needs current step");
    let created = match inner.observe_store(
        inner
            .store()
            .commit_model_attempt_intent(
                step.id,
                basis.turn.cancellation.generation,
                timing.clone(),
            )
            .await,
    ) {
        Ok(created) => created,
        Err(StoreError::Cancelled(_)) => {
            return DispatchAction::Exit(finish_cancelled(inner, basis.turn.id).await);
        }
        Err(error) => return DispatchAction::Exit(store_exit(basis.turn.id, error)),
    };

    let gate = inner.effect_gate(basis.turn.id);
    let Some(permit) = gate.admit() else {
        let state = ModelAttemptState::NotStarted {
            reason: "turn effect gate closed before provider start".to_owned(),
        };
        if let Err(error) = persist_attempt(inner, created.attempt.id, state).await {
            return DispatchAction::Exit(store_exit(basis.turn.id, error));
        }
        return DispatchAction::Exit(finish_cancelled(inner, basis.turn.id).await);
    };
    let stop = permit.stop();
    if stop.is_cancelled() {
        let state = ModelAttemptState::NotStarted {
            reason: "turn was stopped before the provider boundary was called".to_owned(),
        };
        if let Err(error) = persist_attempt(inner, created.attempt.id, state).await {
            return DispatchAction::Exit(store_exit(basis.turn.id, error));
        }
        return DispatchAction::Exit(finish_cancelled(inner, basis.turn.id).await);
    }

    let effect_key = effect_key(inner, step.id);
    let start = tokio::select! {
        () = stop.cancelled() => {
            let state = ModelAttemptState::Indeterminate {
                reason: "turn stopped while provider start was in progress".to_owned(),
                usage: Usage::unknown(),
                start_receipt: None,
            };
            if let Err(error) = persist_attempt(inner, created.attempt.id, state).await {
                return DispatchAction::Exit(store_exit(basis.turn.id, error));
            }
            return DispatchAction::Exit(after_local_stop(inner, basis.turn.id).await);
        }
        result = timeout(
            Duration::from_millis(timing.connect_timeout_ms),
            prepared.boundary.start(
                created.attempt.id,
                effect_key,
                prepared.request.clone(),
                stop.clone(),
            ),
        ) => result,
    };

    let started = match start {
        Ok(started) => started,
        Err(_) => {
            let state = ModelAttemptState::Indeterminate {
                reason: "provider start timed out".to_owned(),
                usage: Usage::unknown(),
                start_receipt: None,
            };
            if let Err(error) = persist_attempt(inner, created.attempt.id, state).await {
                return DispatchAction::Exit(store_exit(basis.turn.id, error));
            }
            return DispatchAction::Exit(after_local_stop(inner, basis.turn.id).await);
        }
    };

    match started {
        ModelStart::NotStarted { reason } => {
            if let Err(error) = persist_attempt(
                inner,
                created.attempt.id,
                ModelAttemptState::NotStarted { reason },
            )
            .await
            {
                return DispatchAction::Exit(store_exit(basis.turn.id, error));
            }
            DispatchAction::Continue
        }
        ModelStart::Indeterminate {
            reason,
            usage,
            start_receipt,
        } => {
            if let Some(receipt) = &start_receipt
                && let Err(error) =
                    record_start_receipt(inner, created.attempt.id, receipt.clone()).await
            {
                return DispatchAction::Exit(store_exit(basis.turn.id, error));
            }
            if let Err(error) = persist_attempt(
                inner,
                created.attempt.id,
                ModelAttemptState::Indeterminate {
                    reason,
                    usage,
                    start_receipt,
                },
            )
            .await
            {
                return DispatchAction::Exit(store_exit(basis.turn.id, error));
            }
            DispatchAction::Exit(DriveExit::Parked(ParkReason::RecoveryRequired))
        }
        ModelStart::Started {
            mut stream,
            start_receipt,
        } => {
            if let Some(receipt) = &start_receipt
                && let Err(error) =
                    record_start_receipt(inner, created.attempt.id, receipt.clone()).await
            {
                return DispatchAction::Exit(store_exit(basis.turn.id, error));
            }

            let deadline = Instant::now() + Duration::from_millis(timing.response_timeout_ms);
            let mut usage = Usage::unknown();
            loop {
                let next = tokio::select! {
                    () = stop.cancelled() => {
                        let state = ModelAttemptState::Indeterminate {
                            reason: "turn stopped while provider response was in progress".to_owned(),
                            usage,
                            start_receipt: start_receipt.clone(),
                        };
                        if let Err(error) = persist_attempt(inner, created.attempt.id, state).await {
                            return DispatchAction::Exit(store_exit(basis.turn.id, error));
                        }
                        return DispatchAction::Exit(after_local_stop(inner, basis.turn.id).await);
                    }
                    result = timeout_at(deadline, stream.next()) => result,
                };

                let event = match next {
                    Ok(Some(Ok(event))) => event,
                    Ok(Some(Err(error))) => {
                        let state = provider_stream_error(error, usage, start_receipt.clone());
                        if let Err(error) = persist_attempt(inner, created.attempt.id, state).await
                        {
                            return DispatchAction::Exit(store_exit(basis.turn.id, error));
                        }
                        return DispatchAction::Continue;
                    }
                    Ok(None) => {
                        let state = ModelAttemptState::Indeterminate {
                            reason: "provider stream ended without a terminal response".to_owned(),
                            usage,
                            start_receipt: start_receipt.clone(),
                        };
                        if let Err(error) = persist_attempt(inner, created.attempt.id, state).await
                        {
                            return DispatchAction::Exit(store_exit(basis.turn.id, error));
                        }
                        return DispatchAction::Exit(DriveExit::Parked(
                            ParkReason::RecoveryRequired,
                        ));
                    }
                    Err(_) => {
                        let state = ModelAttemptState::Indeterminate {
                            reason: "provider response timed out".to_owned(),
                            usage,
                            start_receipt: start_receipt.clone(),
                        };
                        if let Err(error) = persist_attempt(inner, created.attempt.id, state).await
                        {
                            return DispatchAction::Exit(store_exit(basis.turn.id, error));
                        }
                        return DispatchAction::Exit(DriveExit::Parked(
                            ParkReason::RecoveryRequired,
                        ));
                    }
                };

                match event {
                    ModelStreamEvent::Usage(value) => usage = value,
                    ModelStreamEvent::TextDelta(_) | ModelStreamEvent::ToolCall(_) => {}
                    ModelStreamEvent::Completed(response) => {
                        let encoded = match serde_json::to_vec(&response) {
                            Ok(encoded) => encoded,
                            Err(error) => {
                                return DispatchAction::Exit(DriveExit::Faulted {
                                    turn: basis.turn.id,
                                    message: format!(
                                        "provider response could not be encoded durably: {error}"
                                    ),
                                });
                            }
                        };
                        if encoded.len() > prepared.response_limit as usize {
                            let failure = ProviderFailureEvidence {
                                kind: ProviderErrorKind::InvalidRequest,
                                message: format!(
                                    "provider response exceeded the {} byte durable limit",
                                    prepared.response_limit
                                ),
                                usage: response.usage,
                                provider_reported_cost_microusd: None,
                            };
                            let state = ModelAttemptState::Failed {
                                failure,
                                start_receipt: start_receipt.clone(),
                            };
                            if let Err(error) =
                                persist_attempt(inner, created.attempt.id, state).await
                            {
                                return DispatchAction::Exit(store_exit(basis.turn.id, error));
                            }
                            return DispatchAction::Exit(DriveExit::Parked(ParkReason::Capacity));
                        }

                        let state = ModelAttemptState::ResponseReady {
                            response: response.clone(),
                            start_receipt: start_receipt.clone(),
                        };
                        if let Err(error) = persist_attempt(inner, created.attempt.id, state).await
                        {
                            return DispatchAction::Exit(store_exit(basis.turn.id, error));
                        }
                        return DispatchAction::Exit(
                            select_ready(inner, basis.turn.id, &created.attempt, &response).await,
                        );
                    }
                }
            }
        }
    }
}

async fn select_ready(
    inner: &Arc<SessionInner>,
    turn_id: TurnId,
    attempt: &ModelAttempt,
    response: &ion_ai::ModelResponse,
) -> DriveExit {
    if !response.is_complete() {
        return DriveExit::Parked(ParkReason::ProviderUnavailable);
    }
    if response
        .message
        .content
        .iter()
        .any(|content| matches!(content, Content::ToolCall(_)))
    {
        return DriveExit::Parked(ParkReason::ToolUnavailable);
    }

    match inner.observe_store(inner.store().select_final_model_response(attempt.id).await) {
        Ok(selected) => match selected.turn.outcome {
            Some(outcome) => DriveExit::Settled(outcome),
            None => DriveExit::Faulted {
                turn: turn_id,
                message: "final response selection did not settle its Turn".to_owned(),
            },
        },
        Err(StoreError::Cancelled(_)) => finish_cancelled(inner, turn_id).await,
        Err(error) => store_exit(turn_id, error),
    }
}

async fn persist_attempt(
    inner: &Arc<SessionInner>,
    attempt: crate::AttemptId,
    state: ModelAttemptState,
) -> Result<(), StoreError> {
    let result = inner.observe_store(inner.store().settle_model_attempt(attempt, state).await)?;
    match result {
        RecordedModelAttempt::Committed { .. } | RecordedModelAttempt::Unchanged(_) => Ok(()),
    }
}

async fn record_start_receipt(
    inner: &Arc<SessionInner>,
    attempt: crate::AttemptId,
    receipt: ProviderStartReceipt,
) -> Result<(), StoreError> {
    let result = inner.observe_store(
        inner
            .store()
            .record_model_start_receipt(attempt, receipt)
            .await,
    )?;
    match result {
        RecordedModelAttempt::Committed { .. } | RecordedModelAttempt::Unchanged(_) => Ok(()),
    }
}

async fn finish_cancelled(inner: &Arc<SessionInner>, turn: TurnId) -> DriveExit {
    match inner.observe_store(inner.store().finish_cancelled_turn(turn).await) {
        Ok(FinishedTurn { turn, .. }) => match turn.outcome {
            Some(outcome) => DriveExit::Settled(outcome),
            None => DriveExit::Stopped { turn: turn.id },
        },
        Err(StoreError::InvalidState(_)) => DriveExit::Stopped { turn },
        Err(error) => store_exit(turn, error),
    }
}

async fn after_local_stop(inner: &Arc<SessionInner>, turn: TurnId) -> DriveExit {
    match inner.observe_store(inner.store().drive_basis(turn).await) {
        Ok(basis) if basis.turn.cancellation.requested => finish_cancelled(inner, turn).await,
        Ok(_) => DriveExit::Stopped { turn },
        Err(error) => store_exit(turn, error),
    }
}

fn provider_stream_error(
    error: ProviderError,
    usage: Usage,
    start_receipt: Option<ProviderStartReceipt>,
) -> ModelAttemptState {
    match error.kind {
        ProviderErrorKind::Transport
        | ProviderErrorKind::Timeout
        | ProviderErrorKind::Cancelled
        | ProviderErrorKind::Unknown => ModelAttemptState::Indeterminate {
            reason: error.to_string(),
            usage,
            start_receipt,
        },
        kind => ModelAttemptState::Failed {
            failure: ProviderFailureEvidence {
                kind,
                message: error.message,
                usage,
                provider_reported_cost_microusd: None,
            },
            start_receipt,
        },
    }
}

fn provider_failure_permits_fallback(kind: ProviderErrorKind) -> bool {
    matches!(
        kind,
        ProviderErrorKind::Authentication
            | ProviderErrorKind::Permission
            | ProviderErrorKind::ContextLength
            | ProviderErrorKind::RateLimited
            | ProviderErrorKind::Quota
            | ProviderErrorKind::Unsupported
            | ProviderErrorKind::Overloaded
            | ProviderErrorKind::Server
    )
}

fn retryable_provider_failure(kind: ProviderErrorKind) -> bool {
    matches!(
        kind,
        ProviderErrorKind::RateLimited | ProviderErrorKind::Overloaded | ProviderErrorKind::Server
    )
}

fn effect_key(inner: &SessionInner, step: crate::StepId) -> String {
    format!("ion:{}:model-step:{step}", inner.session_id())
}

fn store_exit(turn: TurnId, error: StoreError) -> DriveExit {
    match error {
        StoreError::Cancelled(_) => DriveExit::Stopped { turn },
        StoreError::ContextCapacity(_) => DriveExit::Parked(ParkReason::ContextCapacity),
        StoreError::Limit(_) => DriveExit::Parked(ParkReason::Capacity),
        other => DriveExit::Faulted {
            turn,
            message: other.to_string(),
        },
    }
}
