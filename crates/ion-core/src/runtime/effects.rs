use super::*;

impl<P: Provider> SessionRuntime<P> {
    pub(crate) fn spawn_model_step(
        &mut self,
        operation_id: OperationId,
        model: ModelConfig,
        plan: ContextPlan,
        tools: Vec<ToolSpec>,
    ) {
        let provider = Arc::clone(&self.provider);
        let cancel = self
            .active(operation_id)
            .map(|active| active.cancel.child_token())
            .unwrap_or_else(|| self.cancel_root.child_token());
        let out = self.engine_tx.clone();
        let live = self
            .live_mut(operation_id)
            .expect("operation residency exists before model execution");
        live.model_step += 1;
        let step = live.model_step;
        let request = ProviderRequest {
            operation_id,
            step,
            model: model.clone(),
            plan,
            tools,
        };
        debug!(%operation_id, step, model = %model.model_ref, "starting model step effect");
        let terminal = self.engine_tx.clone();
        self.tracker.spawn(async move {
            provider.run(request, cancel, out.clone()).await;
            let _ = terminal
                .send(EngineSignal::ProviderExited { operation_id, step })
                .await;
        });
    }

    pub(crate) async fn persist_assistant_frame(
        &mut self,
        operation_id: OperationId,
    ) -> Result<(), StoreError> {
        let Some(effect_id) = self
            .active(operation_id)
            .and_then(|active| active.open_effect.as_ref().map(|effect| effect.id))
        else {
            return Ok(());
        };
        let live = self
            .live(operation_id)
            .expect("operation residency exists while persisting assistant frame");
        let frame = AssistantFrame {
            effect_id,
            session_id: self.session_id,
            operation_id,
            step: live.model_step,
            frame_seq: live.assistant_frame_seq.saturating_add(1),
            text: bounded_frame_text(&live.draft_text),
            thinking: bounded_frame_text(&live.draft_thinking),
        };
        self.live_mut(operation_id)
            .expect("operation residency exists while installing assistant frame")
            .assistant_frame_seq = frame.frame_seq;
        self.store.upsert_assistant_frame(frame).await
    }

