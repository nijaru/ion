from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"anchor missing in {path}: {old[:160]!r}")
    p.write_text(text.replace(old, new, 1))


def insert_after(path: str, anchor: str, addition: str) -> None:
    replace(path, anchor, anchor + addition)


# Tool selection is a structural tool-boundary concept, not runtime name logic.
replace(
    "crates/ion-core/src/tool/mod.rs",
    "use std::collections::HashMap;\nuse std::collections::VecDeque;",
    "use std::collections::{BTreeSet, HashMap, VecDeque};",
)
insert_after(
    "crates/ion-core/src/tool/mod.rs",
    '''pub struct ToolSpec {\n    pub name: String,\n    pub description: String,\n    pub input_schema: Value,\n}\n''',
    '''\n/// Durable structural selection applied to a lane's future tool snapshots.\n/// Tool-name semantics live at the tool boundary; runtime/store code only\n/// composes and validates selections.\n#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]\npub(crate) enum ToolSelection {\n    All,\n    Only(BTreeSet<String>),\n}\n\nimpl ToolSelection {\n    #[must_use]\n    pub(crate) const fn all() -> Self {\n        Self::All\n    }\n\n    #[must_use]\n    pub(crate) fn read_only() -> Self {\n        Self::Only(BTreeSet::from([\n            "find".to_owned(),\n            "read".to_owned(),\n            "search".to_owned(),\n        ]))\n    }\n\n    #[must_use]\n    pub(crate) fn narrowed_by(&self, limit: &Self) -> Self {\n        match (self, limit) {\n            (Self::All, other) => other.clone(),\n            (other, Self::All) => other.clone(),\n            (Self::Only(current), Self::Only(limit)) => {\n                Self::Only(current.intersection(limit).cloned().collect())\n            }\n        }\n    }\n\n    #[must_use]\n    pub(crate) fn is_subset_of(&self, parent: &Self) -> bool {\n        match (self, parent) {\n            (_, Self::All) => true,\n            (Self::All, Self::Only(_)) => false,\n            (Self::Only(child), Self::Only(parent)) => child.is_subset(parent),\n        }\n    }\n\n    fn allows(&self, name: &str) -> bool {\n        match self {\n            Self::All => true,\n            Self::Only(names) => names.contains(name),\n        }\n    }\n}\n''',
)
insert_after(
    "crates/ion-core/src/tool/mod.rs",
    '''    pub fn cwd(&self) -> &Path {\n        &self.cwd\n    }\n''',
    '''\n    /// Structurally narrow this immutable registry. Missing tools stay absent;\n    /// selection never manufactures an executor.\n    #[must_use]\n    pub(crate) fn selected(&self, selection: &ToolSelection) -> Self {\n        let mut entries = self.entries.as_ref().clone();\n        entries.retain(|name, _| selection.allows(name));\n        Self {\n            cwd: Arc::clone(&self.cwd),\n            entries: Arc::new(entries),\n        }\n    }\n\n    /// Reconstruct only executors that exactly match a persisted capability\n    /// snapshot. A same-name replacement with a new spec/generation is absent\n    /// rather than silently retargeting recovered work.\n    #[must_use]\n    pub(crate) fn available_for_snapshot(\n        &self,\n        snapshot: &crate::context::CapabilitySnapshot,\n    ) -> Self {\n        let mut entries = self.entries.as_ref().clone();\n        entries.retain(|name, entry| {\n            snapshot\n                .tools\n                .iter()\n                .position(|tool| tool.name == *name)\n                .is_some_and(|index| {\n                    snapshot.tools[index] == entry.spec\n                        && snapshot\n                            .identities\n                            .get(index)\n                            .is_some_and(|identity| identity == &entry.capability_id)\n                })\n        });\n        Self {\n            cwd: Arc::clone(&self.cwd),\n            entries: Arc::new(entries),\n        }\n    }\n''',
)

