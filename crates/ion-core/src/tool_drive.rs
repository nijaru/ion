//! Sequential safe baseline. Storage staging remains source-order independent.
use crate::{
    session::SessionInner,
    store::{DriveBasis, StoreError, ToolMutation},
    *,
};
use ion_ai::Content;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) fn now_unix_ms() -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StoreError::InvalidState("clock before Unix epoch".into()))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| StoreError::InvalidState("clock exceeds Unix millisecond range".into()))
}

pub(crate) fn compatible(basis: &DriveBasis, tools: &ToolBoundaries) -> bool {
    basis.turn.settings.active_tools.iter().all(|id| {
        basis
            .turn
            .environment
            .tool(id)
            .is_some_and(|b| tools.resolve(b, &basis.turn.environment.workspace).is_ok())
    })
}

pub(crate) async fn admit(
    inner: &Arc<SessionInner>,
    basis: &DriveBasis,
    attempt: &ModelAttempt,
    tools: &ToolBoundaries,
) -> Result<(), StoreError> {
    let ModelAttemptState::ResponseReady { response, .. } = &attempt.state else {
        return Err(StoreError::InvalidState("response not ready".into()));
    };
    if response
        .message
        .content
        .iter()
        .filter(|c| matches!(c, Content::ToolCall(_)))
        .count()
        > crate::tool_boundary::MAX_TOOL_BATCH
    {
        return Err(StoreError::Limit("tool batch count".into()));
    }
    let mut preparations = Vec::new();
    for item in &response.message.content {
        if let Content::ToolCall(call) = item {
            let binding = basis
                .turn
                .environment
                .tools
                .iter()
                .find(|b| {
                    b.spec.name == call.name && basis.turn.settings.active_tools.contains(&b.id)
                })
                .ok_or(StoreError::ToolsPending)?;
            let preparation = match tools.resolve(binding, &basis.turn.environment.workspace) {
                Ok(boundary) => match crate::tool_boundary::prepare_action(
                    boundary.as_ref(),
                    binding,
                    call.arguments.clone(),
                ) {
                    Ok(action) => ToolPreparation::Ready(action),
                    Err(ToolBoundaryError::Unavailable | ToolBoundaryError::Incompatible) => {
                        ToolPreparation::Unavailable
                    }
                    Err(error) => return Err(StoreError::InvalidState(error.to_string())),
                },
                Err(_) => ToolPreparation::Unavailable,
            };
            preparations.push(preparation);
        }
    }
    inner.observe_store(
        inner
            .store()
            .tool_mutate(ToolMutation::Admit {
                attempt: attempt.id,
                preparations,
            })
            .await,
    )?;
    Ok(())
}

fn execution(
    inner: &SessionInner,
    turn: &Turn,
    call: &ToolInvocation,
    attempt: &ToolAttempt,
) -> ToolExecution {
    ToolExecution {
        session: inner.session_id(),
        invocation: call.id,
        attempt: attempt.id,
        effect_key: format!("{}:tool:{}", inner.session_id(), call.id),
        binding: turn
            .environment
            .tool(&call.binding)
            .expect("validated frozen invocation")
            .clone(),
        action: call
            .preparation
            .ready()
            .expect("attempt requires a prepared action")
            .clone(),
        workspace: turn.environment.workspace.clone(),
        ceiling: turn.environment.authority.clone(),
        approval: call.approval.clone(),
        output_limit: (turn.environment.limits.max_tool_preview_bytes as usize)
            .min(MAX_TOOL_RECORD_BYTES),
    }
}

