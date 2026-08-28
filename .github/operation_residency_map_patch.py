from pathlib import Path


def replace_once(path: Path, old: str, new: str, label: str) -> None:
    text = path.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    path.write_text(text.replace(old, new, 1))


runtime = Path("crates/ion-core/src/runtime/mod.rs")
effects = Path("crates/ion-core/src/runtime/effects.rs")
recovery = Path("crates/ion-core/src/runtime/recovery.rs")

replace_once(
    runtime,
    '''    draft_usage: Option<TokenUsage>,\n    last_context_tokens: Option<u64>,\n    last_prefix_fingerprint: Option<String>,\n    pending_compact: Option<Option<String>>,\n''',
    '''    draft_usage: Option<TokenUsage>,\n    pending_compact: Option<Option<String>>,\n''',
    "lane caches leave operation residency",
)
replace_once(
    runtime,
    '''struct OperationResidency {\n    operation_tool_calls: u32,\n    draft_text: String,\n    draft_thinking: String,\n    assistant_frame_seq: u64,\n    draft_calls: Vec<ToolCall>,\n    draft_usage: Option<TokenUsage>,\n    pending_compact: Option<Option<String>>,\n    overflow_retry_used: bool,\n    last_step_was_compaction: bool,\n    model_step: u64,\n    live_tools: Vec<PendingTool>,\n}\n''',
    '''struct OperationResidency {\n    operation_tool_calls: u32,\n    draft_text: String,\n    draft_thinking: String,\n    assistant_frame_seq: u64,\n    draft_calls: Vec<ToolCall>,\n    draft_usage: Option<TokenUsage>,\n    pending_compact: Option<Option<String>>,\n    overflow_retry_used: bool,\n    last_step_was_compaction: bool,\n    model_step: u64,\n    live_tools: Vec<PendingTool>,\n}\n\nstruct ResidentOperation {\n    lane_name: String,\n    active: ActiveOperation,\n    live: OperationResidency,\n}\n\nimpl ResidentOperation {\n    fn new(lane_name: impl Into<String>, active: ActiveOperation) -> Self {\n        Self {\n            lane_name: lane_name.into(),\n            active,\n            live: OperationResidency::default(),\n        }\n    }\n}\n''',
    "resident operation container",
)
replace_once(
    runtime,
    '''    next_entry_seq: u64,\n    operation: Option<ActiveOperation>,\n    operation_live: OperationResidency,\n    /// Most recently settled model-step usage, restored from the durable\n''',
    '''    next_entry_seq: u64,\n    /// Live operation residency keyed by durable operation identity. Public\n    /// commands still project `main`, but effects no longer depend on one\n    /// session-global operation slot.\n    operations: HashMap<OperationId, ResidentOperation>,\n    /// Main-lane context/cache observations intentionally survive operation\n    /// boundaries; these become lane-addressed with the lane command surface.\n    last_context_tokens: Option<u64>,\n    last_prefix_fingerprint: Option<String>,\n    /// Most recently settled model-step usage, restored from the durable\n''',
    "session runtime operation map",
)
replace_once(
    runtime,
    '''            next_entry_seq: 1,\n            operation: None,\n            operation_live: OperationResidency::default(),\n            latest_usage: None,\n''',
    '''            next_entry_seq: 1,\n            operations: HashMap::new(),\n            last_context_tokens: None,\n            last_prefix_fingerprint: None,\n            latest_usage: None,\n''',
    "operation map constructor",
)

# Lane-scoped cache facts were briefly co-located during the extraction.
for path in (runtime, effects, recovery):
    text = path.read_text()
    text = text.replace("self.operation_live.last_context_tokens", "self.last_context_tokens")
    text = text.replace("self.operation_live.last_prefix_fingerprint", "self.last_prefix_fingerprint")
    path.write_text(text)

