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
            .operation
            .as_ref()
            .map(|active| active.cancel.child_token())
            .unwrap_or_else(|| self.cancel_root.child_token());
        let out = self.engine_tx.clone();
        self.model_step += 1;
        let step = self.model_step;
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

    pub(crate) async fn persist_assistant_frame(&mut self) -> Result<(), StoreError> {
        let Some(active) = self.operation.as_ref() else {
            return Ok(());
        };
        let Some(effect) = active.open_effect.as_ref() else {
            return Ok(());
        };
        let frame = AssistantFrame {
            effect_id: effect.id,
            session_id: self.session_id,
            operation_id: active.machine.operation_id(),
            step: self.model_step,
            frame_seq: self.assistant_frame_seq.saturating_add(1),
            text: bounded_frame_text(&self.draft_text),
            thinking: bounded_frame_text(&self.draft_thinking),
        };
        self.assistant_frame_seq = frame.frame_seq;
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
        let cancel = self
            .operation
            .as_ref()
            .map(|active| active.cancel.child_token())
            .unwrap_or_else(|| self.cancel_root.child_token());
        let tool_tx = self.tool_tx.clone();
        let (progress_tx, mut progress_rx) = mpsc::channel::<ToolProgress>(8);
        let ToolCall {
            call_id,
            name,
            arguments,
            ..
        } = call;
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
                .send(ToolSignal::Settled { effect_id, result })
                .await;
        });
    }

    pub(crate) async fn handle_engine(&mut self, signal: EngineSignal) {
        let Some(active) = &self.operation else {
            debug!("ignored engine signal with no active operation");
            return;
        };
        if active.machine.operation_id() != signal_operation_id(&signal) {
            debug!(?signal, "ignored stale engine signal");
            return;
        }
        if signal_step(&signal) != self.model_step {
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
        if matches!(active.machine.state(), OperationState::CompactionPending) {
            self.settle_compaction(signal).await;
            return;
        }
        match signal {
            EngineSignal::TextDelta { text, .. } => {
                self.draft_text.push_str(&text);
                self.emit(RuntimeEvent::AssistantTextDelta {
                    cursor: RuntimeCursor::default(),
                    operation_id: active.machine.operation_id(),
                    text,
                });
                if let Err(err) = self.persist_assistant_frame().await {
                    self.fail_operation_on_persistence(err).await;
                }
            }
            EngineSignal::ThinkingDelta { text, .. } => {
                self.draft_thinking.push_str(&text);
                self.emit(RuntimeEvent::ThinkingDelta {
                    cursor: RuntimeCursor::default(),
                    operation_id: active.machine.operation_id(),
                    text,
                });
                if let Err(err) = self.persist_assistant_frame().await {
                    self.fail_operation_on_persistence(err).await;
                }
            }
            EngineSignal::UsageUpdate { usage, .. } => {
                self.last_context_tokens =
                    Some(usage.input + usage.output + usage.cache_read + usage.cache_write);
                self.draft_usage = Some(usage);
                self.emit(RuntimeEvent::UsageUpdate {
                    cursor: RuntimeCursor::default(),
                    operation_id: active.machine.operation_id(),
                    usage,
                });
            }
            EngineSignal::ToolCallCompleted { call, .. } => {
                if call.operation_id != active.machine.operation_id() {
                    debug!(?call, "dropped tool call attributed to another operation");
                    return;
                }
                // Buffered until the step completes; tool calls are never
                // executed from partial streamed JSON (DESIGN.md §15.2).
                self.draft_calls.push(call);
            }
            EngineSignal::Completed { .. } => {
                let cancel_requested = self
                    .operation
                    .as_ref()
                    .is_some_and(|active| active.machine.cancel_requested());
                let text = std::mem::take(&mut self.draft_text);
                let tool_calls = std::mem::take(&mut self.draft_calls);
                let transition = if cancel_requested {
                    Transition::ProviderCancelled
                } else {
                    Transition::ProviderCompleted { text, tool_calls }
                };
                self.settle_model_step(transition).await;
            }
            EngineSignal::Failed { message, .. } => {
                let cancel_requested = self
                    .operation
                    .as_ref()
                    .is_some_and(|active| active.machine.cancel_requested());
                if !cancel_requested
                    && is_context_overflow(&message)
                    && !self.overflow_retry_used
                    && !self.last_step_was_compaction
                {
                    // 14.7.4: one compaction, one retry. The failed
                    // attempt produced no durable effect beyond its
                    // intent; its partial request state is discarded.
                    self.settle_overflow_to_compaction().await;
                    return;
                }
                let transition = if cancel_requested {
                    Transition::ProviderCancelled
                } else {
                    Transition::ProviderFailed { message }
                };
                self.settle_model_step(transition).await;
            }
            EngineSignal::Cancelled { .. } => {
                self.settle_model_step(Transition::ProviderCancelled).await;
            }
            EngineSignal::ProviderExited { .. } => {
                // A sentinel for the live step means the provider died
                // without a terminal signal; earlier steps were already
                // dropped by the step correlation above.
                self.settle_model_step(Transition::ProviderFailed {
                    message: "provider exited without a terminal signal".to_owned(),
                })
                .await;
            }
        }
    }

    /// Commit a model-step settlement atomically: settled effect (with
    /// its typed outcome), semantic entries, and the next total state
    /// agree in one transaction. Every settlement path ends the live
    /// reasoning draft (display-only, §21.3).
    pub(crate) async fn settle_model_step(&mut self, transition: Transition) {
        let mut staged = self.operation.clone().expect("settle needs an operation");
        if !matches!(
            staged.machine.state(),
            OperationState::AssistantEffectPending
        ) {
            // A same-step exit sentinel after an already-settled step.
            debug!("ignored settlement for an already-settled model step");
            return;
        }
        self.draft_thinking.clear();
        self.last_step_was_compaction = false;
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
        let settled_usage = self.draft_usage.take();
        let usage = settled_usage
            .map(|u| {
                vec![UsageRecord {
                    step: self.model_step,
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
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.entries.extend(applied.entries);
        if let Some(usage) = settled_usage {
            self.latest_usage = Some(usage);
        }
        self.emit_terminal_state(&applied.state.clone());
        self.operation = Some(staged);
        self.advance().await;
    }

    /// Settle a context-overflow failure into a compaction (14.7.4):
    /// the failed attempt's effect settles without entries, the
    /// Compaction intent commits in the same transaction, and the
    /// retry is the natural continuation after the summary lands.
    pub(crate) async fn settle_overflow_to_compaction(&mut self) {
        self.overflow_retry_used = true;
        let mut staged = self.operation.clone().expect("settle needs an operation");
        let frame_effect_id = staged.open_effect.as_ref().map(|effect| effect.id);
        let model = self.current_model_config().await;
        let (_, manifest) = self.current_context_manifest();
        let mut plan = project_with_manifest(&self.entries, self.first_entry_seq(), &manifest);
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
                "step": self.model_step + 1,
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
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.operation = Some(staged);
        // The failed attempt's partial buffers are discarded; usage
        // from a rejected request is not trustworthy.
        self.draft_text.clear();
        self.draft_thinking.clear();
        self.draft_calls.clear();
        self.draft_usage = None;
        self.last_step_was_compaction = true;
        self.last_prefix_fingerprint = None;
        warn!(%operation_id, "context overflow; compacting once and retrying");
        self.spawn_model_step(operation_id, model, plan, Vec::new());
    }

    /// Settle a compaction step: the summary becomes a readable entry
    /// covering everything before it; failure continues without a
    /// baseline (visible in tracing), unless cancellation was requested.
    pub(crate) async fn settle_compaction(&mut self, signal: EngineSignal) {
        let mut staged = self.operation.clone().expect("settle needs an operation");
        if !matches!(staged.machine.state(), OperationState::CompactionPending) {
            debug!("ignored settlement for an already-settled compaction step");
            return;
        }
        let transition = match signal {
            EngineSignal::TextDelta { text, .. } => {
                self.draft_text.push_str(&text);
                return;
            }
            EngineSignal::Completed { .. } => {
                let summary = std::mem::take(&mut self.draft_text);
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
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        self.entries.extend(applied.entries);
        self.emit_terminal_state(&applied.state.clone());
        self.operation = Some(staged);
        self.advance().await;
    }

    pub(crate) async fn handle_tool_result(&mut self, settlement: ToolSettlement) {
        let (effect_id, result) = match settlement {
            ToolSignal::Settled { effect_id, result } => (effect_id, result),
            ToolSignal::Progress {
                effect_id,
                call_id,
                output,
            } => {
                let Some(active) = self.operation.as_ref() else {
                    return;
                };
                if active.open_effect.as_ref().map(|effect| effect.id) != Some(effect_id) {
                    return;
                }
                let operation_id = active.machine.operation_id();
                let progress = ToolProgressCheckpoint {
                    effect_id,
                    session_id: self.session_id,
                    operation_id,
                    call_id,
                    output,
                };
                if let Err(err) = self.store.upsert_tool_progress(progress).await {
                    self.fail_operation_on_persistence(err).await;
                }
                return;
            }
        };
        let call_id = result.call_id();
        let is_error = matches!(&result, ToolResult::Err { .. });
        let preview = result.display_preview();
        let expected = self
            .operation
            .as_ref()
            .and_then(|active| active.open_effect.as_ref().map(|e| e.id));
        if expected != Some(effect_id) {
            // Stale or unknown tool result: a typed diagnostic, never a
            // panic and never a state change.
            debug!(?effect_id, ?expected, "dropped stale tool settlement");
            return;
        }
        self.wait_effect_boundary(EffectBoundary::ToolSettlement)
            .await;
        let mut staged = self.operation.clone().expect("settle needs an operation");
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
        if let Err(err) = self.store.commit(request).await {
            self.fail_operation_on_persistence(err).await;
            return;
        }
        self.next_entry_seq = new_entry_seq;
        staged.state_seq += 1;
        staged.open_effect = None;
        self.entries.extend(applied.entries);
        self.live_tools.retain(|pending| pending.call_id != call_id);
        self.emit(RuntimeEvent::ToolSettled {
            cursor: RuntimeCursor::default(),
            operation_id: staged.machine.operation_id(),
            call_id,
            is_error,
            preview,
        });
        self.emit_terminal_state(&applied.state.clone());
        self.operation = Some(staged);
        self.advance().await;
    }

    /// A required commit failed: the staged clone is discarded and live
    /// state stays at its last durable checkpoint. Fail the operation
    /// visibly from that checkpoint; if even the failure commit fails,
    /// fence the session — never continue as if durability succeeded
    /// (DESIGN.md §26.2).
    pub(crate) async fn fail_operation_on_persistence(&mut self, err: StoreError) {
        let Some(active) = &self.operation else {
            error!(session = %self.session_id, %err, "persistence failed with no active operation");
            return;
        };
        let operation_id = active.machine.operation_id();
        error!(
            %operation_id,
            %err,
            "durable commit failed; failing the operation from its last checkpoint"
        );
        if matches!(active.machine.state(), OperationState::Finished(_)) {
            // The failed write was the terminal checkpoint itself; the
            // durable operation stays open and recoverable.
            self.emit(RuntimeEvent::OperationFailed {
                cursor: RuntimeCursor::default(),
                operation_id,
                message: format!("persistence failed: {err}"),
            });
            self.operation.take();
            return;
        }
        // Stage the failure from the untouched live machine.
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
        match self.store.commit(request).await {
            Ok(()) => {
                let applied_state = staged.machine.state().clone();
                self.operation = Some(staged);
                self.emit_terminal_state(&applied_state);
                if let Some(active) = &self.operation {
                    active.cancel.cancel();
                }
                self.operation.take();
            }
            Err(second) => {
                // Fatal: memory stays at the last confirmed checkpoint and
                // the session is fenced — no further work is accepted.
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
                self.operation.take();
            }
        }
    }
}