pub(crate) async fn reconcile(
    inner: &Arc<SessionInner>,
    step: StepId,
    tools: &ToolBoundaries,
) -> Result<(), StoreError> {
    let records = inner.observe_store(inner.store().tool_records(step).await)?;
    for attempt in &records.attempts {
        if !matches!(
            attempt.state,
            ToolAttemptState::IntentCommitted { .. } | ToolAttemptState::Indeterminate { .. }
        ) {
            continue;
        }
        let call = records
            .invocations
            .iter()
            .find(|c| c.id == attempt.invocation)
            .ok_or_else(|| StoreError::Corrupt("orphan attempt".into()))?;
        let binding = records
            .turn
            .environment
            .tool(&call.binding)
            .ok_or(StoreError::ToolsPending)?;
        let boundary = tools
            .resolve(binding, &records.turn.environment.workspace)
            .map_err(|_| StoreError::ToolsPending)?;
        let state = boundary
            .reconcile(
                execution(inner, &records.turn, call, attempt),
                attempt.clone(),
            )
            .await;
        if matches!(state, ToolAttemptState::NotStarted { .. })
            && binding.start_receipts != StartReceiptCapability::Authoritative
        {
            continue;
        }
        inner.observe_store(
            inner
                .store()
                .tool_mutate(ToolMutation::Evidence {
                    step,
                    attempt: attempt.id,
                    state: Box::new(state),
                })
                .await,
        )?;
    }
    Ok(())
}