# Lane config owns future execution capability selection alongside model config.
replace(
    "crates/ion-core/src/session/lane.rs",
    "use crate::ids::{EntryId, OperationId};",
    "use crate::ids::{EntryId, OperationId};\nuse crate::tool::ToolSelection;",
)
replace(
    "crates/ion-core/src/session/lane.rs",
    '''pub(crate) struct Config {\n    pub(crate) model_ref: String,\n}\n\nimpl Config {\n    pub(crate) fn new(model_ref: impl Into<String>) -> Self {\n        Self {\n            model_ref: model_ref.into(),\n        }\n    }\n}\n''',
    '''pub(crate) struct Config {\n    pub(crate) model_ref: String,\n    pub(crate) tools: ToolSelection,\n}\n\nimpl Config {\n    pub(crate) fn new(model_ref: impl Into<String>) -> Self {\n        Self {\n            model_ref: model_ref.into(),\n            tools: ToolSelection::all(),\n        }\n    }\n\n    pub(crate) fn with_tools(model_ref: impl Into<String>, tools: ToolSelection) -> Self {\n        Self {\n            model_ref: model_ref.into(),\n            tools,\n        }\n    }\n}\n''',
)

# Serialized lane config changed; pre-1.0 policy archives older development DBs.
replace(
    "crates/ion-core/src/store/schema.rs",
    "const SCHEMA_VERSION: i64 = 19;",
    "const SCHEMA_VERSION: i64 = 20;",
)

# Preserve the public store convenience API while giving the runtime a total-config path.
replace(
    "crates/ion-core/src/store/mod.rs",
    '''    pub async fn create_lane(\n        &self,\n        session_id: SessionId,\n        lane_name: impl Into<String>,\n        source_leaf: Option<EntryId>,\n        model_ref: impl Into<String>,\n    ) -> Result<(), StoreError> {\n        let lane = crate::session::lane::Lane {\n            name: lane_name.into(),\n            state: crate::session::lane::State {\n                leaf: source_leaf,\n                current_operation: None,\n                pending_next_run: None,\n            },\n            config: crate::session::lane::Config::new(model_ref),\n        };\n        self.request(|reply| StoreCommand::CreateLane {\n            session_id,\n            lane,\n            reply,\n        })\n        .await\n    }\n''',
    '''    pub async fn create_lane(\n        &self,\n        session_id: SessionId,\n        lane_name: impl Into<String>,\n        source_leaf: Option<EntryId>,\n        model_ref: impl Into<String>,\n    ) -> Result<(), StoreError> {\n        self.create_lane_with_config(\n            session_id,\n            lane_name,\n            source_leaf,\n            crate::session::lane::Config::new(model_ref),\n        )\n        .await\n    }\n\n    pub(crate) async fn create_lane_with_config(\n        &self,\n        session_id: SessionId,\n        lane_name: impl Into<String>,\n        source_leaf: Option<EntryId>,\n        config: crate::session::lane::Config,\n    ) -> Result<(), StoreError> {\n        let lane = crate::session::lane::Lane {\n            name: lane_name.into(),\n            state: crate::session::lane::State {\n                leaf: source_leaf,\n                current_operation: None,\n                pending_next_run: None,\n            },\n            config,\n        };\n        self.request(|reply| StoreCommand::CreateLane {\n            session_id,\n            lane,\n            reply,\n        })\n        .await\n    }\n''',
)
replace(
    "crates/ion-core/src/store/mod.rs",
    '''        source_leaf: Option<EntryId>,\n        model_ref: impl Into<String>,\n    ) -> Result<(), StoreError> {\n        let lane = crate::session::lane::Lane {\n            name: lane_name.into(),\n            state: crate::session::lane::State {\n                leaf: source_leaf,\n                current_operation: None,\n                pending_next_run: None,\n            },\n            config: crate::session::lane::Config::new(model_ref),\n        };\n''',
    '''        source_leaf: Option<EntryId>,\n        config: crate::session::lane::Config,\n    ) -> Result<(), StoreError> {\n        let lane = crate::session::lane::Lane {\n            name: lane_name.into(),\n            state: crate::session::lane::State {\n                leaf: source_leaf,\n                current_operation: None,\n                pending_next_run: None,\n            },\n            config,\n        };\n''',
)

