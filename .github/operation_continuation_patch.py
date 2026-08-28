from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    return text.replace(old, new, 1)


def rewrite_between(text: str, start: str, end: str, label: str, transform) -> str:
    start_i = text.find(start)
    if start_i < 0:
        raise SystemExit(f"{label}: start not found")
    end_i = text.find(end, start_i)
    if end_i < 0:
        raise SystemExit(f"{label}: end not found")
    block = text[start_i:end_i]
    rewritten = transform(block)
    if rewritten == block:
        raise SystemExit(f"{label}: transform made no changes")
    return text[:start_i] + rewritten + text[end_i:]


runtime = Path("crates/ion-core/src/runtime/mod.rs")
effects = Path("crates/ion-core/src/runtime/effects.rs")
recovery = Path("crates/ion-core/src/runtime/recovery.rs")

text = runtime.read_text()

text = replace_once(
    text,
    '''    fn operation_lane_live_mut(&mut self, operation_id: OperationId) -> Option<&mut LaneResidency> {
        let lane_name = self.operation_lane_name(operation_id)?.to_owned();
        self.lane_live_mut(&lane_name)
    }
''',
    '''    fn operation_lane_live(&self, operation_id: OperationId) -> Option<&LaneResidency> {
        let lane_name = self.operation_lane_name(operation_id)?;
        self.lane_live(lane_name)
    }

    fn operation_lane_live_mut(&mut self, operation_id: OperationId) -> Option<&mut LaneResidency> {
        let lane_name = self.operation_lane_name(operation_id)?.to_owned();
        self.lane_live_mut(&lane_name)
    }
''',
    "operation lane live accessor",
)

text = replace_once(
    text,
    '''    fn main_branch_records(&self) -> Vec<&EntryRecord> {
        self.lane_branch_records(crate::session::lane::MAIN)
            .expect("live main lane branch is complete")
    }
''',
    '''    fn operation_branch_records(&self, operation_id: OperationId) -> Option<Vec<&EntryRecord>> {
        self.lane_branch_records(self.operation_lane_name(operation_id)?)
    }

    fn main_branch_records(&self) -> Vec<&EntryRecord> {
        self.lane_branch_records(crate::session::lane::MAIN)
            .expect("live main lane branch is complete")
    }
''',
    "operation branch accessor",
)

# Startup/main command projection stays main-only, but once an operation is
# selected the continuation driver receives its identity explicitly.
text = text.replace(
    '''        if !self.closed
            && self.main_active().is_none()
            && self.main_pending_next_run().is_some()
            && self.promote_pending_next_run().await
        {
            self.advance().await;
        }
''',
    '''        if !self.closed
            && self.main_active().is_none()
            && self.main_pending_next_run().is_some()
            && self.promote_pending_next_run().await
            && let Some(operation_id) = self.main_resident_id()
        {
            self.advance(operation_id).await;
        }
''',
)

# Public main-lane submission/queue paths hand the accepted identity to the
# operation-addressed driver instead of asking the driver to rediscover main.
text = rewrite_between(
    text,
    "    async fn submit_if_idle(&mut self, prompt: String) -> Result<OperationId, CommandError> {",
    "    /// Persist one next-run input durably.",
    "submit continuation",
    lambda b: b.replace("        self.advance().await;", "        self.advance(operation_id).await;"),
)
text = rewrite_between(
    text,
    "    async fn next_run(&mut self, prompt: String) -> Result<crate::ids::EntryId, CommandError> {",
    "    /// Create the durable operation only when the lane is free.",
    "next-run continuation",
    lambda b: b.replace(
        "            self.advance().await;",
        '''            let operation_id = active.machine.operation_id();
            self.advance(operation_id).await;''',
    ),
)

