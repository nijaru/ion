//! Logical exchange settlement is independent of immutable physical run evidence.
use super::{
    model_state::*,
    semantic::{Sequence, advance_metadata, insert_entry, json_from as decode, json_to, load_turn},
};
use crate::store::{StoreError, ToolMutation, ToolMutationResult, ToolRecords};
use crate::*;
use ion_ai::Content;
use rusqlite::{Connection, params};

fn json_from<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, StoreError> {
    decode(raw, "tool record")
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidState(message.into())
}

pub(super) fn records(connection: &Connection, step: StepId) -> Result<ToolRecords, StoreError> {
    let bytes: i64 = connection.query_row("SELECT COALESCE((SELECT SUM(length(prepared_action)+length(approval)+length(exchange_state)) FROM tool_invocations WHERE step_id=?1),0) + COALESCE((SELECT SUM(length(state)+COALESCE(length(progress),0)) FROM tool_attempts WHERE invocation_id IN (SELECT id FROM tool_invocations WHERE step_id=?1)),0)",[step.get()],|row| row.get(0))?;
    if bytes < 0 || bytes as u64 > crate::tool_boundary::MAX_TOOL_BATCH_BYTES as u64 {
        return Err(StoreError::Limit("tool batch evidence capacity".into()));
    }
    let mut stmt = connection.prepare("SELECT id, assistant_entry, source_index, origin_provider_call_id, binding_id, prepared_action, approval, exchange_state FROM tool_invocations WHERE step_id=?1 ORDER BY source_index LIMIT 129")?;
    let rows = stmt.query_map([step.get()], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, u32>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, String>(7)?,
        ))
    })?;
    let mut invocations = Vec::new();
    let mut attempts = Vec::new();
    for row in rows {
        let (
            raw,
            entry,
            source_index,
            origin_provider_call_id,
            binding,
            prepared,
            approval,
            exchange,
        ) = row?;
        if invocations.len() == crate::tool_boundary::MAX_TOOL_BATCH {
            return Err(StoreError::Limit("tool batch count".into()));
        }
        let invocation = ToolInvocation {
            id: id(raw, "invocation")?,
            step,
            assistant_entry: id(entry, "assistant entry")?,
            source_index,
            origin_provider_call_id,
            binding: ToolBindingId::new(binding).map_err(|e| invalid(&e.to_string()))?,
            preparation: json_from(&prepared)?,
            approval: json_from(&approval)?,
            exchange: json_from(&exchange)?,
        };
        let mut stmt = connection.prepare("SELECT id, ordinal, generation, executor, progress, state FROM tool_attempts WHERE invocation_id=?1 ORDER BY ordinal LIMIT 5")?;
        let rows = stmt.query_map([raw], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, u32>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?;
        let mut count = 0;
        for row in rows {
            count += 1;
            if count > crate::tool_boundary::MAX_TOOL_ATTEMPTS {
                return Err(StoreError::Corrupt("tool attempt count".into()));
            }
            let (raw, ordinal, generation, executor, progress, state) = row?;
            attempts.push(ToolAttempt {
                id: id(raw, "tool attempt")?,
                invocation: invocation.id,
                ordinal,
                generation: u64::try_from(generation)
                    .map_err(|_| invalid("negative generation"))?,
                executor: json_from(&executor)?,
                progress: progress.as_deref().map(json_from).transpose()?,
                state: json_from(&state)?,
            });
        }
        invocations.push(invocation);
    }
    let bytes = json_to(&invocations)?
        .len()
        .checked_add(json_to(&attempts)?.len())
        .ok_or_else(|| invalid("tool batch bytes overflow"))?;
    if bytes > crate::tool_boundary::MAX_TOOL_BATCH_BYTES {
        return Err(StoreError::Limit("tool batch evidence capacity".into()));
    }
    Ok(ToolRecords {
        turn: load_turn(connection, load_step(connection, step)?.turn)?,
        invocations,
        attempts,
    })
}

