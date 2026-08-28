from pathlib import Path
import re


def replace_once(path: Path, old: str, new: str, label: str) -> None:
    text = path.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    path.write_text(text.replace(old, new, 1))


def regex_once(path: Path, pattern: str, replacement: str, label: str) -> None:
    text = path.read_text()
    new, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    path.write_text(new)


runtime = Path("crates/ion-core/src/runtime/mod.rs")
effects = Path("crates/ion-core/src/runtime/effects.rs")

# Add identity-addressed residency accessors while retaining main projections
# for the still-single-lane command surface.
replace_once(
    runtime,
    '''    fn lane_resident_id(&self, lane_name: &str) -> Option<OperationId> {
''',
    '''    fn resident(&self, operation_id: OperationId) -> Option<&ResidentOperation> {
        self.operations.get(&operation_id)
    }

    fn resident_mut(&mut self, operation_id: OperationId) -> Option<&mut ResidentOperation> {
        self.operations.get_mut(&operation_id)
    }

    fn active(&self, operation_id: OperationId) -> Option<&ActiveOperation> {
        self.resident(operation_id).map(|resident| &resident.active)
    }

    fn live(&self, operation_id: OperationId) -> Option<&OperationResidency> {
        self.resident(operation_id).map(|resident| &resident.live)
    }

    fn live_mut(&mut self, operation_id: OperationId) -> Option<&mut OperationResidency> {
        self.resident_mut(operation_id)
            .map(|resident| &mut resident.live)
    }

    fn operation_lane_name(&self, operation_id: OperationId) -> Option<&str> {
        self.resident(operation_id)
            .map(|resident| resident.lane_name.as_str())
    }

    fn lane_resident_id(&self, lane_name: &str) -> Option<OperationId> {
''',
    "identity residency helpers",
)
replace_once(
    runtime,
    '''    fn main_resident(&self) -> Option<&ResidentOperation> {
        let operation_id = self.main_resident_id()?;
        self.operations.get(&operation_id)
    }

    fn main_resident_mut(&mut self) -> Option<&mut ResidentOperation> {
        let operation_id = self.main_resident_id()?;
        self.operations.get_mut(&operation_id)
    }
''',
    '''    fn main_resident(&self) -> Option<&ResidentOperation> {
        self.resident(self.main_resident_id()?)
    }

    fn main_resident_mut(&mut self) -> Option<&mut ResidentOperation> {
        self.resident_mut(self.main_resident_id()?)
    }
''',
    "main projections use identity helpers",
)
replace_once(
    runtime,
    '''    fn remove_main_operation(&mut self) -> Option<ResidentOperation> {
        let operation_id = self.main_resident_id()?;
        self.operations.remove(&operation_id)
    }
''',
    '''    fn remove_operation(&mut self, operation_id: OperationId) -> Option<ResidentOperation> {
        self.operations.remove(&operation_id)
    }

    fn remove_main_operation(&mut self) -> Option<ResidentOperation> {
        self.remove_operation(self.main_resident_id()?)
    }
''',
    "generic remove operation",
)

# Model effect launch is already passed authoritative operation identity; use it
# rather than re-resolving through main.
regex_once(
    effects,
    r'''    pub\(crate\) fn spawn_model_step\(\n        &mut self,\n        operation_id: OperationId,\n        model: ModelConfig,\n        plan: ContextPlan,\n        tools: Vec<ToolSpec>,\n    \) \{.*?\n    \}\n\n    pub\(crate\) async fn persist_assistant_frame''',
    '''    pub(crate) fn spawn_model_step(
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

    pub(crate) async fn persist_assistant_frame''',
    "operation-address model launch",
)

# Assistant recovery frames belong to one operation/effect, not to main.
regex_once(
    effects,
    r'''    pub\(crate\) async fn persist_assistant_frame\(&mut self\) -> Result<\(\), StoreError> \{.*?\n    \}\n\n    pub\(crate\) fn spawn_tool_effect''',
    '''    pub(crate) async fn persist_assistant_frame(
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

    pub(crate) fn spawn_tool_effect''',
    "operation-address assistant frame",
)