# Steer and cancel already name one operation semantically; continuation and
# cancellation state now route through that identity rather than main.
def rewrite_steer(block: str) -> str:
    block = block.replace(
        "        self.install_active(staged);\n        self.advance().await;",
        '''        let operation_id = staged.machine.operation_id();
        self.install_active(staged);
        self.advance(operation_id).await;''',
    )
    return block

text = rewrite_between(
    text,
    "    async fn enqueue_steer(&mut self, text: String) -> Result<(), CommandError> {",
    "    /// Request semantic cancellation",
    "steer continuation",
    rewrite_steer,
)


def rewrite_cancel(block: str) -> str:
    block = block.replace(
        '''        let active_id = self
            .main_active()
            .map(|active| active.machine.operation_id());
        if active_id.is_none() {
            return Err(CommandError::NoActiveOperation);
        }
        if active_id != Some(operation_id) {
            return Err(CommandError::NotActive { operation_id });
        }
        let mut staged = self.main_active().cloned().expect("checked above");''',
        '''        let Some(active) = self.active(operation_id) else {
            return if self.operations.is_empty() {
                Err(CommandError::NoActiveOperation)
            } else {
                Err(CommandError::NotActive { operation_id })
            };
        };
        let mut staged = active.clone();''',
    )
    block = block.replace(
        '''        self.main_active()
            .expect("cancelled operation installed")
            .cancel
            .cancel();''',
        '''        self.active(operation_id)
            .expect("cancelled operation installed")
            .cancel
            .cancel();''',
    )
    block = block.replace(
        '''        if let Some(active) = self.main_active()
            && matches!(active.machine.state(), OperationState::Finished(_))''',
        '''        if let Some(active) = self.active(operation_id)
            && matches!(active.machine.state(), OperationState::Finished(_))''',
    )
    block = block.replace(
        "            self.emit_terminal_state(&state);\n            self.advance().await;",
        "            self.emit_terminal_state_for(operation_id, &state);\n            self.advance(operation_id).await;",
    )
    return block

text = rewrite_between(
    text,
    "    async fn cancel(&mut self, operation_id: OperationId) -> Result<(), CommandError> {",
    "    /// Drive the machine forward",
    "cancel routing",
    rewrite_cancel,
)

# Operation-addressed continuation driver.
def rewrite_advance(block: str) -> str:
    block = block.replace(
        "    async fn advance(&mut self) {",
        "    async fn advance(&mut self, mut operation_id: OperationId) {",
    )
    block = block.replace(".main_active()", ".active(operation_id)")
    block = block.replace(".main_live_mut()", ".live_mut(operation_id)")
    block = block.replace("self.drain_queued().await", "self.drain_queued(operation_id).await")
    block = block.replace("self.safety_net_compaction_due()", "self.safety_net_compaction_due(operation_id)")
    block = block.replace("self.start_compaction(request).await", "self.start_compaction(operation_id, request).await")
    block = block.replace("self.start_compaction(None).await", "self.start_compaction(operation_id, None).await")
    block = block.replace("self.start_model_step().await", "self.start_model_step(operation_id).await")
    block = block.replace("self.admit_next_tool().await", "self.admit_next_tool(operation_id).await")
    block = block.replace(
        '''                OperationState::Finished(_) => {
                    self.remove_main_operation();
                    if self.promote_pending_next_run().await {
                        continue;
                    }
                    return;
                }''',
        '''                OperationState::Finished(_) => {
                    let lane_name = self.operation_lane_name(operation_id).map(str::to_owned);
                    self.remove_operation(operation_id);
                    if lane_name.as_deref() == Some(crate::session::lane::MAIN)
                        && self.promote_pending_next_run().await
                        && let Some(next_operation_id) = self.main_resident_id()
                    {
                        operation_id = next_operation_id;
                        continue;
                    }
                    return;
                }''',
    )
    return block

text = rewrite_between(
    text,
    "    async fn advance(&mut self) {",
    "    /// Drain queued steers",
    "advance routing",
    rewrite_advance,
)


