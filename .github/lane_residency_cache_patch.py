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
recovery = Path("crates/ion-core/src/runtime/recovery.rs")

# A resident lane combines one authoritative durable lane projection with
# ephemeral observations that are meaningful only relative to that lane's
# context/configuration. These are caches/telemetry, not a second lane state.
replace_once(
    runtime,
    '''impl ResidentOperation {
    fn new(lane_name: impl Into<String>, active: ActiveOperation) -> Self {
        Self {
            lane_name: lane_name.into(),
            active,
            live: OperationResidency::default(),
        }
    }
}

/// The single-writer owner''',
    '''impl ResidentOperation {
    fn new(lane_name: impl Into<String>, active: ActiveOperation) -> Self {
        Self {
            lane_name: lane_name.into(),
            active,
            live: OperationResidency::default(),
        }
    }
}

#[derive(Debug, Default)]
struct LaneResidency {
    last_context_tokens: Option<u64>,
    last_prefix_fingerprint: Option<String>,
    latest_usage: Option<TokenUsage>,
    context_window: Option<u64>,
    model_capabilities: Option<(String, ModelCapabilities)>,
}

struct ResidentLane {
    durable: crate::session::lane::Lane,
    live: LaneResidency,
}

impl ResidentLane {
    fn new(durable: crate::session::lane::Lane) -> Self {
        Self {
            durable,
            live: LaneResidency::default(),
        }
    }
}

/// The single-writer owner''',
    "resident lane type",
)
replace_once(
    runtime,
    '''    /// Durable total lane projections. Public commands still address `main`
    /// until the next multi-lane residency checkpoint.
    lanes: BTreeMap<String, crate::session::lane::Lane>,
    /// Next session-global durable entry sequence.
    next_entry_seq: u64,
    /// Live operation residency keyed by durable operation identity. Public
    /// commands still project `main`, but effects no longer depend on one
    /// session-global operation slot.
    operations: HashMap<OperationId, ResidentOperation>,
    /// Main-lane context/cache observations intentionally survive operation
    /// boundaries; these become lane-addressed with the lane command surface.
    last_context_tokens: Option<u64>,
    last_prefix_fingerprint: Option<String>,
    /// Most recently settled model-step usage, restored from the durable
    /// ledger and exposed through snapshots for frontend resynchronization.
    latest_usage: Option<TokenUsage>,
    /// Cached model context window (14.8); fetched from the adapter
    /// once, on first use.
    context_window: Option<u64>,
    /// Cached model capability metadata, keyed by the selected model.
    model_capabilities: Option<(String, ModelCapabilities)>,
''',
    '''    /// Durable lane projections paired with lane-relative live cache and
    /// telemetry observations. Public commands still address `main` until the
    /// lane command surface lands.
    lanes: BTreeMap<String, ResidentLane>,
    /// Next session-global durable entry sequence.
    next_entry_seq: u64,
    /// Live operation residency keyed by durable operation identity.
    operations: HashMap<OperationId, ResidentOperation>,
''',
    "session fields use resident lanes",
)
replace_once(
    runtime,
    '''        lanes.insert(
            crate::session::lane::MAIN.to_owned(),
            crate::session::lane::Lane {
                name: crate::session::lane::MAIN.to_owned(),
                state: crate::session::lane::State {
                    leaf: None,
                    current_operation: None,
                    pending_next_run: None,
                },
                config: crate::session::lane::Config::new(initial_model_ref),
            },
        );
''',
    '''        lanes.insert(
            crate::session::lane::MAIN.to_owned(),
            ResidentLane::new(crate::session::lane::Lane {
                name: crate::session::lane::MAIN.to_owned(),
                state: crate::session::lane::State {
                    leaf: None,
                    current_operation: None,
                    pending_next_run: None,
                },
                config: crate::session::lane::Config::new(initial_model_ref),
            }),
        );
''',
    "initialize resident main lane",
)
replace_once(
    runtime,
    '''            lanes,
            next_entry_seq: 1,
            operations: HashMap::new(),
            last_context_tokens: None,
            last_prefix_fingerprint: None,
            latest_usage: None,
            context_window: None,
            model_capabilities: None,
            suspended_operations: Vec::new(),
''',
    '''            lanes,
            next_entry_seq: 1,
            operations: HashMap::new(),
            suspended_operations: Vec::new(),
''',
    "remove session-global lane observations",
)