# Store independently enforces that atomic agent+lane publication cannot escalate
# its control parent's durable lane capabilities/model selection.
replace(
    "crates/ion-core/src/store/sql.rs",
    '''    let (parent_family, parent_session, parent_leaf): (String, String, Option<String>) = tx\n        .query_row(\n            "SELECT a.family_session_id, a.session_id, lane.leaf_id\n             FROM agents a\n             JOIN lanes lane ON lane.session_id = a.session_id AND lane.name = a.lane_name\n             WHERE a.id = ?1",\n            [&parent],\n            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),\n        )?;\n    let source_leaf = lane.state.leaf.map(|id| id.as_uuid().to_string());\n    if parent_family != session || parent_session != session || parent_leaf != source_leaf {\n        return Err(rusqlite::Error::InvalidParameterName(\n            "agent lane must anchor at its control parent's current lane leaf".to_owned(),\n        ));\n    }\n''',
    '''    let (parent_family, parent_session, parent_leaf, parent_config): (\n        String,\n        String,\n        Option<String>,\n        String,\n    ) = tx.query_row(\n        "SELECT a.family_session_id, a.session_id, lane.leaf_id, lane.config\n         FROM agents a\n         JOIN lanes lane ON lane.session_id = a.session_id AND lane.name = a.lane_name\n         WHERE a.id = ?1",\n        [&parent],\n        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),\n    )?;\n    let source_leaf = lane.state.leaf.map(|id| id.as_uuid().to_string());\n    if parent_family != session || parent_session != session || parent_leaf != source_leaf {\n        return Err(rusqlite::Error::InvalidParameterName(\n            "agent lane must anchor at its control parent's current lane leaf".to_owned(),\n        ));\n    }\n    let parent_config: crate::session::lane::Config = serde_json::from_str(&parent_config)\n        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(err.into()))?;\n    if lane.config.model_ref != parent_config.model_ref\n        || !lane.config.tools.is_subset_of(&parent_config.tools)\n    {\n        return Err(rusqlite::Error::InvalidParameterName(\n            "agent lane configuration may narrow but not escalate its control parent".to_owned(),\n        ));\n    }\n''',
)
replace(
    "crates/ion-core/src/store/sql.rs",
    '''        if capability_snapshot.id != checkpoint.capability_snapshot_id {\n            return Err(StoreError::Sqlite(\n                "checkpoint capability snapshot id mismatch".to_owned(),\n            ));\n        }\n''',
    '''        if capability_snapshot.id != checkpoint.capability_snapshot_id\n            || !capability_snapshot.is_consistent()\n        {\n            return Err(StoreError::Sqlite(\n                "checkpoint capability snapshot is inconsistent".to_owned(),\n            ));\n        }\n''',
)