def rewrite_drain(block: str) -> str:
    block = block.replace(
        "    async fn drain_queued(&mut self) -> bool {",
        "    async fn drain_queued(&mut self, operation_id: OperationId) -> bool {",
    )
    block = block.replace(".main_active()", ".active(operation_id)")
    block = block.replace(
        "self.fail_operation_on_persistence(err).await;",
        "self.fail_operation_on_persistence_for(operation_id, err).await;",
    )
    return block

text = rewrite_between(
    text,
    "    async fn drain_queued(&mut self) -> bool {",
    "    /// Project the model-step input",
    "drain routing",
    rewrite_drain,
)

# Model metadata and context projection are selected from the operation's owning
# lane. No lane borrow is held across provider awaits.
old_model_config_start = "    async fn current_model_config(&mut self) -> ModelConfig {"
model_config_end = "    fn current_context_manifest"
text = rewrite_between(
    text,
    old_model_config_start,
    model_config_end,
    "model config routing",
    lambda _b: '''    async fn current_model_config(&mut self, operation_id: OperationId) -> ModelConfig {
        let lane_name = self
            .operation_lane_name(operation_id)
            .expect("resident operation has an owning lane")
            .to_owned();
        let selected_model_ref = self
            .lanes
            .get(&lane_name)
            .expect("operation lane exists")
            .durable
            .config
            .model_ref
            .clone();
        let capabilities = match self
            .lane_live(&lane_name)
            .expect("operation lane residency exists")
            .model_capabilities
            .as_ref()
        {
            Some((model_ref, capabilities)) if model_ref == &selected_model_ref => *capabilities,
            _ => {
                let capabilities = self.provider.capabilities_for(&selected_model_ref).await;
                self.lane_live_mut(&lane_name)
                    .expect("operation lane residency exists")
                    .model_capabilities = Some((selected_model_ref.clone(), capabilities));
                capabilities
            }
        };
        let context_window = match self
            .lane_live(&lane_name)
            .expect("operation lane residency exists")
            .context_window
        {
            Some(window) => Some(window),
            None => {
                let window = self.provider.context_window_for(&selected_model_ref).await;
                self.lane_live_mut(&lane_name)
                    .expect("operation lane residency exists")
                    .context_window = window;
                window
            }
        };
        ModelConfig {
            model_ref: selected_model_ref,
            context_window,
            capabilities,
        }
    }

''',
)


def rewrite_cache(block: str) -> str:
    block = block.replace(
        "    fn cache_expectation(&self, model: &ModelConfig, prefix_fingerprint: &str) -> &'static str {",
        "    fn cache_expectation(&self, operation_id: OperationId, model: &ModelConfig, prefix_fingerprint: &str) -> &'static str {",
    )
    block = block.replace(
        "self.main_lane_live().last_prefix_fingerprint.as_deref()",
        '''self.operation_lane_live(operation_id)
            .expect("resident operation has an owning lane")
            .last_prefix_fingerprint
            .as_deref()''',
    )
    return block

text = rewrite_between(
    text,
    "    fn cache_expectation(&self, model: &ModelConfig, prefix_fingerprint: &str) -> &'static str {",
    "    async fn project_model_step_plan",
    "cache expectation routing",
    rewrite_cache,
)


def rewrite_plan(block: str) -> str:
    block = block.replace(
        '''    async fn project_model_step_plan(
        &mut self,
        manifest: &ContextManifest,
    ) -> crate::context::ContextPlan {''',
        '''    async fn project_model_step_plan(
        &mut self,
        operation_id: OperationId,
        manifest: &ContextManifest,
    ) -> crate::context::ContextPlan {''',
    )
    block = block.replace("self.current_model_config().await", "self.current_model_config(operation_id).await")
    block = block.replace(
        "let branch = self.main_branch_records();",
        '''let branch = self
            .operation_branch_records(operation_id)
            .expect("resident operation lane branch is complete");''',
    )
    return block