# Keep loaded latest usage as a temporary local until durable lanes are installed;
# today's loader returns the main projection. Per-lane recovery follows before
# non-main execution is enabled.
regex_once(
    runtime,
    r'''    fn restore_from\(&mut self, loaded: LoadedSession\) \{\n        self\.latest_usage = loaded\.latest_usage\.map\(\|usage\| TokenUsage \{.*?\n        \}\);\n        self\.last_context_tokens = self\.latest_usage\.map\(\|usage\| \{.*?\n        \}\);\n        let assistant_frames = loaded\.assistant_frames;''',
    '''    fn restore_from(&mut self, loaded: LoadedSession) {
        let latest_usage = loaded.latest_usage.map(|usage| TokenUsage {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_read: usage.cache_read_tokens,
            cache_write: usage.cache_write_tokens,
        });
        let last_context_tokens = latest_usage.map(|usage| {
            usage
                .input
                .saturating_add(usage.output)
                .saturating_add(usage.cache_read)
                .saturating_add(usage.cache_write)
        });
        let assistant_frames = loaded.assistant_frames;''',
    "restore usage locals",
)
replace_once(
    runtime,
    '''        self.lanes = loaded
            .lanes
            .into_iter()
            .map(|lane| (lane.name.clone(), lane))
            .collect();
        if !self.lanes.contains_key(crate::session::lane::MAIN) {
''',
    '''        self.lanes = loaded
            .lanes
            .into_iter()
            .map(|lane| (lane.name.clone(), ResidentLane::new(lane)))
            .collect();
        if !self.lanes.contains_key(crate::session::lane::MAIN) {
''',
    "restore resident lanes",
)
replace_once(
    runtime,
    '''            return;
        }
        let Some(main_branch) = self.lane_branch_records(crate::session::lane::MAIN) else {
''',
    '''            return;
        }
        {
            let live = &mut self
                .lanes
                .get_mut(crate::session::lane::MAIN)
                .expect("checked main lane")
                .live;
            live.latest_usage = latest_usage;
            live.last_context_tokens = last_context_tokens;
        }
        let Some(main_branch) = self.lane_branch_records(crate::session::lane::MAIN) else {
''',
    "restore main lane observations",
)

# Durable lane accessors preserve existing callers while live lane state gets a
# separate explicit path.
replace_once(
    runtime,
    '''    fn main_lane(&self) -> &crate::session::lane::Lane {
        self.lanes
            .get(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
    }

    fn main_lane_mut(&mut self) -> &mut crate::session::lane::Lane {
        self.lanes
            .get_mut(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
    }
''',
    '''    fn main_lane(&self) -> &crate::session::lane::Lane {
        &self
            .lanes
            .get(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
            .durable
    }

    fn main_lane_mut(&mut self) -> &mut crate::session::lane::Lane {
        &mut self
            .lanes
            .get_mut(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
            .durable
    }

    fn lane_live(&self, lane_name: &str) -> Option<&LaneResidency> {
        self.lanes.get(lane_name).map(|lane| &lane.live)
    }

    fn lane_live_mut(&mut self, lane_name: &str) -> Option<&mut LaneResidency> {
        self.lanes.get_mut(lane_name).map(|lane| &mut lane.live)
    }

    fn operation_lane_live(&self, operation_id: OperationId) -> Option<&LaneResidency> {
        self.lane_live(self.operation_lane_name(operation_id)?)
    }

    fn operation_lane_live_mut(
        &mut self,
        operation_id: OperationId,
    ) -> Option<&mut LaneResidency> {
        let lane_name = self.operation_lane_name(operation_id)?.to_owned();
        self.lane_live_mut(&lane_name)
    }

    fn main_lane_live(&self) -> &LaneResidency {
        self.lane_live(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
    }

    fn main_lane_live_mut(&mut self) -> &mut LaneResidency {
        self.lane_live_mut(crate::session::lane::MAIN)
            .expect("main lane exists while session runtime is live")
    }
''',
    "durable and live lane accessors",
)

# ResidentLane wraps durable state, so direct map traversal must name the layer.
text = runtime.read_text()
text = text.replace("lane.state.current_operation", "lane.durable.state.current_operation")
text = text.replace("let mut cursor = lane.state.leaf;", "let mut cursor = lane.durable.state.leaf;")
text = text.replace("            .state\n            .leaf;", "            .durable\n            .state\n            .leaf;")
text = text.replace("        let lane = self\n            .lanes\n            .get_mut(&lane_name)\n            .expect(\"operation lane exists after durable commit\");\n", "        let lane = &mut self\n            .lanes\n            .get_mut(&lane_name)\n            .expect(\"operation lane exists after durable commit\")\n            .durable;\n")
runtime.write_text(text)