# Runtime derives every executable snapshot from the owning lane selection.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''    RecoveryClass, ToolCall, ToolCatalog, ToolProgress, ToolRegistry, ToolResult, ToolSpec,\n''',
    '''    RecoveryClass, ToolCall, ToolCatalog, ToolProgress, ToolRegistry, ToolResult, ToolSelection,\n    ToolSpec,\n''',
)
insert_after(
    "crates/ion-core/src/runtime/mod.rs",
    '''    fn main_lane(&self) -> &crate::session::lane::Lane {\n        self.lane(crate::session::lane::MAIN)\n            .expect("main lane exists while session runtime is live")\n    }\n''',
    '''\n    fn tool_registry_for_lane(&self, lane_name: &str) -> Option<ToolRegistry> {\n        let selection = &self.lane(lane_name)?.config.tools;\n        Some(self.tools.snapshot().selected(selection))\n    }\n\n    fn tool_registry_for_operation(&self, operation_id: OperationId) -> Option<ToolRegistry> {\n        let lane_name = self.operation_lane_name(operation_id)?;\n        self.tool_registry_for_lane(lane_name)\n    }\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''            let steers: Vec<InboxItem> = operation\n                .pending_inbox\n                .iter()\n                .filter(|item| item.kind == InboxKind::Steer)\n                .map(|item| InboxItem {\n                    kind: item.kind.clone(),\n                    text: item.text.clone(),\n                })\n                .collect();\n''',
    '''            let pending_inputs: Vec<InboxItem> = operation\n                .pending_inbox\n                .iter()\n                .filter(|item| !matches!(&item.kind, InboxKind::Prompt))\n                .map(|item| InboxItem {\n                    kind: item.kind.clone(),\n                    text: item.text.clone(),\n                })\n                .collect();\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''                payload.cancel_requested,\n                steers,\n            );\n''',
    '''                payload.cancel_requested,\n                pending_inputs,\n            );\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''            let active = ActiveOperation {\n                machine,\n                capability_snapshot: operation.capability_snapshot.clone(),\n                tool_registry: self.tools.snapshot(),\n''',
    '''            let tool_registry = self\n                .tool_registry_for_lane(&operation.lane_name)\n                .expect("loaded operation origin lane exists")\n                .available_for_snapshot(&operation.capability_snapshot);\n            let active = ActiveOperation {\n                machine,\n                capability_snapshot: operation.capability_snapshot.clone(),\n                tool_registry,\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let source_leaf = self.main_lane().state.leaf;\n        let model_ref = self.main_model_ref().to_owned();\n        self.store\n            .create_lane(\n                self.session_id,\n                lane_name.clone(),\n                source_leaf,\n                model_ref.clone(),\n            )\n''',
    '''        let source_leaf = self.main_lane().state.leaf;\n        let config = self.main_lane().config.clone();\n        self.store\n            .create_lane_with_config(\n                self.session_id,\n                lane_name.clone(),\n                source_leaf,\n                config.clone(),\n            )\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''                config: crate::session::lane::Config::new(model_ref),\n''',
    '''                config,\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let source_leaf = source.state.leaf;\n        let model_ref = source.config.model_ref.clone();\n        let lane_name = agent_id.to_string();\n''',
    '''        let source_leaf = source.state.leaf;\n        let mut config = source.config.clone();\n        config.tools = config.tools.narrowed_by(&ToolSelection::read_only());\n        let lane_name = agent_id.to_string();\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''                lane_name.clone(),\n                source_leaf,\n                model_ref.clone(),\n            )\n''',
    '''                lane_name.clone(),\n                source_leaf,\n                config.clone(),\n            )\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''                config: crate::session::lane::Config::new(model_ref),\n''',
    '''                config,\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let operation_id = OperationId::generate();\n        let tool_registry = self.tools.snapshot();\n''',
    '''        let operation_id = OperationId::generate();\n        let tool_registry = self\n            .tool_registry_for_lane(lane_name)\n            .expect("operation acceptance lane exists");\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        self.store\n            .set_lane_config(\n                self.session_id,\n                &lane_name,\n                crate::session::lane::Config::new(model_ref.clone()),\n            )\n            .await\n            .map_err(persistence_command_error)?;\n        self.lane_mut(&lane_name)\n            .expect("configured lane remains resident")\n            .config\n            .model_ref = model_ref;\n''',
    '''        let mut config = self\n            .lane(&lane_name)\n            .expect("checked lane")\n            .config\n            .clone();\n        config.model_ref = model_ref;\n        self.store\n            .set_lane_config(self.session_id, &lane_name, config.clone())\n            .await\n            .map_err(persistence_command_error)?;\n        self.lane_mut(&lane_name)\n            .expect("configured lane remains resident")\n            .config = config;\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let mut staged = self\n            .main_active()\n            .cloned()\n            .expect("budget fail needs an operation");\n''',
    '''        let mut staged = self\n            .active(operation_id)\n            .cloned()\n            .expect("budget fail needs the addressed operation");\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let step_registry = self.tools.snapshot();\n''',
    '''        let step_registry = self\n            .tool_registry_for_operation(operation_id)\n            .expect("model step operation has an owning lane");\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        let current_capability = self\n            .tools\n            .snapshot()\n            .capability_snapshot()\n            .identity(&call.name)\n            .map(str::to_owned);\n''',
    '''        let current_capability = self\n            .tool_registry_for_operation(operation_id)\n            .expect("tool admission operation has an owning lane")\n            .capability_snapshot()\n            .identity(&call.name)\n            .map(str::to_owned);\n''',
)