text = rewrite_between(
    text,
    "    async fn project_model_step_plan(",
    "    async fn wait_effect_boundary",
    "model plan routing",
    rewrite_plan,
)


def rewrite_safety(block: str) -> str:
    block = block.replace(
        "    fn safety_net_compaction_due(&self) -> bool {",
        "    fn safety_net_compaction_due(&self, operation_id: OperationId) -> bool {",
    )
    block = block.replace(".main_live()", ".live(operation_id)")
    block = block.replace(
        "self.main_lane_live().context_window",
        '''self.operation_lane_live(operation_id)
            .expect("resident operation has an owning lane")
            .context_window''',
    )
    block = block.replace(
        "self.main_lane_live().last_context_tokens.unwrap_or(0)",
        '''self.operation_lane_live(operation_id)
                    .expect("resident operation has an owning lane")
                    .last_context_tokens
                    .unwrap_or(0)''',
    )
    return block

text = rewrite_between(
    text,
    "    fn safety_net_compaction_due(&self) -> bool {",
    "    /// Commit the compaction effect intent",
    "compaction safety routing",
    rewrite_safety,
)


def rewrite_start_compaction(block: str) -> str:
    block = block.replace(
        "    async fn start_compaction(&mut self, instructions: Option<String>) -> bool {",
        "    async fn start_compaction(&mut self, operation_id: OperationId, instructions: Option<String>) -> bool {",
    )
    block = block.replace("self.current_model_config().await", "self.current_model_config(operation_id).await")
    block = block.replace(
        "let branch = self.main_branch_records();",
        '''let branch = self
            .operation_branch_records(operation_id)
            .expect("resident operation lane branch is complete");''',
    )
    block = block.replace(".main_active()", ".active(operation_id)")
    block = block.replace(".main_live_mut()", ".live_mut(operation_id)")
    block = block.replace("self.main_lane_live_mut()", "self.operation_lane_live_mut(operation_id).expect(\"resident operation has an owning lane\")")
    block = block.replace(
        "self.fail_operation_on_persistence(err).await;",
        "self.fail_operation_on_persistence_for(operation_id, err).await;",
    )
    return block

text = rewrite_between(
    text,
    "    async fn start_compaction(&mut self, instructions: Option<String>) -> bool {",
    "    /// Commit the model-step effect intent",
    "start compaction routing",
    rewrite_start_compaction,
)


def rewrite_budget(block: str) -> str:
    block = block.replace(
        "    async fn fail_budgeted(&mut self, dimension: &str) {",
        "    async fn fail_budgeted(&mut self, operation_id: OperationId, dimension: &str) {",
    )
    block = block.replace("self.main_active()", "self.active(operation_id)")
    block = block.replace(
        "self.fail_operation_on_persistence(err).await;",
        "self.fail_operation_on_persistence_for(operation_id, err).await;",
    )
    block = block.replace("self.emit_terminal_state(&applied.state);", "self.emit_terminal_state_for(operation_id, &applied.state);")
    block = block.replace("self.remove_main_operation();", "self.remove_operation(operation_id);")
    return block

text = rewrite_between(
    text,
    "    async fn fail_budgeted(&mut self, dimension: &str) {",
    "    async fn start_model_step",
    "budget routing",
    rewrite_budget,
)