pub(super) fn mutate(
    connection: &mut Connection,
    operation: ToolMutation,
) -> Result<ToolMutationResult, StoreError> {
    let tx = connection.transaction()?;
    let mut seq = Sequence::load(&tx)?;
    let mut changes = Vec::new();
    let mut created = None;
    match operation {
        ToolMutation::Admit {
            attempt,
            preparations,
        } => {
            let model_attempt = load_attempt(&tx, attempt)?;
            let ModelAttemptState::ResponseReady { response, .. } = &model_attempt.state else {
                return Err(invalid("model response not ready"));
            };
            if !response.is_complete() || response.message.role != ion_ai::Role::Assistant {
                return Err(invalid("incomplete assistant response"));
            }
            let mut step = load_step(&tx, model_attempt.step)?;
            let mut turn = load_turn(&tx, step.turn)?;
            eligible(&turn, step.id, model_attempt.generation, false)?;
            if step.disposition != StepDisposition::Open {
                return Err(invalid("model response already selected"));
            }
            let count = response
                .message
                .content
                .iter()
                .filter(|c| matches!(c, Content::ToolCall(_)))
                .count();
            if count == 0
                || count != preparations.len()
                || count > crate::tool_boundary::MAX_TOOL_BATCH
            {
                return Err(invalid("tool preparation count mismatch"));
            }
            let count = u32::try_from(count).map_err(|_| invalid("tool count"))?;
            let total = turn
                .budget
                .tool_invocations
                .checked_add(count)
                .ok_or_else(|| invalid("tool count overflow"))?;
            if total > turn.environment.limits.max_tool_invocations {
                return Err(StoreError::Limit("tool invocation limit".into()));
            }
            // Every admitted call must remain closable even if cancellation
            // wins before execution, or the backend outcome remains unknown.
            for reason in [UNKNOWN_RESULT, CANCELLED_RESULT, UNAVAILABLE_RESULT] {
                validate_result(&error_result(reason), &turn)?;
            }
            let entry_id = seq.next()?;
            let mut content = Vec::new();
            let mut invocations = Vec::new();
            let mut preparations = preparations.into_iter();
            let mut provider_ids = std::collections::HashSet::new();
            for item in &response.message.content {
                match item {
                    Content::Text(text) => content.push(TranscriptContent::Text(text.clone())),
                    Content::ToolCall(call) => {
                        if !provider_ids.insert(&call.id) {
                            return Err(invalid("duplicate provider tool call ID"));
                        }
                        let preparation = preparations
                            .next()
                            .ok_or_else(|| invalid("missing tool preparation"))?;
                        let binding = turn
                            .environment
                            .tools
                            .iter()
                            .find(|b| {
                                b.spec.name == call.name
                                    && turn.settings.active_tools.contains(&b.id)
                            })
                            .ok_or_else(|| invalid("unknown active frozen tool"))?;
                        let exchange = match &preparation {
                            ToolPreparation::Ready(action) => {
                                if action.binding != binding.id || action.egress != binding.egress {
                                    return Err(invalid("prepared binding mismatch"));
                                }
                                crate::tool_boundary::bounded(action)
                                    .map_err(|e| invalid(&e.to_string()))?;
                                ToolExchangeState::Pending
                            }
                            ToolPreparation::Unavailable => ToolExchangeState::OutcomeReady {
                                source: OutcomeSource::Unavailable,
                                result: error_result(UNAVAILABLE_RESULT),
                            },
                        };
                        let invocation_id = seq.next()?;
                        content.push(TranscriptContent::ToolCall {
                            invocation: invocation_id,
                            name: call.name.clone(),
                            arguments: call.arguments.clone(),
                            origin_provider_id: Some(call.id.clone()),
                        });
                        invocations.push(ToolInvocation {
                            id: invocation_id,
                            step: step.id,
                            assistant_entry: entry_id,
                            source_index: u32::try_from(invocations.len())
                                .map_err(|_| invalid("source index"))?,
                            origin_provider_call_id: Some(call.id.clone()),
                            binding: binding.id.clone(),
                            preparation,
                            approval: ApprovalState::NotRequired,
                            exchange,
                        });
                    }
                    _ => return Err(invalid("unexpected provider tool result")),
                }
            }
            reserve_storage(&invocations, turn.environment.limits.max_tool_preview_bytes)?;
            let entry = Entry {
                id: entry_id,
                conversation: turn.conversation,
                data: EntryData::Assistant { step: step.id },
                projection: vec![TranscriptMessage {
                    role: TranscriptRole::Assistant,
                    content,
                    provider_replay: response.message.provider_replay.clone(),
                }],
            };
            // Actual complete batch, including every result envelope and the full
            // per-call reserved preview. No older-history compaction in this baseline.
            let mut closure = load_turn_entries(&tx, &turn)?;
            closure.push(entry.clone());
            let preview = usize::try_from(turn.environment.limits.max_tool_preview_bytes)
                .map_err(|_| invalid("preview cap"))?
                .min(crate::tool_boundary::MAX_TOOL_RECORD_BYTES);
            if preview < 256
                || preview
                    .checked_mul(invocations.len())
                    .is_none_or(|n| n > crate::tool_boundary::MAX_TOOL_BATCH_BYTES / 2)
            {
                return Err(StoreError::Limit("tool batch preview reserve".into()));
            }
            for invocation in &invocations {
                let result = ToolResult {
                    value: serde_json::Value::String("x".repeat(preview)),
                    is_error: true,
                    capture: OutputCapture::CompleteInline,
                };
                closure.push(result_entry(
                    seq.next()?,
                    turn.conversation,
                    invocation,
                    &result,
                    &turn,
                )?);
            }
            let fits = turn.environment.providers.iter().any(|provider| {
                let mut settings = turn.settings.clone();
                settings.provider = provider.id.clone();
                assemble(&turn.environment, &settings, &closure, None).is_ok_and(|r| {
                    r.bytes
                        <= u64::from(
                            provider
                                .capabilities
                                .max_input_tokens
                                .min(turn.environment.context.max_input_tokens),
                        )
                })
            });
            if !fits {
                return Err(StoreError::Limit("tool batch continuation capacity".into()));
            }
            let commit = seq.next()?;
            insert_entry(&tx, &entry, commit)?;
            changes.push(SessionChange::Entry(entry));
            for invocation in invocations {
                tx.execute("INSERT INTO tool_invocations (id,step_id,assistant_entry,source_index,origin_provider_call_id,binding_id,prepared_action,approval,exchange_state) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![invocation.id.get(),invocation.step.get(),invocation.assistant_entry.get(),invocation.source_index,invocation.origin_provider_call_id,invocation.binding.as_str(),json_to(&invocation.preparation)?,json_to(&invocation.approval)?,json_to(&invocation.exchange)?])?;
                changes.push(SessionChange::ToolInvocation(invocation));
            }
            step.disposition = StepDisposition::Selected(attempt);
            update_step_disposition(&tx, &step)?;
            turn.phase = TurnPhase::Tools(step.id);
            turn.budget.tool_invocations = total;
            update_turn_runtime(&tx, &turn)?;
            changes.extend([SessionChange::ModelStep(step), SessionChange::Turn(turn)]);
            advance_metadata(&tx, &seq, commit, None)?;
            tx.commit()?;
            return Ok(ToolMutationResult {
                attempt: None,
                receipt: Some(CommitReceipt {
                    seq: commit,
                    update: SessionUpdate::new(changes),
                }),
            });
        }
        ToolMutation::RequestApproval {
            step,
            invocation,
            now_unix_ms,
        } => {
            let records = records(&tx, step)?;
            let mut call = records
                .invocations
                .into_iter()
                .find(|i| i.id == invocation)
                .ok_or_else(|| invalid("unknown approval invocation"))?;
            eligible(
                &records.turn,
                step,
                records.turn.cancellation.generation,
                true,
            )?;
            if call.exchange != ToolExchangeState::Pending
                || !call
                    .preparation
                    .ready()
                    .is_some_and(|action| action.permitted_by(&records.turn.environment.authority))
            {
                return Err(invalid(
                    "approval cannot widen authority or settled exchange",
                ));
            }
            match &call.approval {
                ApprovalState::NotRequired => {}
                ApprovalState::Approved {
                    expires_at_unix_ms, ..
                } if *expires_at_unix_ms <= now_unix_ms => {}
                _ => return Err(invalid("approval already decided or pending")),
            }
            call.approval = ApprovalState::Pending;
            update_approval(&tx, &call)?;
            self::records(&tx, step)?;
            changes.push(SessionChange::ToolInvocation(call));
        }
        ToolMutation::DecideApproval {
            step,
            invocation,
            action_digest,
            decision,
            executor,
            now_unix_ms,
        } => {
            let records = records(&tx, step)?;
            let mut call = records
                .invocations
                .into_iter()
                .find(|i| i.id == invocation)
                .ok_or_else(|| invalid("unknown approval invocation"))?;
            let binding = records
                .turn
                .environment
                .tool(&call.binding)
                .ok_or_else(|| invalid("missing frozen binding"))?;
            let same_decision = call
                .preparation
                .ready()
                .is_some_and(|action| action.digest == action_digest)
                && executor.as_str() == records.turn.environment.workspace.backend
                && match (&call.approval, &decision) {
                    (
                        ApprovalState::Approved {
                            action_digest: approved_digest,
                            executor: approved_executor,
                            implementation: approved_implementation,
                            workspace: approved_workspace,
                            expires_at_unix_ms: approved_expiry,
                        },
                        ApprovalDecision::Approve { expires_at_unix_ms },
                    ) => {
                        approved_digest == &action_digest
                            && approved_executor == &executor
                            && approved_implementation == &binding.implementation
                            && approved_workspace == &records.turn.environment.workspace
                            && approved_expiry == expires_at_unix_ms
                    }
                    (
                        ApprovalState::Denied { reason: saved },
                        ApprovalDecision::Deny { reason },
                    ) => saved == reason,
                    _ => false,
                };
            if same_decision {
                return Ok(ToolMutationResult {
                    attempt: None,
                    receipt: None,
                });
            }
            eligible(
                &records.turn,
                step,
                records.turn.cancellation.generation,
                true,
            )?;
            if call.exchange != ToolExchangeState::Pending
                || call.approval != ApprovalState::Pending
                || !call
                    .preparation
                    .ready()
                    .is_some_and(|action| action.digest == action_digest)
                || executor.as_str() != records.turn.environment.workspace.backend
                || !call
                    .preparation
                    .ready()
                    .is_some_and(|action| action.permitted_by(&records.turn.environment.authority))
            {
                return Err(invalid("approval does not match pending prepared action"));
            }
            match decision {
                ApprovalDecision::Approve { expires_at_unix_ms } => {
                    if expires_at_unix_ms <= now_unix_ms
                        || expires_at_unix_ms > now_unix_ms.saturating_add(24 * 60 * 60 * 1000)
                    {
                        return Err(invalid("approval expiry must be within 24 hours"));
                    }
                    call.approval = ApprovalState::Approved {
                        action_digest,
                        implementation: binding.implementation.clone(),
                        executor,
                        workspace: records.turn.environment.workspace.clone(),
                        expires_at_unix_ms,
                    };
                }
                ApprovalDecision::Deny { reason } => {
                    if reason.len() > 1024 {
                        return Err(StoreError::Limit("approval denial reason".into()));
                    }
                    call.approval = ApprovalState::Denied { reason };
                    call.exchange = ToolExchangeState::OutcomeReady {
                        source: OutcomeSource::DeniedApproval,
                        result: error_result("User denied this tool action before execution."),
                    };
                }
            }
            crate::tool_boundary::bounded(&call.approval)
                .map_err(|_| StoreError::Limit("approval evidence capacity".into()))?;
            update_approval(&tx, &call)?;
            update_call(&tx, &call)?;
            self::records(&tx, step)?;
            changes.push(SessionChange::ToolInvocation(call));
        }
        ToolMutation::Intent {
            step,
            invocation,
            generation,
            executor,
            approval_required,
            now_unix_ms,
        } => {
            let records = records(&tx, step)?;
            let call = records
                .invocations
                .iter()
                .find(|i| i.id == invocation)
                .ok_or_else(|| invalid("unknown invocation"))?;
            let turn = load_turn(&tx, load_step(&tx, step)?.turn)?;
            eligible(&turn, step, generation, true)?;
            if call.exchange != ToolExchangeState::Pending {
                return Err(invalid("tool outcome already staged"));
            }
            let action = call
                .preparation
                .ready()
                .ok_or_else(|| invalid("unavailable tool cannot admit an attempt"))?;
            if !action.permitted_by(&turn.environment.authority) {
                return Err(invalid("prepared action exceeds frozen authority ceiling"));
            }
            if executor.as_str() != turn.environment.workspace.backend {
                return Err(invalid("executor mismatch"));
            }
            let predecessors: Vec<_> = records
                .attempts
                .into_iter()
                .filter(|a| a.invocation == invocation)
                .collect();
            let binding = turn
                .environment
                .tool(&call.binding)
                .ok_or_else(|| invalid("missing binding"))?;
            if matches!(call.approval, ApprovalState::Denied { .. })
                || (approval_required
                    && !call.approval.permits(
                        action,
                        &binding.implementation,
                        &executor,
                        &turn.environment.workspace,
                        now_unix_ms,
                    ))
            {
                return Err(StoreError::ApprovalRequired);
            }
            if !crate::tool_boundary::permits_retry(binding, &predecessors) {
                return Err(invalid("prior execution does not permit retry"));
            }
            let attempt = ToolAttempt {
                id: seq.next()?,
                invocation,
                ordinal: u32::try_from(predecessors.len() + 1)
                    .map_err(|_| invalid("attempt ordinal"))?,
                generation,
                executor,
                progress: None,
                state: ToolAttemptState::IntentCommitted {
                    start_receipt: None,
                },
            };
            tx.execute("INSERT INTO tool_attempts (id,invocation_id,ordinal,generation,executor,progress,state) VALUES (?1,?2,?3,?4,?5,NULL,?6)",params![attempt.id.get(),invocation.get(),attempt.ordinal,i64::try_from(attempt.generation).map_err(|_| invalid("generation overflow"))?,json_to(&attempt.executor)?,json_to(&attempt.state)?])?;
            changes.push(SessionChange::ToolAttempt(attempt.clone()));
            created = Some(attempt);
        }
        ToolMutation::Evidence {
            step,
            attempt,
            state,
        } => {
            let state = *state;
            crate::tool_boundary::bounded(&state).map_err(|e| invalid(&e.to_string()))?;
            let records = records(&tx, step)?;
            let mut prior = records
                .attempts
                .into_iter()
                .find(|a| a.id == attempt)
                .ok_or_else(|| invalid("unknown tool attempt"))?;
            if !evidence_refines(&prior.state, &state) {
                return Err(invalid("tool evidence cannot be rewritten"));
            }
            if let ToolAttemptState::Settled { result, .. } = &state {
                let turn = load_turn(&tx, load_step(&tx, step)?.turn)?;
                validate_result(result, &turn)?;
            }
            prior.state = state;
            crate::tool_boundary::bounded(&prior)
                .map_err(|_| StoreError::Limit("physical tool attempt capacity".into()))?;
            tx.execute(
                "UPDATE tool_attempts SET state=?2 WHERE id=?1",
                params![attempt.get(), json_to(&prior.state)?],
            )?;
            // Check the prospective committed representation, not a conservative
            // sum that double-counts the prior state during reconciliation.
            self::records(&tx, step)?;
            changes.push(SessionChange::ToolAttempt(prior));
        }
        ToolMutation::Stage {
            step,
            invocation,
            source,
        } => {
            let records = records(&tx, step)?;
            let mut call = records
                .invocations
                .into_iter()
                .find(|i| i.id == invocation)
                .ok_or_else(|| invalid("unknown invocation"))?;
            if call.exchange != ToolExchangeState::Pending {
                return Err(invalid("tool result already selected"));
            }
            let turn = load_turn(&tx, load_step(&tx, step)?.turn)?;
            let result = match source {
                OutcomeSource::Attempt(id) => {
                    // Cancellation may close an already-admitted exchange with
                    // known execution facts, but must never resume continuation.
                    if turn.is_terminal() || turn.phase != TurnPhase::Tools(step) {
                        return Err(StoreError::Cancelled(turn.id));
                    }
                    let attempt = records
                        .attempts
                        .iter()
                        .find(|a| a.id == id && a.invocation == invocation)
                        .ok_or_else(|| invalid("wrong result attempt"))?;
                    if !turn.cancellation.requested
                        && turn.cancellation.generation != attempt.generation
                    {
                        return Err(StoreError::Cancelled(turn.id));
                    }
                    if records
                        .attempts
                        .iter()
                        .any(|a| a.invocation == invocation && a.ordinal > attempt.ordinal)
                    {
                        return Err(invalid("superseded attempt result"));
                    }
                    match &attempt.state {
                        ToolAttemptState::Settled { result, .. } => result.clone(),
                        ToolAttemptState::NotStarted { reason } => error_result(reason),
                        _ => return Err(invalid("attempt has no known result")),
                    }
                }
                OutcomeSource::AcceptedUnknown => {
                    if !records.attempts.iter().any(|a| {
                        a.invocation == invocation
                            && matches!(
                                a.state,
                                ToolAttemptState::Indeterminate { .. }
                                    | ToolAttemptState::IntentCommitted { .. }
                            )
                    }) {
                        return Err(invalid("no uncertain attempt"));
                    }
                    error_result(UNKNOWN_RESULT)
                }
                OutcomeSource::DeniedApproval | OutcomeSource::Unavailable => {
                    return Err(invalid("non-execution outcome is staged at admission"));
                }
                OutcomeSource::CancelledBeforeStart => {
                    if !turn.cancellation.requested
                        || records.attempts.iter().any(|a| {
                            a.invocation == invocation
                                && !matches!(a.state, ToolAttemptState::NotStarted { .. })
                        })
                    {
                        return Err(invalid("execution not proven unstarted"));
                    }
                    error_result(CANCELLED_RESULT)
                }
            };
            validate_result(&result, &turn)?;
            call.exchange = ToolExchangeState::OutcomeReady { source, result };
            update_call(&tx, &call)?;
            self::records(&tx, step)?;
            changes.push(SessionChange::ToolInvocation(call));
        }
        ToolMutation::Materialize { step } => {
            let records = records(&tx, step)?;
            let mut turn = load_turn(&tx, load_step(&tx, step)?.turn)?;
            let commit = seq.next()?;
            let mut complete = true;
            for mut call in records.invocations {
                match &call.exchange {
                    ToolExchangeState::Materialized { .. } => continue,
                    ToolExchangeState::Pending => {
                        complete = false;
                        break;
                    }
                    ToolExchangeState::OutcomeReady { result, source } => {
                        let entry =
                            result_entry(seq.next()?, turn.conversation, &call, result, &turn)?;
                        insert_entry(&tx, &entry, commit)?;
                        call.exchange = ToolExchangeState::Materialized {
                            entry: entry.id,
                            source: *source,
                        };
                        update_call(&tx, &call)?;
                        changes.extend([
                            SessionChange::Entry(entry),
                            SessionChange::ToolInvocation(call),
                        ]);
                    }
                }
            }
            if complete && !turn.is_terminal() && !turn.cancellation.requested {
                turn.phase = TurnPhase::Ready;
                update_turn_runtime(&tx, &turn)?;
                changes.push(SessionChange::Turn(turn));
            }
            advance_metadata(&tx, &seq, commit, None)?;
            tx.commit()?;
            return Ok(ToolMutationResult {
                attempt: None,
                receipt: Some(CommitReceipt {
                    seq: commit,
                    update: SessionUpdate::new(changes),
                }),
            });
        }
    }
    let commit = seq.next()?;
    advance_metadata(&tx, &seq, commit, None)?;
    tx.commit()?;
    Ok(ToolMutationResult {
        attempt: created,
        receipt: Some(CommitReceipt {
            seq: commit,
            update: SessionUpdate::new(changes),
        }),
    })
}

const UNAVAILABLE_RESULT: &str = "Exact tool implementation unavailable for this response.";

fn reserve_storage(invocations: &[ToolInvocation], preview: u32) -> Result<(), StoreError> {
    use crate::tool_boundary::{MAX_TOOL_ATTEMPTS, MAX_TOOL_BATCH_BYTES, MAX_TOOL_RECORD_BYTES};
    // Reserve every retained physical attempt plus one complete staged result.
    // 256 bytes per call covers the exchange/source/EntryId wrapper (IDs are at
    // most 20 decimal digits) and separators in both record arrays. Prepared
    // actions and provider IDs are counted exactly, rather than guessed here.
    let per_call = MAX_TOOL_ATTEMPTS * MAX_TOOL_RECORD_BYTES
        + (preview as usize).min(MAX_TOOL_RECORD_BYTES)
        + 256;
    let base = json_to(&invocations)?.len();
    let reserved = invocations
        .len()
        .checked_mul(per_call)
        .and_then(|extra| base.checked_add(extra));
    if reserved.is_none_or(|bytes| bytes > MAX_TOOL_BATCH_BYTES) {
        return Err(StoreError::Limit(
            "tool batch storage closure reserve".into(),
        ));
    }
    Ok(())
}

fn eligible(turn: &Turn, step: StepId, generation: u64, tools: bool) -> Result<(), StoreError> {
    if turn.is_terminal()
        || turn.cancellation.requested
        || turn.cancellation.generation != generation
    {
        return Err(StoreError::Cancelled(turn.id));
    }
    if turn.phase
        != if tools {
            TurnPhase::Tools(step)
        } else {
            TurnPhase::Model(step)
        }
    {
        return Err(invalid("step no longer current"));
    }
    Ok(())
}
fn update_approval(connection: &Connection, call: &ToolInvocation) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE tool_invocations SET approval=?2 WHERE id=?1",
        params![call.id.get(), json_to(&call.approval)?],
    )?;
    Ok(())
}
fn update_call(connection: &Connection, call: &ToolInvocation) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE tool_invocations SET exchange_state=?2 WHERE id=?1",
        params![call.id.get(), json_to(&call.exchange)?],
    )?;
    Ok(())
}
fn result_entry(
    id: EntryId,
    conversation: ConversationId,
    call: &ToolInvocation,
    result: &ToolResult,
    turn: &Turn,
) -> Result<Entry, StoreError> {
    let name = turn
        .environment
        .tool(&call.binding)
        .ok_or_else(|| invalid("missing tool binding"))?
        .spec
        .name
        .clone();
    Ok(Entry {
        id,
        conversation,
        data: EntryData::ToolResult {
            invocation: call.id,
        },
        projection: vec![TranscriptMessage {
            role: TranscriptRole::Tool,
            content: vec![TranscriptContent::ToolResult {
                invocation: call.id,
                name,
                result: serde_json::to_value(result).map_err(|e| invalid(&e.to_string()))?,
            }],
            provider_replay: None,
        }],
    })
}
fn validate_result(result: &ToolResult, turn: &Turn) -> Result<(), StoreError> {
    // Artifact-bearing results still require Session-bound publication evidence.
    if matches!(&result.capture, OutputCapture::CompleteArtifact { .. }) {
        return Err(StoreError::InvalidRequest(
            "artifact publication is not yet wired".into(),
        ));
    }
    let limit = (turn.environment.limits.max_tool_preview_bytes as usize)
        .min(crate::tool_boundary::MAX_TOOL_RECORD_BYTES / 2);
    if let OutputCapture::Incomplete {
        retained_bytes,
        observed_bytes,
        ..
    } = &result.capture
        && (*retained_bytes > limit as u64
            || observed_bytes.is_some_and(|observed| observed < *retained_bytes))
    {
        return Err(invalid(
            "incomplete output metadata exceeds the retained preview",
        ));
    }
    crate::tool_boundary::bounded_to(result, limit)
        .map_err(|_| StoreError::Limit("tool result preview capacity".into()))
}
const UNKNOWN_RESULT: &str =
    "Execution outcome unknown; effects may have occurred and may still be live.";