helper_anchor = '''    fn main_lane_mut(&mut self) -> &mut crate::session::lane::Lane {\n        self.lanes\n            .get_mut(crate::session::lane::MAIN)\n            .expect("main lane exists while session runtime is live")\n    }\n'''
helpers = helper_anchor + '''\n    fn lane_resident_id(&self, lane_name: &str) -> Option<OperationId> {\n        let lane = self.lanes.get(lane_name)?;\n        lane.state.current_operation.or_else(|| {\n            self.operations\n                .iter()\n                .find_map(|(operation_id, resident)| {\n                    (resident.lane_name == lane_name).then_some(*operation_id)\n                })\n        })\n    }\n\n    fn main_resident_id(&self) -> Option<OperationId> {\n        self.lane_resident_id(crate::session::lane::MAIN)\n    }\n\n    fn main_resident(&self) -> Option<&ResidentOperation> {\n        let operation_id = self.main_resident_id()?;\n        self.operations.get(&operation_id)\n    }\n\n    fn main_resident_mut(&mut self) -> Option<&mut ResidentOperation> {\n        let operation_id = self.main_resident_id()?;\n        self.operations.get_mut(&operation_id)\n    }\n\n    fn main_active(&self) -> Option<&ActiveOperation> {\n        self.main_resident().map(|resident| &resident.active)\n    }\n\n    fn main_active_mut(&mut self) -> Option<&mut ActiveOperation> {\n        self.main_resident_mut().map(|resident| &mut resident.active)\n    }\n\n    fn main_live(&self) -> Option<&OperationResidency> {\n        self.main_resident().map(|resident| &resident.live)\n    }\n\n    fn main_live_mut(&mut self) -> Option<&mut OperationResidency> {\n        self.main_resident_mut().map(|resident| &mut resident.live)\n    }\n\n    fn install_active(&mut self, active: ActiveOperation) {\n        let operation_id = active.machine.operation_id();\n        let resident = self\n            .operations\n            .get_mut(&operation_id)\n            .expect("installed operation residency exists");\n        resident.active = active;\n    }\n\n    fn remove_main_operation(&mut self) -> Option<ResidentOperation> {\n        let operation_id = self.main_resident_id()?;\n        self.operations.remove(&operation_id)\n    }\n'''
replace_once(runtime, helper_anchor, helpers, "operation residency helpers")

# Mechanical projection of the old singleton active slot through `main`.
for path in (runtime, effects, recovery):
    text = path.read_text()
    text = text.replace("self.operation.as_ref()", "self.main_active()")
    text = text.replace("self.operation.as_mut()", "self.main_active_mut()")
    text = text.replace("self.operation.is_some()", "self.main_active().is_some()")
    text = text.replace("self.operation.is_none()", "self.main_active().is_none()")
    text = text.replace("self.operation.clone()", "self.main_active().cloned()")
    text = text.replace("match &self.operation", "match self.main_active()")
    text = text.replace("if let Some(active) = &self.operation", "if let Some(active) = self.main_active()")
    text = text.replace("if let Some(active) = &mut self.operation", "if let Some(active) = self.main_active_mut()")
    text = text.replace("self.operation = Some(staged);", "self.install_active(staged);")
    text = text.replace("self.operation.take();", "self.remove_main_operation();")
    path.write_text(text)

# Ephemeral state now lives beside the operation core in the same residency.
# Use mutable access in mutation-heavy runtime paths; immutable methods are
# corrected below explicitly.
for path in (runtime, effects, recovery):
    text = path.read_text().replace(
        "self.operation_live.",
        'self.main_live_mut().expect("main operation residency exists").',
    )
    path.write_text(text)

# Immutable main-lane cache/snapshot helpers cannot borrow residency mutably.
text = runtime.read_text()
text = text.replace(
    'self.main_live_mut().expect("main operation residency exists").last_step_was_compaction',
    'self.main_live().expect("main operation residency exists").last_step_was_compaction',
)
text = text.replace(
    'self.main_live_mut().expect("main operation residency exists").draft_text.clone()',
    'self.main_live().expect("main operation residency exists").draft_text.clone()',
)
text = text.replace(
    'self.main_live_mut().expect("main operation residency exists").draft_thinking.clone()',
    'self.main_live().expect("main operation residency exists").draft_thinking.clone()',
)
text = text.replace(
    'self.main_live_mut().expect("main operation residency exists").live_tools.clone()',
    'self.main_live().expect("main operation residency exists").live_tools.clone()',
)
runtime.write_text(text)

