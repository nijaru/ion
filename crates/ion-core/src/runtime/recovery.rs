use super::*;

impl<P: Provider> SessionRuntime<P> {
    /// Ordinary recovery for an operation found open after process loss
    /// (DESIGN.md §32 Step 3, §25.3): pending model steps and
    /// ReplaySafe tool effects replay with a persisted attempt count;
    /// unresolved NeverReplay effects settle as indeterminate and are
    /// never replayed (§12.2); quiescent operations simply continue.
    pub(super) async fn recover_open_operation(&mut self) {
        let suspended = std::mem::take(&mut self.suspended_operations);
        for (operation_id, state_seq, payload, capability_snapshot) in suspended {
            let request = CommitRequest {
                session_id: self.session_id,
                operation_id,
                checkpoint: CheckpointRecord {
                    state_seq: state_seq + 1,
                    payload: crate::store::CheckpointPayload {
                        state: OperationState::Finished(OperationOutcome::Cancelled),
                        cancel_requested: false,
                        prompt: payload.prompt.clone(),
                        capability_snapshot_id: capability_snapshot.id.clone(),
                        open_effect: None,
                    },
                    capability_snapshot,
                },
                entries: Vec::new(),
                open_effects: Vec::new(),
                settled_effects: Vec::new(),
                indeterminate_effects: Vec::new(),
                inbox: Vec::new(),
                inbox_applied: Vec::new(),
                usage: Vec::new(),
                context_manifests: Vec::new(),
                assistant_frames_delete: payload
                    .open_effect
                    .as_ref()
                    .map(|effect| vec![effect.id])
                    .unwrap_or_default(),
                tool_progress_delete: payload
                    .open_effect
                    .as_ref()
                    .map(|effect| vec![effect.id])
                    .unwrap_or_default(),
            };
            if let Err(err) = self.commit_transition(request).await {
                error!(session = %self.session_id, error = %err, "could not settle a suspended operation");
                self.closed = true;
                return;
            }
            info!(session = %self.session_id, operation = %operation_id, "settled a reopened suspended operation as cancelled");
        }
        let Some(state) = self
            .operation
            .as_ref()
            .map(|active| active.machine.state().clone())
        else {
            return;
        };
        match state {
            OperationState::AssistantEffectPending => {
                let Some(open) = self.operation.as_ref().and_then(|a| a.open_effect.clone()) else {
                    error!(session = %self.session_id, "pending model step without an effect intent; fencing");
                    self.closed = true;
                    return;
                };
                let Some((
                    step,
                    model,
                    plan,
                    persisted_snapshot_id,
                    persisted_manifest_id,
                    persisted_prefix_fingerprint,
                    persisted_cache_expectation,
                )) = model_step_from_input(&open.effective_input)
                else {
                    error!(session = %self.session_id, "pending model step lacks an exact model snapshot; fencing");
                    self.closed = true;
                    return;
                };
                let mut staged = self.operation.clone().expect("operation present");
                let snapshot_id = staged.capability_snapshot.id.clone();
                if snapshot_id != persisted_snapshot_id
                    || persisted_manifest_id.is_empty()
                    || persisted_prefix_fingerprint.is_empty()
                    || persisted_cache_expectation.is_empty()
                {
                    error!(session = %self.session_id, "pending model step capability snapshot disagrees with checkpoint; fencing");
                    self.closed = true;
                    return;
                }
                let applied = staged
                    .machine
                    .apply(Transition::RecoverModelStep {
                        model: model.clone(),
                        plan: plan.clone(),
                    })
                    .expect("recover a pending model step");
                let EffectIntent::ModelStep { tools, .. } = applied.intents[0].clone() else {
                    panic!("RecoverModelStep must yield a model-step intent");
                };
                // Replace the old pending model effect and its auxiliary frame together.
                let settled = vec![SettledEffect {
                    id: open.id,
                    settlement: serde_json::json!({ "recovered": "process_loss" }),
                }];
                let effect = open.next_attempt();
                staged.open_effect = Some(effect.clone());
                let (mut request, new_entry_seq) = build_commit_request(
                    self.session_id,
                    &staged,
                    staged.state_seq + 1,
                    self.next_entry_seq,
                    Vec::new(),
                    vec![effect.clone()],
                    settled,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                );
                request.assistant_frames_delete.push(open.id);
                if let Err(err) = self.commit_transition(request).await {
                    self.fail_operation_on_persistence(err).await;
                    return;
                }
                let operation_id = staged.machine.operation_id();
                self.next_entry_seq = new_entry_seq;
                staged.state_seq += 1;
                staged.open_effect = Some(effect);
                self.operation = Some(staged);
                self.operation_live.model_step = step.saturating_sub(1);
                self.operation_live.last_prefix_fingerprint = Some(persisted_prefix_fingerprint);
                self.operation_live.draft_text.clear();
                self.operation_live.draft_thinking.clear();
                self.operation_live.assistant_frame_seq = 0;
                warn!(%operation_id, model = %model.model_ref, "recovered a pending model step by replay");
                self.spawn_model_step(operation_id, model, plan, tools);
            }
            OperationState::CompactionPending => {
                let Some(open) = self.operation.as_ref().and_then(|a| a.open_effect.clone()) else {
                    error!(session = %self.session_id, "pending compaction without an effect intent; fencing");
                    self.closed = true;
                    return;
                };
                let Some((step, model, plan)) = compaction_from_input(&open.effective_input) else {
                    error!(session = %self.session_id, "pending compaction lacks an exact model snapshot; fencing");
                    self.closed = true;
                    return;
                };
                let mut staged = self.operation.clone().expect("operation present");
                staged
                    .machine
                    .apply(Transition::RecoverCompaction { plan: plan.clone() })
                    .expect("recover a pending compaction step");
                let settled = vec![SettledEffect {
                    id: open.id,
                    settlement: serde_json::json!({ "recovered": "process_loss" }),
                }];
                let effect = open.next_attempt();
                staged.open_effect = Some(effect.clone());
                let (request, new_entry_seq) = build_commit_request(
                    self.session_id,
                    &staged,
                    staged.state_seq + 1,
                    self.next_entry_seq,
                    Vec::new(),
                    vec![effect.clone()],
                    settled,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                );
                if let Err(err) = self.commit_transition(request).await {
                    self.fail_operation_on_persistence(err).await;
                    return;
                }
                let operation_id = staged.machine.operation_id();
                self.next_entry_seq = new_entry_seq;
                staged.state_seq += 1;
                staged.open_effect = Some(effect);
                self.operation = Some(staged);
                self.operation_live.model_step = step.saturating_sub(1);
                warn!(%operation_id, model = %model.model_ref, "recovered a pending compaction step by replay");
                self.spawn_model_step(operation_id, model, plan, Vec::new());
            }
            OperationState::ToolEffectPending { .. } => {
                let Some(open) = self.operation.as_ref().and_then(|a| a.open_effect.clone()) else {
                    error!(session = %self.session_id, "pending tool effect without an effect intent; fencing");
                    self.closed = true;
                    return;
                };
                match open.recovery_class {
                    RecoveryClass::ReplaySafe => {
                        let operation_id = self
                            .operation
                            .as_ref()
                            .expect("operation present")
                            .machine
                            .operation_id();
                        let Some((call, _invocation)) =
                            tool_call_from_input(operation_id, &open.effective_input)
                        else {
                            error!(session = %self.session_id, effect = %open.id, "pending replay-safe tool has an invalid durable invocation; fencing");
                            self.closed = true;
                            return;
                        };
                        let mut staged = self.operation.clone().expect("operation present");
                        staged
                            .machine
                            .apply(Transition::RecoverTool { call: call.clone() })
                            .expect("recover a pending replay-safe tool effect");
                        let settled = vec![SettledEffect {
                            id: open.id,
                            settlement: serde_json::json!({ "recovered": "process_loss" }),
                        }];
                        let effect = open.next_attempt();
                        staged.open_effect = Some(effect.clone());
                        let (mut request, new_entry_seq) = build_commit_request(
                            self.session_id,
                            &staged,
                            staged.state_seq + 1,
                            self.next_entry_seq,
                            Vec::new(),
                            vec![effect.clone()],
                            settled,
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        );
                        request.tool_progress_delete.push(open.id);
                        if let Err(err) = self.commit_transition(request).await {
                            self.fail_operation_on_persistence(err).await;
                            return;
                        }
                        let operation_id = staged.machine.operation_id();
                        self.next_entry_seq = new_entry_seq;
                        staged.state_seq += 1;
                        let effect_id = effect.id;
                        staged.open_effect = Some(effect);
                        self.operation = Some(staged);
                        let tools = self.tools.snapshot();
                        self.emit_tool_started(
                            operation_id,
                            call.call_id,
                            &call.name,
                            target_summary_registry(&tools, &call.name, &call.arguments),
                        );
                        warn!(%operation_id, tool = %call.name, attempt = open.attempt + 1, "recovered a pending replay-safe tool by re-execution");
                        self.spawn_tool_effect(Some(effect_id), call, None, tools);
                    }
                    RecoveryClass::NeverReplay => {
                        // Side effects cannot be classified (§12.4); an
                        // unresolved effect of this class is
                        // indeterminate, never replayed.
                        let mut staged = self.operation.clone().expect("operation present");
                        let applied = staged
                            .machine
                            .apply(Transition::SettleIndeterminate)
                            .expect("settle an unresolved effect as indeterminate");
                        // The indeterminate status IS the settlement; the
                        // effect must not also be marked settled.
                        let settled = Vec::new();
                        let indeterminate = vec![open.id];
                        let (request, new_entry_seq) = build_commit_request(
                            self.session_id,
                            &staged,
                            staged.state_seq + 1,
                            self.next_entry_seq,
                            applied.entries.clone(),
                            Vec::new(),
                            settled,
                            indeterminate,
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                        );
                        if let Err(err) = self.commit_transition(request).await {
                            self.fail_operation_on_persistence(err).await;
                            return;
                        }
                        let operation_id = staged.machine.operation_id();
                        self.next_entry_seq = new_entry_seq;
                        staged.state_seq += 1;
                        staged.open_effect = None;
                        self.emit_terminal_state(&applied.state);
                        self.operation = Some(staged);
                        warn!(%operation_id, "an unresolved never-replay effect settled as indeterminate");
                        self.advance().await;
                    }
                    RecoveryClass::Reconcile => {
                        let operation_id = self
                            .operation
                            .as_ref()
                            .expect("operation present")
                            .machine
                            .operation_id();
                        let Some((call, invocation)) =
                            tool_call_from_input(operation_id, &open.effective_input)
                        else {
                            error!(session = %self.session_id, effect = %open.id, "pending reconcilable tool has an invalid durable invocation; fencing");
                            self.closed = true;
                            return;
                        };
                        let evidence = invocation
                            .reconciliation
                            .clone()
                            .unwrap_or(serde_json::Value::Null);
                        let verdict = match &evidence {
                            serde_json::Value::Null => crate::tool::ReconcileVerdict::Unknown,
                            evidence => match evidence.get("path").and_then(|v| v.as_str()) {
                                Some(path) => match crate::tool::file_snapshot(
                                    self.tools.cwd(),
                                    std::path::Path::new(path),
                                    true,
                                )
                                .await
                                {
                                    Ok(current) => crate::tool::classify_reconciliation_snapshot(
                                        evidence,
                                        current.as_ref(),
                                    ),
                                    Err(err) => {
                                        warn!(
                                            "cannot inspect reconciliation target during recovery: {err}"
                                        );
                                        crate::tool::ReconcileVerdict::Conflict
                                    }
                                },
                                None => crate::tool::ReconcileVerdict::Unknown,
                            },
                        };
                        match verdict {
                            crate::tool::ReconcileVerdict::SafeToExecute => {
                                let mut staged = self.operation.clone().expect("operation present");
                                staged
                                    .machine
                                    .apply(Transition::RecoverTool { call: call.clone() })
                                    .expect("recover a pending reconcilable tool effect");
                                let settled = vec![SettledEffect {
                                    id: open.id,
                                    settlement: serde_json::json!({
                                        "recovered": "reconciled_preimage_match",
                                    }),
                                }];
                                let effect = open.next_attempt();
                                let effect_id = effect.id;
                                staged.open_effect = Some(effect.clone());
                                let (mut request, new_entry_seq) = build_commit_request(
                                    self.session_id,
                                    &staged,
                                    staged.state_seq + 1,
                                    self.next_entry_seq,
                                    Vec::new(),
                                    vec![effect],
                                    settled,
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                );
                                request.tool_progress_delete.push(open.id);
                                if let Err(err) = self.commit_transition(request).await {
                                    self.fail_operation_on_persistence(err).await;
                                    return;
                                }
                                let operation_id = staged.machine.operation_id();
                                self.next_entry_seq = new_entry_seq;
                                staged.state_seq += 1;
                                self.operation = Some(staged);
                                let tools = self.tools.snapshot();
                                self.emit_tool_started(
                                    operation_id,
                                    call.call_id,
                                    &call.name,
                                    target_summary_registry(&tools, &call.name, &call.arguments),
                                );
                                self.spawn_tool_effect(
                                    Some(effect_id),
                                    call,
                                    Some(evidence.clone()),
                                    tools,
                                );
                                info!(%operation_id, "reconciled a pending file mutation by preimage match");
                            }
                            crate::tool::ReconcileVerdict::AlreadyApplied => {
                                let mut staged = self.operation.clone().expect("operation present");
                                let applied = staged
                                    .machine
                                    .apply(Transition::ToolSettled {
                                        result: ToolResult::Ok {
                                            call_id: invocation.call_id,
                                            output: "recovered: already applied".to_owned(),
                                            artifact: None,
                                        },
                                    })
                                    .expect("settle an already-applied reconcilable effect");
                                let settled = vec![SettledEffect {
                                    id: open.id,
                                    settlement: serde_json::json!({
                                        "recovered": "reconciled_postimage_match",
                                    }),
                                }];
                                let (mut request, new_entry_seq) = build_commit_request(
                                    self.session_id,
                                    &staged,
                                    staged.state_seq + 1,
                                    self.next_entry_seq,
                                    applied.entries.clone(),
                                    Vec::new(),
                                    settled,
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                );
                                request.tool_progress_delete.push(open.id);
                                if let Err(err) = self.commit_transition(request).await {
                                    self.fail_operation_on_persistence(err).await;
                                    return;
                                }
                                let operation_id = staged.machine.operation_id();
                                self.next_entry_seq = new_entry_seq;
                                staged.state_seq += 1;
                                staged.open_effect = None;
                                self.operation = Some(staged);
                                info!(%operation_id, "reconciled a pending file mutation as already applied");
                                self.advance().await;
                            }
                            crate::tool::ReconcileVerdict::Conflict
                            | crate::tool::ReconcileVerdict::Unknown => {
                                let mut staged = self.operation.clone().expect("operation present");
                                let applied = staged
                                    .machine
                                    .apply(Transition::SettleIndeterminate)
                                    .expect("settle an unresolved effect as indeterminate");
                                let indeterminate = vec![open.id];
                                let (mut request, new_entry_seq) = build_commit_request(
                                    self.session_id,
                                    &staged,
                                    staged.state_seq + 1,
                                    self.next_entry_seq,
                                    applied.entries.clone(),
                                    Vec::new(),
                                    Vec::new(),
                                    indeterminate,
                                    Vec::new(),
                                    Vec::new(),
                                    Vec::new(),
                                );
                                request.tool_progress_delete.push(open.id);
                                if let Err(err) = self.commit_transition(request).await {
                                    self.fail_operation_on_persistence(err).await;
                                    return;
                                }
                                let operation_id = staged.machine.operation_id();
                                self.next_entry_seq = new_entry_seq;
                                staged.state_seq += 1;
                                staged.open_effect = None;
                                self.emit_terminal_state(&applied.state);
                                self.operation = Some(staged);
                                warn!(%operation_id, "a pending file mutation settled as indeterminate (conflict)");
                                self.advance().await;
                            }
                        }
                    }
                }
            }
            OperationState::ApprovalPending { .. } => {
                // The staged call is durable in the state and nothing may
                // execute before the decision. An interactive host
                // re-surfaces the parked decision; a host that cannot
                // grant approvals terminates it (DESIGN.md §17.2, §17.4).
                let call = self
                    .operation
                    .as_ref()
                    .and_then(|active| active.machine.state().staged_call())
                    .cloned()
                    .expect("parked approval carries its staged call");
                if self.interactive_approvals {
                    let tools = self.tools.snapshot();
                    let target = target_summary_registry(&tools, &call.name, &call.arguments);
                    self.emit(RuntimeEvent::ApprovalPending {
                        cursor: RuntimeCursor::default(),
                        operation_id: call.operation_id,
                        tool: call.name.clone(),
                        target,
                    });
                    info!(operation = %call.operation_id, tool = %call.name, "re-surfaced a parked approval after process loss");
                } else {
                    let mut staged = self.operation.clone().expect("parked approval present");
                    let applied = staged
                        .machine
                        .apply(Transition::ApprovalRequired {
                            tool: call.name.clone(),
                        })
                        .expect("terminate a parked approval in non-interactive mode");
                    let (request, new_entry_seq) = build_commit_request(
                        self.session_id,
                        &staged,
                        staged.state_seq + 1,
                        self.next_entry_seq,
                        applied.entries.clone(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                    );
                    if let Err(err) = self.commit_transition(request).await {
                        self.fail_operation_on_persistence(err).await;
                        return;
                    }
                    self.next_entry_seq = new_entry_seq;
                    staged.state_seq += 1;
                    self.operation = Some(staged);
                    self.emit_terminal_state(&applied.state);
                    self.operation.take();
                }
            }
            OperationState::Accepted
            | OperationState::NeedAssistant
            | OperationState::NeedContinuation
            | OperationState::ToolsPlanned { .. }
            | OperationState::Suspended => {
                // Quiescent or fully-committed states continue through
                // ordinary flow.
                self.advance().await;
            }
            OperationState::Finished(_) => {}
        }
    }
}