pub(crate) async fn drive(
    inner: &Arc<SessionInner>,
    basis: &DriveBasis,
    tools: &ToolBoundaries,
) -> Result<Option<ParkReason>, StoreError> {
    let step = basis
        .current_step
        .as_ref()
        .ok_or_else(|| StoreError::InvalidState("missing tool step".into()))?
        .id;
    let records = inner.observe_store(inner.store().tool_records(step).await)?;
    if basis.turn.cancellation.requested {
        // Close the model exchange without requiring an available backend or
        // confusing cancellation with proof that external execution stopped.
        for call in &records.invocations {
            if !matches!(call.exchange, ToolExchangeState::Pending) {
                continue;
            }
            let last = records
                .attempts
                .iter()
                .rev()
                .find(|a| a.invocation == call.id);
            let source = match last.map(|attempt| &attempt.state) {
                None | Some(ToolAttemptState::NotStarted { .. }) => {
                    OutcomeSource::CancelledBeforeStart
                }
                Some(ToolAttemptState::Settled { .. }) => {
                    OutcomeSource::Attempt(last.expect("matched attempt").id)
                }
                Some(
                    ToolAttemptState::IntentCommitted { .. }
                    | ToolAttemptState::Indeterminate { .. },
                ) => OutcomeSource::AcceptedUnknown,
            };
            inner.observe_store(
                inner
                    .store()
                    .tool_mutate(ToolMutation::Stage {
                        step,
                        invocation: call.id,
                        source,
                    })
                    .await,
            )?;
        }
        inner.observe_store(
            inner
                .store()
                .tool_mutate(ToolMutation::Materialize { step })
                .await,
        )?;
        return Ok(None);
    }
    for call in &records.invocations {
        if !matches!(call.exchange, ToolExchangeState::Pending) {
            continue;
        }
        let binding = basis
            .turn
            .environment
            .tool(&call.binding)
            .ok_or(StoreError::ToolsPending)?;
        let boundary = tools
            .resolve(binding, &basis.turn.environment.workspace)
            .ok();
        let prior: Vec<_> = records
            .attempts
            .iter()
            .filter(|a| a.invocation == call.id)
            .cloned()
            .collect();
        if let Some(attempt) = prior.last() {
            match &attempt.state {
                ToolAttemptState::IntentCommitted { .. }
                | ToolAttemptState::Indeterminate { .. } => {
                    let Some(boundary) = boundary else {
                        return Ok(Some(ParkReason::ToolUnavailable));
                    };
                    let state = boundary
                        .reconcile(
                            execution(inner, &basis.turn, call, attempt),
                            attempt.clone(),
                        )
                        .await;
                    let state = if matches!(state, ToolAttemptState::NotStarted { .. })
                        && binding.start_receipts != StartReceiptCapability::Authoritative
                    {
                        ToolAttemptState::Indeterminate {
                            reason: "non-authoritative absence cannot prove not started".into(),
                            receipt: match &attempt.state {
                                ToolAttemptState::IntentCommitted { start_receipt } => {
                                    start_receipt.clone()
                                }
                                ToolAttemptState::Indeterminate { receipt, .. } => receipt.clone(),
                                _ => None,
                            },
                        }
                    } else {
                        state
                    };
                    let unresolved = matches!(
                        state,
                        ToolAttemptState::IntentCommitted { .. }
                            | ToolAttemptState::Indeterminate { .. }
                    );
                    inner.observe_store(
                        inner
                            .store()
                            .tool_mutate(ToolMutation::Evidence {
                                step,
                                attempt: attempt.id,
                                state: Box::new(state),
                            })
                            .await,
                    )?;
                    if unresolved {
                        return Ok(Some(ParkReason::RecoveryRequired));
                    }
                    return Ok(None);
                }
                ToolAttemptState::Settled { .. } | ToolAttemptState::NotStarted { .. } => {
                    if basis.turn.cancellation.requested {
                        continue;
                    }
                    // Terminal evidence belongs to the Session, not the executor.
                    // Missing code cannot invalidate it or authorize another run.
                    if !boundary.as_ref().is_some_and(|b| b.permits_retry())
                        || !crate::tool_boundary::permits_retry(binding, &prior)
                    {
                        inner.observe_store(
                            inner
                                .store()
                                .tool_mutate(ToolMutation::Stage {
                                    step,
                                    invocation: call.id,
                                    source: OutcomeSource::Attempt(attempt.id),
                                })
                                .await,
                        )?;
                        return Ok(None);
                    }
                }
            }
        }
        if basis.turn.cancellation.requested {
            continue;
        }
        let Some(boundary) = boundary else {
            return Ok(Some(ParkReason::ToolUnavailable));
        };
        let action = call
            .preparation
            .ready()
            .ok_or_else(|| StoreError::Corrupt("unavailable invocation left pending".into()))?;
        if !action.permitted_by(&basis.turn.environment.authority) {
            return Ok(Some(ParkReason::AuthorityDenied));
        }
        let policy = boundary.live_authority(action, &basis.turn.environment.workspace);
        if policy == crate::LiveToolAuthority::Deny {
            return Ok(Some(ParkReason::AuthorityDenied));
        }
        let now = now_unix_ms()?;
        if policy == crate::LiveToolAuthority::Ask {
            let grant_valid = call.approval.permits(
                action,
                &binding.implementation,
                &boundary.executor(),
                &basis.turn.environment.workspace,
                now,
            );
            if !grant_valid {
                if call.approval != ApprovalState::Pending {
                    inner.observe_store(
                        inner
                            .store()
                            .tool_mutate(ToolMutation::RequestApproval {
                                step,
                                invocation: call.id,
                                now_unix_ms: now,
                            })
                            .await,
                    )?;
                }
                return Ok(Some(ParkReason::AwaitingApproval));
            }
        }
        let created = match inner.observe_store(
            inner
                .store()
                .tool_mutate(ToolMutation::Intent {
                    step,
                    invocation: call.id,
                    generation: basis.turn.cancellation.generation,
                    executor: boundary.executor(),
                    approval_required: policy == crate::LiveToolAuthority::Ask,
                    now_unix_ms: now_unix_ms()?,
                })
                .await,
        ) {
            Ok(created) => created,
            Err(StoreError::ApprovalRequired) => return Ok(Some(ParkReason::AwaitingApproval)),
            Err(error) => return Err(error),
        };
        let attempt = created
            .attempt
            .ok_or_else(|| StoreError::Corrupt("intent missing attempt".into()))?;
        let gate = inner.effect_gate(basis.turn.id);
        let permit = gate.admit();
        let state = if let Some(permit) = permit {
            // The backend rechecks live policy and claims, and joins before returning.
            // Keep this permit through that join; cancellation never drops the future.
            let state = boundary
                .execute(execution(inner, &basis.turn, call, &attempt), permit.stop())
                .await;
            drop(permit);
            state
        } else {
            ToolAttemptState::NotStarted {
                reason: "effect gate closed before admission".into(),
            }
        };
        inner.observe_store(
            inner
                .store()
                .tool_mutate(ToolMutation::Evidence {
                    step,
                    attempt: attempt.id,
                    state: Box::new(state),
                })
                .await,
        )?;
        return Ok(None);
    }
    if !basis.turn.cancellation.requested {
        inner.observe_store(
            inner
                .store()
                .tool_mutate(ToolMutation::Materialize { step })
                .await,
        )?;
    }
    Ok(None)
}