# Behavioral coverage: lane agents are structurally read-only, and a replaced
# dynamic generation cannot retarget a recovered capability snapshot.
insert_after(
    "crates/ion-core/src/tests/agent_family.rs",
    '''use super::support::*;\n''',
    '''\n#[tokio::test]\nasync fn lane_agents_are_structurally_read_only() {\n    let provider = SharedLogProvider::default();\n    let store = SessionStore::open_in_memory().expect("store");\n    let runtime = start_runtime_with_store(provider.clone(), ToolRegistry::default(), store.clone());\n    let family = runtime.agent_family(1).await.expect("family");\n    let agent = family\n        .admit_lane(family.root())\n        .await\n        .expect("agent admission");\n    family.start(agent, "inspect only").await.expect("agent start");\n\n    timeout(Duration::from_secs(2), async {\n        loop {\n            if !provider.requests().is_empty() {\n                break;\n            }\n            sleep(Duration::from_millis(10)).await;\n        }\n    })\n    .await\n    .expect("provider request");\n\n    let names = provider.requests()[0]\n        .tools\n        .iter()\n        .map(|tool| tool.name.as_str())\n        .collect::<Vec<_>>();\n    assert_eq!(names, vec!["find", "read", "search"]);\n    assert!(!names.iter().any(|name| matches!(*name, "write" | "edit" | "bash")));\n\n    let loaded = store.load(runtime.session_id()).await.expect("load");\n    let record = loaded\n        .agents\n        .iter()\n        .find(|record| record.id == agent)\n        .expect("agent record");\n    let lane = loaded\n        .lanes\n        .iter()\n        .find(|lane| lane.name == record.lane_name)\n        .expect("agent lane");\n    assert_eq!(lane.config.tools, crate::tool::ToolSelection::read_only());\n\n    runtime.session().close().await.expect("close");\n    runtime.join().await.expect("join");\n}\n''',
)
insert_after(
    "crates/ion-core/src/tool/catalog.rs",
    '''    fn scope_replacement_advances_capability_generation() {\n        let catalog = ToolCatalog::with_cwd("/tmp");\n        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);\n        let first = catalog.snapshot().capability_snapshot();\n        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);\n        let second = catalog.snapshot().capability_snapshot();\n        assert_ne!(first.id, second.id);\n        assert_ne!(first.identities, second.identities);\n    }\n''',
    '''\n    #[test]\n    fn recovery_registry_never_retargets_a_replaced_capability_generation() {\n        let catalog = ToolCatalog::with_cwd("/tmp");\n        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);\n        let persisted = catalog.snapshot().capability_snapshot();\n\n        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);\n        let recovered = catalog.snapshot().available_for_snapshot(&persisted);\n\n        assert!(recovered.get("mcp_echo").is_none());\n        assert!(recovered.get("read").is_some());\n    }\n''',
)

# Keep the canonical architecture document synchronized with the real checkpoint.
replace(
    "DESIGN.md",
    '''Storage and recovery can now represent multiple lanes. Live execution still projects `main` and owns singleton active-operation/draft/effect state; that is the active boundary.\n''',
    '''Storage, recovery, and live execution now support multiple concurrent lanes under one session writer. Operation residency/effects/continuation are operation-addressed, family-scoped retained agents have separate execution permits, waits are event-driven, and agent messaging uses the durable input path. The current client snapshot still projects `main`; scoped capabilities and replacement of the remaining child-only scaffolding are the active boundary.\n''',
)
replace(
    "DESIGN.md",
    '''7. Add scoped capability publication/teardown around agent creation/resume.\n8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants.\n''',
    '''7. Make scoped capability publication/teardown structural at lane/agent admission and exact on recovery; capability narrowing must never be reset by unrelated lane configuration changes.\n8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery and multi-agent invariants.\n''',
)