    pub(crate) fn spawn_tool_effect(
        &mut self,
        effect_id: Option<EffectId>,
        call: ToolCall,
        reconciliation: Option<serde_json::Value>,
        tools: ToolRegistry,
    ) {
        let Some(effect_id) = effect_id else {
            return;
        };
        let artifact_root = self.artifact_root.clone();
        let ToolCall {
            operation_id,
            call_id,
            name,
            arguments,
        } = call;
        let cancel = self
            .active(operation_id)
            .map(|active| active.cancel.child_token())
            .unwrap_or_else(|| self.cancel_root.child_token());
        let tool_tx = self.tool_tx.clone();
        let (progress_tx, mut progress_rx) = mpsc::channel::<ToolProgress>(8);
        debug!(tool = %name, %call_id, "dispatching tool effect");
        self.tracker.spawn(async move {
            let execute = tools.execute_with_reconciliation(
                &name,
                &arguments,
                reconciliation.as_ref(),
                artifact_root.as_deref(),
                cancel,
                Some(progress_tx),
            );
            let forward = async {
                while let Some(progress) = progress_rx.recv().await {
                    if tool_tx
                        .send(ToolSignal::Progress {
                            operation_id,
                            effect_id,
                            call_id,
                            output: progress.output,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            };
            let (outcome, ()) = tokio::join!(execute, forward);
            let result = ToolResult::from_outcome(call_id, outcome);
            let _ = tool_tx
                .send(ToolSignal::Settled {
                    operation_id,
                    effect_id,
                    result,
                })
                .await;
        });
    }

    pub(crate) async fn handle_engine(&mut self, signal: EngineSignal) {
        let operation_id = signal_operation_id(&signal);
        let Some((active_operation_id, compaction_pending)) =
            self.active(operation_id).map(|active| {
                (
                    active.machine.operation_id(),
                    matches!(active.machine.state(), OperationState::CompactionPending),
                )
            })
        else {
            debug!(%operation_id, "ignored engine signal for non-resident operation");
            return;
        };
        debug_assert_eq!(active_operation_id, operation_id);
        let model_step = self
            .live(operation_id)
            .expect("resident operation has live execution state")
            .model_step;
        if signal_step(&signal) != model_step {
            debug!(?signal, "ignored engine signal from a stale model step");
            return;
        }
        if matches!(
            &signal,
            EngineSignal::Completed { .. }
                | EngineSignal::Failed { .. }
                | EngineSignal::Cancelled { .. }
                | EngineSignal::ProviderExited { .. }
        ) {
            self.wait_effect_boundary(EffectBoundary::ModelSettlement)
                .await;
        }
        if compaction_pending {
            self.settle_compaction(operation_id, signal).await;
            return;
        }
        match signal {
            EngineSignal::TextDelta { text, .. } => {
                self.live_mut(operation_id)
                    .expect("resident operation has live execution state")
                    .draft_text
                    .push_str(&text);
                self.emit(RuntimeEvent::AssistantTextDelta {
                    cursor: RuntimeCursor::default(),
                    operation_id,
                    text,
                });
                if let Err(err) = self.persist_assistant_frame(operation_id).await {
                    self.fail_operation_on_persistence_for(operation_id, err)
                        .await;
                }
            }
            EngineSignal::ThinkingDelta { text, .. } => {
                self.live_mut(operation_id)
                    .expect("resident operation has live execution state")
                    .draft_thinking
                    .push_str(&text);
                self.emit(RuntimeEvent::ThinkingDelta {
                    cursor: RuntimeCursor::default(),
                    operation_id,
                    text,
                });
                if let Err(err) = self.persist_assistant_frame(operation_id).await {
                    self.fail_operation_on_persistence_for(operation_id, err)
                        .await;
                }
            }
            EngineSignal::UsageUpdate { usage, .. } => {
                self.operation_lane_live_mut(operation_id)
                    .expect("resident operation has an owning lane")
                    .last_context_tokens =
                    Some(usage.input + usage.output + usage.cache_read + usage.cache_write);
                self.live_mut(operation_id)
                    .expect("resident operation has live execution state")
                    .draft_usage = Some(usage);
                self.emit(RuntimeEvent::UsageUpdate {
                    cursor: RuntimeCursor::default(),
                    operation_id,
                    usage,
                });
            }
            EngineSignal::ToolCallCompleted { call, .. } => {
                if call.operation_id != operation_id {
                    debug!(?call, "dropped tool call attributed to another operation");
                    return;
                }
                self.live_mut(operation_id)
                    .expect("resident operation has live execution state")
                    .draft_calls
                    .push(call);
            }
            EngineSignal::Completed { .. } => {
                let cancel_requested = self
                    .active(operation_id)
                    .is_some_and(|active| active.machine.cancel_requested());
                let live = self
                    .live_mut(operation_id)
                    .expect("resident operation has live execution state");
                let text = std::mem::take(&mut live.draft_text);
                let tool_calls = std::mem::take(&mut live.draft_calls);
                let transition = if cancel_requested {
                    Transition::ProviderCancelled
                } else {
                    Transition::ProviderCompleted { text, tool_calls }
                };
                self.settle_model_step(operation_id, transition).await;
            }
            EngineSignal::Failed { message, .. } => {
                let cancel_requested = self
                    .active(operation_id)
                    .is_some_and(|active| active.machine.cancel_requested());
                let live = self
                    .live(operation_id)
                    .expect("resident operation has live execution state");
                if !cancel_requested
                    && is_context_overflow(&message)
                    && !live.overflow_retry_used
                    && !live.last_step_was_compaction
                {
                    self.settle_overflow_to_compaction(operation_id).await;
                    return;
                }
                let transition = if cancel_requested {
                    Transition::ProviderCancelled
                } else {
                    Transition::ProviderFailed { message }
                };
                self.settle_model_step(operation_id, transition).await;
            }
            EngineSignal::Cancelled { .. } => {
                self.settle_model_step(operation_id, Transition::ProviderCancelled)
                    .await;
            }
            EngineSignal::ProviderExited { .. } => {
                self.settle_model_step(
                    operation_id,
                    Transition::ProviderFailed {
                        message: "provider exited without a terminal signal".to_owned(),
                    },
                )
                .await;
            }
        }
    }

    /// Commit a model-step settlement atomically: settled effect (with
    /// its typed outcome), semantic entries, and the next total state
    /// agree in one transaction. Every settlement path ends the live
    /// reasoning draft (display-only, §21.3).
    pub(crate) async fn settle_model_step(
        &mut self,
        operation_id: OperationId,
        transition: Transition,
    ) {
        let mut staged = self
            .active(operation_id)
            .cloned()
            .expect("settle needs an operation");
        if !matches!(
            staged.machine.state(),
            OperationState::AssistantEffectPending
        ) {
            // A same-step exit sentinel after an already-settled step.
            debug!("ignored settlement for an already-settled model step");
            return;
        }
        self.live_mut(operation_id)
            .expect("main operation residency exists")
            .draft_thinking
            .clear();
        self.live_mut(operation_id)
            .expect("main operation residency exists")
            .last_step_was_compaction = false;
        let applied = staged
            .machine
            .apply(transition)
            .expect("model-step settlement while AssistantEffectPending");
        let frame_effect_id = staged.open_effect.as_ref().map(|effect| effect.id);
        let settled = staged
            .open_effect
            .take()
            .map(|effect| SettledEffect {
                id: effect.id,
                settlement: serde_json::json!({ "kind": "model_step" }),
            })
            .into_iter()
            .collect();
        // Usage persists with the settlement, independent of operation
        // success (DESIGN.md §27.2).
        let settled_usage = self
            .live_mut(operation_id)
            .expect("main operation residency exists")
            .draft_usage
            .take();
        let usage = settled_usage
            .map(|u| {
                vec![UsageRecord {
                    step: self
                        .live_mut(operation_id)
                        .expect("main operation residency exists")
                        .model_step,
                    input_tokens: u.input,
                    output_tokens: u.output,
                    cache_read_tokens: u.cache_read,
                    cache_write_tokens: u.cache_write,
                }]
            })
            .unwrap_or_default();
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
            usage,
        );
        request.assistant_frames_delete = frame_effect_id.into_iter().collect();
        if let Err(err) = self.commit_transition(request).await {
            self.fail_operation_on_persistence_for(operation_id, err)
                .await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        if let Some(usage) = settled_usage {
            self.operation_lane_live_mut(operation_id)
                .expect("resident operation has an owning lane")
                .latest_usage = Some(usage);
        }
        self.emit_terminal_state_for(operation_id, &applied.state.clone());
        self.install_active(staged);
        self.advance(operation_id).await;
    }

    /// Settle a context-overflow failure into a compaction (14.7.4):
    /// the failed attempt's effect settles without entries, the
    /// Compaction intent commits in the same transaction, and the
    /// retry is the natural continuation after the summary lands.
    pub(crate) async fn settle_overflow_to_compaction(&mut self, operation_id: OperationId) {
        self.live_mut(operation_id)
            .expect("main operation residency exists")
            .overflow_retry_used = true;
        let mut staged = self
            .active(operation_id)
            .cloned()
            .expect("settle needs an operation");
        let frame_effect_id = staged.open_effect.as_ref().map(|effect| effect.id);
        let model = self.current_model_config(operation_id).await;
        let (_, manifest) = self.current_context_manifest();
        let branch = self
            .operation_branch_records(operation_id)
            .expect("resident operation lane branch is complete");
        let mut plan = project_with_manifest(
            branch.iter().map(|record| &record.entry),
            branch
                .first()
                .map_or(self.next_entry_seq, |record| record.seq),
            &manifest,
        );
        plan.messages.push(crate::context::ContextMessage::User {
            content: crate::context::SUMMARIZE_INSTRUCTION.to_owned(),
        });
        let applied = staged
            .machine
            .apply(Transition::OverflowCompaction { plan: plan.clone() })
            .expect("overflow compaction while AssistantEffectPending");
        let EffectIntent::Compaction { operation_id, .. } = applied.intents[0].clone() else {
            panic!("OverflowCompaction must yield a compaction intent");
        };
        let settled = staged
            .open_effect
            .take()
            .map(|effect| SettledEffect {
                id: effect.id,
                settlement: serde_json::json!({ "kind": "model_step", "overflow": true }),
            })
            .into_iter()
            .collect();
        let effect = EffectRecord {
            id: EffectId::generate(),
            kind: "compaction".to_owned(),
            recovery_class: RecoveryClass::ReplaySafe,
            effective_input: serde_json::json!({
                "step": self.live_mut(operation_id).expect("main operation residency exists").model_step + 1,
                "model": model,
                "plan": plan
            }),
            attempt: 1,
        };
        staged.open_effect = Some(effect.clone());
        let (mut request, new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            applied.entries.clone(),
            vec![effect],
            settled,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        request.assistant_frames_delete = frame_effect_id.into_iter().collect();
        if let Err(err) = self.commit_transition(request).await {
            self.fail_operation_on_persistence_for(operation_id, err)
                .await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.install_active(staged);
        // The failed attempt's partial buffers are discarded; usage
        // from a rejected request is not trustworthy.
        self.live_mut(operation_id)
            .expect("main operation residency exists")
            .draft_text
            .clear();
        self.live_mut(operation_id)
            .expect("main operation residency exists")
            .draft_thinking
            .clear();
        self.live_mut(operation_id)
            .expect("main operation residency exists")
            .draft_calls
            .clear();
        self.live_mut(operation_id)
            .expect("main operation residency exists")
            .draft_usage = None;
        self.live_mut(operation_id)
            .expect("main operation residency exists")
            .last_step_was_compaction = true;
        self.operation_lane_live_mut(operation_id)
            .expect("resident operation has an owning lane")
            .last_prefix_fingerprint = None;
        warn!(%operation_id, "context overflow; compacting once and retrying");
        self.spawn_model_step(operation_id, model, plan, Vec::new());
    }

    /// Settle a compaction step: the summary becomes a readable entry
    /// covering everything before it; failure continues without a
    /// baseline (visible in tracing), unless cancellation was requested.
    pub(crate) async fn settle_compaction(
        &mut self,
        operation_id: OperationId,
        signal: EngineSignal,
    ) {
        let mut staged = self
            .active(operation_id)
            .cloned()
            .expect("settle needs an operation");
        if !matches!(staged.machine.state(), OperationState::CompactionPending) {
            debug!("ignored settlement for an already-settled compaction step");
            return;
        }
        let transition = match signal {
            EngineSignal::TextDelta { text, .. } => {
                self.live_mut(operation_id)
                    .expect("main operation residency exists")
                    .draft_text
                    .push_str(&text);
                return;
            }
            EngineSignal::Completed { .. } => {
                let summary = std::mem::take(
                    &mut self
                        .live_mut(operation_id)
                        .expect("main operation residency exists")
                        .draft_text,
                );
                Transition::CompactionCompleted {
                    summary,
                    covers_through_seq: self.next_entry_seq - 1,
                }
            }
            EngineSignal::Failed { message, .. } => {
                warn!(message = %message, "compaction generation failed; continuing without a baseline");
                Transition::CompactionFailed
            }
            EngineSignal::Cancelled { .. } | EngineSignal::ProviderExited { .. } => {
                Transition::CompactionFailed
            }
            EngineSignal::ThinkingDelta { .. } => return,
            EngineSignal::ToolCallCompleted { .. } | EngineSignal::UsageUpdate { .. } => return,
        };
        let applied = staged
            .machine
            .apply(transition)
            .expect("compaction settlement while CompactionPending");
        let settled = staged
            .open_effect
            .take()
            .map(|effect| SettledEffect {
                id: effect.id,
                settlement: serde_json::json!({ "kind": "compaction" }),
            })
            .into_iter()
            .collect();
        let (request, new_entry_seq) = build_commit_request(
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
        if let Err(err) = self.commit_transition(request).await {
            self.fail_operation_on_persistence_for(operation_id, err)
                .await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.emit_terminal_state_for(operation_id, &applied.state.clone());
        self.install_active(staged);
        self.advance(operation_id).await;
    }

    pub(crate) async fn handle_tool_result(&mut self, settlement: ToolSettlement) {
        let (operation_id, effect_id, result) = match settlement {
            ToolSignal::Settled {
                operation_id,
                effect_id,
                result,
            } => (operation_id, effect_id, result),
            ToolSignal::Progress {
                operation_id,
                effect_id,
                call_id,
                output,
            } => {
                let Some(active) = self.active(operation_id) else {
                    debug!(%operation_id, %effect_id, "dropped tool progress for non-resident operation");
                    return;
                };
                if active.open_effect.as_ref().map(|effect| effect.id) != Some(effect_id) {
                    debug!(%operation_id, %effect_id, "dropped stale tool progress");
                    return;
                }
                let output = crate::tool::bounded_progress_output(output);
                let event_output = output.clone();
                let progress = ToolProgressCheckpoint {
                    effect_id,
                    session_id: self.session_id,
                    operation_id,
                    call_id,
                    output,
                };
                if let Err(err) = self.store.upsert_tool_progress(progress).await {
                    self.fail_operation_on_persistence_for(operation_id, err)
                        .await;
                } else {
                    self.emit(RuntimeEvent::ToolProgress {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                        call_id,
                        output: event_output,
                    });
                }
                return;
            }
        };
        let call_id = result.call_id();
        let is_error = matches!(&result, ToolResult::Err { .. });
        let preview = result.display_preview();
        let expected = self
            .active(operation_id)
            .and_then(|active| active.open_effect.as_ref().map(|effect| effect.id));
        if expected != Some(effect_id) {
            // Stale or unknown tool result: operation and effect identity
            // must both match before the session writer mutates state.
            debug!(%operation_id, %effect_id, ?expected, "dropped stale tool settlement");
            return;
        }
        self.wait_effect_boundary(EffectBoundary::ToolSettlement)
            .await;
        let mut staged = self
            .active(operation_id)
            .cloned()
            .expect("settle needs an operation");
        let applied = staged
            .machine
            .apply(Transition::ToolSettled {
                result: result.clone(),
            })
            .expect("tool settlement while ToolEffectPending");
        let settled = vec![SettledEffect {
            id: effect_id,
            settlement: serde_json::json!({ "output": result.model_text() }),
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
        request.tool_progress_delete.push(effect_id);
        if let Err(err) = self.commit_transition(request).await {
            self.fail_operation_on_persistence_for(operation_id, err)
                .await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        staged.open_effect = None;
        self.live_mut(operation_id)
            .expect("main operation residency exists")
            .live_tools
            .retain(|pending| pending.call_id != call_id);
        self.emit(RuntimeEvent::ToolSettled {
            cursor: RuntimeCursor::default(),
            operation_id: staged.machine.operation_id(),
            call_id,
            is_error,
            preview,
        });
        self.emit_terminal_state_for(operation_id, &applied.state.clone());
        self.install_active(staged);
        self.advance(operation_id).await;
    }

    /// A required commit failed: the staged clone is discarded and live
    /// state stays at its last durable checkpoint. Fail the operation
    /// visibly from that checkpoint; if even the failure commit fails,
    /// fence the session — never continue as if durability succeeded.
    pub(crate) async fn fail_operation_on_persistence_for(
        &mut self,
        operation_id: OperationId,
        err: StoreError,
    ) {
        let Some(active) = self.active(operation_id) else {
            error!(session = %self.session_id, %operation_id, %err, "persistence failed with no resident operation");
            return;
        };
        error!(
            %operation_id,
            %err,
            "durable commit failed; failing the operation from its last checkpoint"
        );
        if matches!(active.machine.state(), OperationState::Finished(_)) {
            self.emit(RuntimeEvent::OperationFailed {
                cursor: RuntimeCursor::default(),
                operation_id,
                message: format!("persistence failed: {err}"),
            });
            self.remove_operation(operation_id);
            return;
        }
        let mut staged = active.clone();
        staged
            .machine
            .apply(Transition::FailOperation {
                message: format!("persistence failed: {err}"),
            })
            .expect("fail the operation from an open state");
        staged.open_effect = None;
        let (request, _new_entry_seq) = build_commit_request(
            self.session_id,
            &staged,
            staged.state_seq + 1,
            self.next_entry_seq,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        match self.commit_transition(request).await {
            Ok(()) => {
                let applied_state = staged.machine.state().clone();
                self.install_active(staged);
                self.emit_terminal_state_for(operation_id, &applied_state);
                if let Some(active) = self.active(operation_id) {
                    active.cancel.cancel();
                }
                self.remove_operation(operation_id);
            }
            Err(second) => {
                error!(
                    %operation_id,
                    %second,
                    "failure commit also failed; fencing the session at its last checkpoint"
                );
                self.emit(RuntimeEvent::OperationFailed {
                    cursor: RuntimeCursor::default(),
                    operation_id,
                    message: format!("persistence failed fatally: {second}"),
                });
                self.closed = true;
                self.remove_operation(operation_id);
            }
        }
    }
}