# Main command/config paths now mutate or read main lane-local observations.
text = runtime.read_text()
text = text.replace("        self.context_window = None;", "        self.main_lane_live_mut().context_window = None;")
text = text.replace("        self.model_capabilities = None;", "        self.main_lane_live_mut().model_capabilities = None;")
text = text.replace("        self.last_prefix_fingerprint = None;", "        self.main_lane_live_mut().last_prefix_fingerprint = None;")

# Replace the model-config helper as one block to avoid borrow-across-await.
pattern = r'''    async fn current_model_config\(&mut self\) -> ModelConfig \{.*?\n    \}\n\n    fn current_context_manifest'''
replacement = '''    async fn current_model_config(&mut self) -> ModelConfig {
        let selected_model_ref = self.main_model_ref().to_owned();
        let capabilities = match self.main_lane_live().model_capabilities.as_ref() {
            Some((model_ref, capabilities)) if model_ref == &selected_model_ref => *capabilities,
            _ => {
                let capabilities = self.provider.capabilities_for(&selected_model_ref).await;
                self.main_lane_live_mut().model_capabilities =
                    Some((selected_model_ref.clone(), capabilities));
                capabilities
            }
        };
        let context_window = match self.main_lane_live().context_window {
            Some(window) => Some(window),
            None => {
                let window = self.provider.context_window_for(&selected_model_ref).await;
                self.main_lane_live_mut().context_window = window;
                window
            }
        };
        ModelConfig {
            model_ref: selected_model_ref,
            context_window,
            capabilities,
        }
    }

    fn current_context_manifest'''
text, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
if count != 1:
    raise SystemExit(f"model config lane cache: expected 1 match, found {count}")
text = text.replace(
    "        match self.last_prefix_fingerprint.as_deref() {",
    "        match self.main_lane_live().last_prefix_fingerprint.as_deref() {",
)
text = text.replace(
    "        match self.context_window {",
    "        match self.main_lane_live().context_window {",
)
text = text.replace(
    "                self.last_context_tokens.unwrap_or(0) > window.saturating_sub(RESERVE_TOKENS)",
    "                self.main_lane_live().last_context_tokens.unwrap_or(0)\n                    > window.saturating_sub(RESERVE_TOKENS)",
)
text = text.replace(
    "        self.last_prefix_fingerprint = Some(prefix_fingerprint);",
    "        self.main_lane_live_mut().last_prefix_fingerprint = Some(prefix_fingerprint);",
)
text = text.replace(
    "            latest_usage: self.latest_usage,",
    "            latest_usage: self.main_lane_live().latest_usage,",
)
runtime.write_text(text)

# Effect telemetry is operation-lane relative even before non-main execution is
# admitted publicly.
text = effects.read_text()
text = text.replace(
    '''                self.last_context_tokens =
                    Some(usage.input + usage.output + usage.cache_read + usage.cache_write);''',
    '''                self.operation_lane_live_mut(operation_id)
                    .expect("resident operation has an owning lane")
                    .last_context_tokens =
                    Some(usage.input + usage.output + usage.cache_read + usage.cache_write);''',
)
text = text.replace(
    '''        if let Some(usage) = settled_usage {
            self.latest_usage = Some(usage);
        }''',
    '''        if let Some(usage) = settled_usage {
            self.operation_lane_live_mut(operation_id)
                .expect("resident operation has an owning lane")
                .latest_usage = Some(usage);
        }''',
)
text = text.replace(
    "        self.last_prefix_fingerprint = None;",
    '''        self.operation_lane_live_mut(operation_id)
            .expect("resident operation has an owning lane")
            .last_prefix_fingerprint = None;''',
)
effects.write_text(text)

# Recovery is still main-only by invariant, but its cache restoration must now
# explicitly write the owning lane rather than removed session-global fields.
text = recovery.read_text()
text = text.replace(
    "                self.last_prefix_fingerprint = Some(persisted_prefix_fingerprint);",
    '''                self.operation_lane_live_mut(operation_id)
                    .expect("recovered operation has an owning lane")
                    .last_prefix_fingerprint = Some(persisted_prefix_fingerprint);''',
)
recovery.write_text(text)

# No removed session-global observation may survive.
for path in (runtime, effects, recovery):
    text = path.read_text()
    for field in (
        "self.last_context_tokens",
        "self.last_prefix_fingerprint",
        "self.latest_usage",
        "self.context_window",
        "self.model_capabilities",
    ):
        if field in text:
            raise SystemExit(f"{path}: leftover session-global lane observation {field}")