# Tool execution also owns operation identity in the ToolCall itself.
regex_once(
    effects,
    r'''    pub\(crate\) fn spawn_tool_effect\(\n        &mut self,\n        effect_id: Option<EffectId>,\n        call: ToolCall,\n        reconciliation: Option<serde_json::Value>,\n        tools: ToolRegistry,\n    \) \{.*?\n    \}\n\n    pub\(crate\) async fn handle_engine''',
    '''    pub(crate) fn spawn_tool_effect(
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

    pub(crate) async fn handle_engine''',
    "operation-address tool launch",
)

# Provider ingress routes by the identity on the signal. Continuation is still
# main-only until the next lane-addressed control-flow checkpoint.
regex_once(
    effects,
    r'''    pub\(crate\) async fn handle_engine\(&mut self, signal: EngineSignal\) \{.*?\n    \}\n\n    /// Commit a model-step settlement atomically''',
    '''    pub(crate) async fn handle_engine(&mut self, signal: EngineSignal) {
        let operation_id = signal_operation_id(&signal);
        let Some((active_operation_id, compaction_pending)) = self.active(operation_id).map(|active| {
            (
                active.machine.operation_id(),
                matches!(active.machine.state(), OperationState::CompactionPending),
            )
        }) else {
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
                    self.fail_operation_on_persistence_for(operation_id, err).await;
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
                    self.fail_operation_on_persistence_for(operation_id, err).await;
                }
            }
            EngineSignal::UsageUpdate { usage, .. } => {
                self.last_context_tokens =
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

    /// Commit a model-step settlement atomically''',
    "operation-address engine ingress",
)

# Settlement helpers become explicitly operation-addressed. The persistence and
# continuation helpers they call are intentionally migrated in the next slice.
text = effects.read_text()
text = text.replace(
    "pub(crate) async fn settle_model_step(&mut self, transition: Transition)",
    "pub(crate) async fn settle_model_step(\n        &mut self,\n        operation_id: OperationId,\n        transition: Transition,\n    )",
    1,
)
text = text.replace(
    "pub(crate) async fn settle_overflow_to_compaction(&mut self)",
    "pub(crate) async fn settle_overflow_to_compaction(&mut self, operation_id: OperationId)",
    1,
)
text = text.replace(
    "pub(crate) async fn settle_compaction(&mut self, signal: EngineSignal)",
    "pub(crate) async fn settle_compaction(\n        &mut self,\n        operation_id: OperationId,\n        signal: EngineSignal,\n    )",
    1,
)
# Within those functions and tool settlement, use the identity-addressed
# resident. This file has no unrelated main_active/main_live references after
# the launch/ingress rewrites except the failure helper, replaced below.
text = text.replace("self.main_active()", "self.active(operation_id)")
text = text.replace("self.main_live_mut()", "self.live_mut(operation_id)")
text = text.replace("self.main_live()", "self.live(operation_id)")
# Explicit failure routing for effect paths. Existing runtime/recovery callers
# retain the main wrapper until continuation itself is operation-addressed.
text = text.replace(
    "self.fail_operation_on_persistence(err).await",
    "self.fail_operation_on_persistence_for(operation_id, err).await",
)
# Terminal emission/removal in this file should use the operation that produced
# the signal, not whatever main currently points at.
text = text.replace("self.emit_terminal_state(&", "self.emit_terminal_state_for(operation_id, &")
text = text.replace("self.remove_main_operation()", "self.remove_operation(operation_id)")
effects.write_text(text)