# Restore builds the resident privately before publication so frame state and
# lane ownership appear together at one in-memory publication point.
text = runtime.read_text()
old = '''            if matches!(payload.state, OperationState::AssistantEffectPending)\n                && let Some(effect_id) = payload.open_effect.as_ref().map(|effect| effect.id)\n                && let Some(frame) = assistant_frames.iter().find(|frame| {\n                    frame.operation_id == operation.id && frame.effect_id == effect_id\n                })\n            {\n                self.main_live_mut().expect("main operation residency exists").draft_text = frame.text.clone();\n                self.main_live_mut().expect("main operation residency exists").draft_thinking = frame.thinking.clone();\n                self.main_live_mut().expect("main operation residency exists").assistant_frame_seq = frame.frame_seq;\n            }\n            if self.operation.replace(active).is_some() {\n'''
new = '''            let mut resident = ResidentOperation::new(operation.lane_name.clone(), active);\n            if matches!(payload.state, OperationState::AssistantEffectPending)\n                && let Some(effect_id) = payload.open_effect.as_ref().map(|effect| effect.id)\n                && let Some(frame) = assistant_frames.iter().find(|frame| {\n                    frame.operation_id == operation.id && frame.effect_id == effect_id\n                })\n            {\n                resident.live.draft_text = frame.text.clone();\n                resident.live.draft_thinking = frame.thinking.clone();\n                resident.live.assistant_frame_seq = frame.frame_seq;\n            }\n            if self.operations.insert(operation.id, resident).is_some() {\n'''
if old not in text:
    raise SystemExit("restore resident publication block mismatch")
text = text.replace(old, new, 1)

# New acceptance publishes the residency once; its live state starts total and
# empty instead of resetting a parallel session-global object.
old = '''    fn start_active(&mut self, active: ActiveOperation) {\n        let operation_id = active.machine.operation_id();\n        let prompt = active.machine.prompt().to_owned();\n        self.operation = Some(active);\n        self.main_live_mut().expect("main operation residency exists").draft_text.clear();\n        self.main_live_mut().expect("main operation residency exists").draft_thinking.clear();\n        self.main_live_mut().expect("main operation residency exists").assistant_frame_seq = 0;\n        self.main_live_mut().expect("main operation residency exists").draft_calls.clear();\n        self.main_live_mut().expect("main operation residency exists").draft_usage = None;\n        self.main_live_mut().expect("main operation residency exists").live_tools.clear();\n        self.main_live_mut().expect("main operation residency exists").pending_compact = None;\n        self.main_live_mut().expect("main operation residency exists").overflow_retry_used = false;\n        self.main_live_mut().expect("main operation residency exists").last_step_was_compaction = false;\n        self.main_live_mut().expect("main operation residency exists").model_step = 0;\n        self.main_live_mut().expect("main operation residency exists").operation_tool_calls = 0;\n'''
new = '''    fn start_active(&mut self, active: ActiveOperation) {\n        let operation_id = active.machine.operation_id();\n        let prompt = active.machine.prompt().to_owned();\n        let previous = self.operations.insert(\n            operation_id,\n            ResidentOperation::new(crate::session::lane::MAIN, active),\n        );\n        debug_assert!(previous.is_none(), "operation residency identity is unique");\n'''
if old not in text:
    raise SystemExit("start_active block mismatch")
text = text.replace(old, new, 1)
runtime.write_text(text)

# A few direct singleton forms are intentionally explicit rather than hidden
# behind broad textual replacements.
for path in (runtime, effects, recovery):
    text = path.read_text()
    if "self.operation" in text:
        # Remaining occurrences must be new map/helper names, not the removed
        # singleton field. Catch accidental old syntax while allowing
        # `self.operations`.
        bad = [line for line in text.splitlines() if "self.operation" in line and "self.operations" not in line]
        if bad:
            raise SystemExit(f"{path}: leftover singleton operation syntax: {bad[:8]}")