const CANCELLED_RESULT: &str = "Cancelled before execution started.";

fn error_result(reason: &str) -> ToolResult {
    ToolResult {
        value: serde_json::Value::String(reason.into()),
        is_error: true,
        capture: OutputCapture::CompleteInline,
    }
}
fn receipt(state: &ToolAttemptState) -> Option<&StartReceipt> {
    match state {
        ToolAttemptState::IntentCommitted { start_receipt } => start_receipt.as_ref(),
        ToolAttemptState::Settled { receipt, .. }
        | ToolAttemptState::Indeterminate { receipt, .. } => receipt.as_ref(),
        ToolAttemptState::NotStarted { .. } => None,
    }
}
fn evidence_refines(old: &ToolAttemptState, new: &ToolAttemptState) -> bool {
    if old == new {
        return true;
    }
    if receipt(old).is_some_and(|r| receipt(new) != Some(r)) {
        return false;
    }
    match old {
        ToolAttemptState::NotStarted { .. } | ToolAttemptState::Settled { .. } => false,
        ToolAttemptState::IntentCommitted { .. } => !matches!(
            new,
            ToolAttemptState::IntentCommitted {
                start_receipt: None
            }
        ),
        ToolAttemptState::Indeterminate { .. } => {
            !matches!(new, ToolAttemptState::IntentCommitted { .. })
        }
    }
}