# Tool progress/settlement must resolve by operation identity directly.
text = effects.read_text()
text = text.replace(
    '''                let Some(active) = self.active(operation_id) else {
                    return;
                };
                if active.machine.operation_id() != operation_id
                    || active.open_effect.as_ref().map(|effect| effect.id) != Some(effect_id)
''',
    '''                let Some(active) = self.active(operation_id) else {
                    debug!(%operation_id, %effect_id, "dropped tool progress for non-resident operation");
                    return;
                };
                if active.open_effect.as_ref().map(|effect| effect.id) != Some(effect_id)
''',
    1,
)
text = text.replace(
    '''        let expected = self.active(operation_id).and_then(|active| {
            (active.machine.operation_id() == operation_id)
                .then(|| active.open_effect.as_ref().map(|effect| effect.id))
                .flatten()
        });
''',
    '''        let expected = self
            .active(operation_id)
            .and_then(|active| active.open_effect.as_ref().map(|effect| effect.id));
''',
    1,
)
effects.write_text(text)

# Make terminal event projection identity-addressed while keeping the legacy
# main wrapper for command paths not yet migrated.
regex_once(
    runtime,
    r'''    fn emit_terminal_state\(&mut self, state: &OperationState\) \{.*?\n    \}\n\n    fn subscribe''',
    '''    fn emit_terminal_state_for(
        &mut self,
        operation_id: OperationId,
        state: &OperationState,
    ) {
        if let Some(live) = self.live_mut(operation_id) {
            live.live_tools.clear();
        }
        if let OperationState::Finished(outcome) = state {
            match outcome {
                OperationOutcome::Completed => {
                    self.emit(RuntimeEvent::OperationFinished {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                    });
                }
                OperationOutcome::Failed(message) => {
                    self.emit(RuntimeEvent::OperationFailed {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                        message: message.clone(),
                    });
                }
                OperationOutcome::ApprovalRequired { tool } => {
                    self.emit(RuntimeEvent::OperationApprovalRequired {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                        tool: tool.clone(),
                    });
                }
                OperationOutcome::Cancelled => {
                    self.emit(RuntimeEvent::OperationCancelled {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                    });
                }
                OperationOutcome::Indeterminate => {
                    self.indeterminate_warning = Some(IndeterminateWarning {
                        operation_id,
                        message: INDETERMINATE_MESSAGE.to_owned(),
                    });
                    self.emit(RuntimeEvent::OperationIndeterminate {
                        cursor: RuntimeCursor::default(),
                        operation_id,
                        message: INDETERMINATE_MESSAGE.to_owned(),
                    });
                }
            }
        }
    }

    fn emit_terminal_state(&mut self, state: &OperationState) {
        if let Some(operation_id) = self.main_resident_id() {
            self.emit_terminal_state_for(operation_id, state);
        }
    }

    fn subscribe''',
    "identity terminal events",
)

# Replace the failure handler with a generic implementation plus a temporary
# main wrapper for call sites migrated in the next control-flow slice.
regex_once(
    effects,
    r'''    /// A required commit failed:.*?    pub\(crate\) async fn fail_operation_on_persistence\(&mut self, err: StoreError\) \{.*?\n    \}\n\}\n$''',
    '''    /// A required commit failed: the staged clone is discarded and live
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

    pub(crate) async fn fail_operation_on_persistence(&mut self, err: StoreError) {
        let Some(operation_id) = self.main_resident_id() else {
            error!(session = %self.session_id, %err, "persistence failed with no active main operation");
            return;
        };
        self.fail_operation_on_persistence_for(operation_id, err).await;
    }
}
''',
    "generic persistence failure routing",
)

# The identity helper is deliberately exercised now; fail if the routing slice
# accidentally leaves no production use for it under strict dead-code lints.
text = runtime.read_text()
if "fn operation_lane_name" not in text:
    raise SystemExit("operation lane identity helper missing")
# Keep the helper only if used; this slice does not need it yet, so remove it
# rather than suppress dead_code. It will reappear with lane-addressed commit.
text = re.sub(
    r'''\n    fn operation_lane_name\(&self, operation_id: OperationId\) -> Option<&str> \{\n        self\.resident\(operation_id\)\n            \.map\(\|resident\| resident\.lane_name\.as_str\(\)\)\n    \}\n''',
    "\n",
    text,
    count=1,
)
runtime.write_text(text)