def rewrite_model_step(block: str) -> str:
    block = block.replace(
        "    async fn start_model_step(&mut self) -> bool {",
        "    async fn start_model_step(&mut self, operation_id: OperationId) -> bool {",
    )
    block = block.replace(".main_live_mut()", ".live_mut(operation_id)")
    block = block.replace("self.fail_budgeted(\"model steps\").await", "self.fail_budgeted(operation_id, \"model steps\").await")
    block = block.replace("self.project_model_step_plan(&planning_manifest).await", "self.project_model_step_plan(operation_id, &planning_manifest).await")
    block = block.replace("self.current_model_config().await", "self.current_model_config(operation_id).await")
    block = block.replace(".main_active()", ".active(operation_id)")
    block = block.replace(
        "self.cache_expectation(&model, &prefix_fingerprint)",
        "self.cache_expectation(operation_id, &model, &prefix_fingerprint)",
    )
    block = block.replace(
        "self.fail_operation_on_persistence(err).await;",
        "self.fail_operation_on_persistence_for(operation_id, err).await;",
    )
    block = block.replace(
        "self.main_lane_live_mut().last_prefix_fingerprint = Some(prefix_fingerprint);",
        '''self.operation_lane_live_mut(operation_id)
            .expect("resident operation has an owning lane")
            .last_prefix_fingerprint = Some(prefix_fingerprint);''',
    )
    return block

text = rewrite_between(
    text,
    "    async fn start_model_step(&mut self) -> bool {",
    "    /// Commit a tool effect intent",
    "model step routing",
    rewrite_model_step,
)


def rewrite_admit(block: str) -> str:
    block = block.replace(
        "    async fn admit_next_tool(&mut self) -> bool {",
        "    async fn admit_next_tool(&mut self, operation_id: OperationId) -> bool {",
    )
    block = block.replace(".main_active()", ".active(operation_id)")
    block = block.replace(".main_live_mut()", ".live_mut(operation_id)")
    block = block.replace(
        "self.fail_operation_on_persistence(err).await;",
        "self.fail_operation_on_persistence_for(operation_id, err).await;",
    )
    block = block.replace("self.emit_terminal_state(&applied.state);", "self.emit_terminal_state_for(operation_id, &applied.state);")
    block = block.replace("self.remove_main_operation();", "self.remove_operation(operation_id);")
    block = block.replace(
        '''        self.commit_tool_admission(
            Transition::AdmitNextTool,''',
        '''        self.commit_tool_admission(
            operation_id,
            Transition::AdmitNextTool,''',
    )
    return block

text = rewrite_between(
    text,
    "    async fn admit_next_tool(&mut self) -> bool {",
    "    /// Apply a tool-admission transition",
    "tool admission routing",
    rewrite_admit,
)


def rewrite_commit_admission(block: str) -> str:
    block = block.replace(
        '''    async fn commit_tool_admission(
        &mut self,
        transition: Transition,''',
        '''    async fn commit_tool_admission(
        &mut self,
        operation_id: OperationId,
        transition: Transition,''',
    )
    block = block.replace(".main_active()", ".active(operation_id)")
    block = block.replace(".main_live_mut()", ".live_mut(operation_id)")
    block = block.replace(
        "self.fail_operation_on_persistence(err).await;",
        "self.fail_operation_on_persistence_for(operation_id, err).await;",
    )
    return block

text = rewrite_between(
    text,
    "    async fn commit_tool_admission(",
    "    /// Durable approval decision",
    "tool admission commit routing",
    rewrite_commit_admission,
)


def rewrite_approval(block: str) -> str:
    block = block.replace(
        '''        let Some(active) = self.main_active() else {
            return Err(CommandError::NoActiveOperation);
        };
        if active.machine.operation_id() != operation_id {
            return Err(CommandError::NotActive { operation_id });
        }''',
        '''        let Some(active) = self.active(operation_id) else {
            return if self.operations.is_empty() {
                Err(CommandError::NoActiveOperation)
            } else {
                Err(CommandError::NotActive { operation_id })
            };
        };''',
    )
    block = block.replace(".main_live_mut()", ".live_mut(operation_id)")
    block = block.replace(
        "self.fail_operation_on_persistence(err).await;",
        "self.fail_operation_on_persistence_for(operation_id, err).await;",
    )
    block = block.replace("self.emit_terminal_state(&state);", "self.emit_terminal_state_for(operation_id, &state);")
    block = block.replace("self.advance().await;", "self.advance(operation_id).await;")
    block = block.replace(
        '''            .commit_tool_admission(
                Transition::ApproveCall,''',
        '''            .commit_tool_admission(
                operation_id,
                Transition::ApproveCall,''',
    )
    return block

text = rewrite_between(
    text,
    "    async fn decide_approval(",
    "    async fn commit_transition",
    "approval routing",
    rewrite_approval,
)

runtime.write_text(text)

# Provider/tool settlement has stable operation identity at ingress; use it all
# the way through staging, lane context, telemetry, and continuation.
text = effects.read_text()


def rewrite_settle_model(block: str) -> str:
    block = block.replace(".main_active()", ".active(operation_id)")
    block = block.replace(".main_live_mut()", ".live_mut(operation_id)")
    block = block.replace("self.advance().await;", "self.advance(operation_id).await;")
    return block

text = rewrite_between(
    text,
    "    pub(crate) async fn settle_model_step(",
    "    /// Settle a context-overflow failure",
    "model settlement routing",
    rewrite_settle_model,
)


def rewrite_overflow(block: str) -> str:
    block = block.replace(".main_active()", ".active(operation_id)")
    block = block.replace("self.current_model_config().await", "self.current_model_config(operation_id).await")
    block = block.replace(
        "let branch = self.main_branch_records();",
        '''let branch = self
            .operation_branch_records(operation_id)
            .expect("resident operation lane branch is complete");''',
    )
    return block

text = rewrite_between(
    text,
    "    pub(crate) async fn settle_overflow_to_compaction",
    "    /// Settle a compaction step",
    "overflow routing",
    rewrite_overflow,
)


def rewrite_settle_compaction(block: str) -> str:
    block = block.replace(".main_active()", ".active(operation_id)")
    block = block.replace(".main_live_mut()", ".live_mut(operation_id)")
    block = block.replace("self.advance().await;", "self.advance(operation_id).await;")
    return block

text = rewrite_between(
    text,
    "    pub(crate) async fn settle_compaction(",
    "    pub(crate) async fn handle_tool_result",
    "compaction settlement routing",
    rewrite_settle_compaction,
)


def rewrite_tool_result(block: str) -> str:
    block = block.replace(".main_active()", ".active(operation_id)")
    block = block.replace("self.advance().await;", "self.advance(operation_id).await;")
    return block

text = rewrite_between(
    text,
    "    pub(crate) async fn handle_tool_result",
    "    /// A required commit failed",
    "tool settlement routing",
    rewrite_tool_result,
)

effects.write_text(text)

# Recovery remains intentionally main-admission-only for this checkpoint, but
# any continuation it starts must call the operation-addressed driver. Use the
# resident main identity only at that existing recovery boundary.
text = recovery.read_text()
text = text.replace(
    "                        self.advance().await;",
    '''                        if let Some(operation_id) = self.main_resident_id() {
                            self.advance(operation_id).await;
                        }''',
)
text = text.replace(
    "                self.advance().await;",
    '''                if let Some(operation_id) = self.main_resident_id() {
                    self.advance(operation_id).await;
                }''',
)
recovery.write_text(text)

# This slice must eliminate implicit continuation routing from runtime code.
for path in (runtime, effects, recovery):
    text = path.read_text()
    if "self.advance().await" in text:
        raise SystemExit(f"{path}: implicit advance call remains")

# Execution helpers converted in this slice must not consult main residency.
for signature in (
    "async fn advance(&mut self, mut operation_id: OperationId)",
    "async fn drain_queued(&mut self, operation_id: OperationId)",
    "async fn start_compaction(&mut self, operation_id: OperationId",
    "async fn start_model_step(&mut self, operation_id: OperationId)",
    "async fn admit_next_tool(&mut self, operation_id: OperationId)",
):
    if signature not in runtime.read_text():
        raise SystemExit(f"missing converted signature: {signature}")
