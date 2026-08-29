from pathlib import Path


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    found = text.count(old)
    if found != count:
        raise SystemExit(f"{path}: expected {count} matches, found {found} for {old[:80]!r}")
    p.write_text(text.replace(old, new, count))


# Durable lane-level dynamic-scope authority, separate from tool-name narrowing.
replace(
    "crates/ion-core/src/session/lane.rs",
    "use crate::ids::{EntryId, OperationId};\nuse crate::tool::ToolSelection;\n",
    "use std::collections::BTreeSet;\n\nuse crate::ids::{EntryId, OperationId};\nuse crate::tool::ToolSelection;\n",
)
replace(
    "crates/ion-core/src/session/lane.rs",
    "pub(crate) const MAIN: &str = \"main\";\n\n/// Model-facing execution selection for future work on one lane.\n",
    '''pub(crate) const MAIN: &str = "main";\n\n/// Durable dynamic capability scopes structurally admitted to one lane.\n/// Core tools are inherent and therefore never appear here. `LegacyAll` is\n/// only a decode bridge for pre-Step-7 lane rows and is materialized before\n/// resumed work can recover or accept a command.\n#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]\npub(crate) enum ScopeGrant {\n    LegacyAll,\n    Only(BTreeSet<String>),\n}\n\nimpl ScopeGrant {\n    fn legacy_all() -> Self {\n        Self::LegacyAll\n    }\n\n    #[must_use]\n    pub(crate) fn none() -> Self {\n        Self::Only(BTreeSet::new())\n    }\n\n    #[must_use]\n    pub(crate) fn from_published(scopes: BTreeSet<String>) -> Self {\n        Self::Only(scopes)\n    }\n\n    pub(crate) fn materialize(&mut self, published: &BTreeSet<String>) -> bool {\n        if matches!(self, Self::LegacyAll) {\n            *self = Self::Only(published.clone());\n            true\n        } else {\n            false\n        }\n    }\n\n    pub(crate) fn insert(&mut self, scope: String) -> bool {\n        match self {\n            Self::LegacyAll => false,\n            Self::Only(scopes) => scopes.insert(scope),\n        }\n    }\n\n    #[must_use]\n    pub(crate) fn allows(&self, scope: &str) -> bool {\n        match self {\n            Self::LegacyAll => true,\n            Self::Only(scopes) => scopes.contains(scope),\n        }\n    }\n\n    #[must_use]\n    pub(crate) fn is_subset_of(&self, parent: &Self) -> bool {\n        match (self, parent) {\n            (_, Self::LegacyAll) => true,\n            (Self::LegacyAll, Self::Only(_)) => false,\n            (Self::Only(child), Self::Only(parent)) => child.is_subset(parent),\n        }\n    }\n}\n\n/// Model-facing execution selection for future work on one lane.\n''',
)
replace(
    "crates/ion-core/src/session/lane.rs",
    "pub(crate) struct Config {\n    pub(crate) model_ref: String,\n    pub(crate) tools: ToolSelection,\n}\n",
    '''pub(crate) struct Config {\n    pub(crate) model_ref: String,\n    pub(crate) tools: ToolSelection,\n    #[serde(default = "ScopeGrant::legacy_all")]\n    pub(crate) scopes: ScopeGrant,\n}\n''',
)
replace(
    "crates/ion-core/src/session/lane.rs",
    "            model_ref: model_ref.into(),\n            tools: ToolSelection::all(),\n",
    "            model_ref: model_ref.into(),\n            tools: ToolSelection::all(),\n            scopes: ScopeGrant::none(),\n",
)

# Catalog snapshots can now be projected through a durable scope grant while
# retaining the existing global snapshot for non-lane composition callers.
replace(
    "crates/ion-core/src/tool/catalog.rs",
    "    /// The merged immutable snapshot: core plus every live scope. Name\n    /// collisions resolve in favor of core tools.\n    #[must_use]\n    pub fn snapshot(&self) -> ToolRegistry {\n        let mut entries: HashMap<String, ToolEntry> = self.core.entries.as_ref().clone();\n        let active_mcp_scopes = self\n            .active_mcp_scopes\n            .read()\n            .expect(\"active MCP scope set poisoned\")\n            .clone();\n        for (scope, scoped) in self.dynamic.read().expect(\"tool catalog poisoned\").iter() {\n            // MCP servers can expose broad APIs. Keep their lifecycle\n            // separate from the deliberately small active set sent to a\n            // model step; extension/delegate scopes remain host-composed.\n            if scope.starts_with(\"mcp:\") && !active_mcp_scopes.contains(scope) {\n                continue;\n            }\n            for entry in scoped {\n                entries\n                    .entry(entry.spec.name.clone())\n                    .or_insert_with(|| entry.clone());\n            }\n        }\n        ToolRegistry {\n            cwd: Arc::from(self.core.cwd()),\n            entries: Arc::new(entries),\n        }\n    }\n",
    '''    fn snapshot_matching(&self, allows_scope: impl Fn(&str) -> bool) -> ToolRegistry {\n        let mut entries: HashMap<String, ToolEntry> = self.core.entries.as_ref().clone();\n        let active_mcp_scopes = self\n            .active_mcp_scopes\n            .read()\n            .expect("active MCP scope set poisoned")\n            .clone();\n        for (scope, scoped) in self.dynamic.read().expect("tool catalog poisoned").iter() {\n            // MCP servers can expose broad APIs. Keep their lifecycle\n            // separate from the deliberately small active set sent to a\n            // model step; extension/delegate scopes remain host-composed.\n            if scope.starts_with("mcp:") && !active_mcp_scopes.contains(scope) {\n                continue;\n            }\n            if !allows_scope(scope) {\n                continue;\n            }\n            for entry in scoped {\n                entries\n                    .entry(entry.spec.name.clone())\n                    .or_insert_with(|| entry.clone());\n            }\n        }\n        ToolRegistry {\n            cwd: Arc::from(self.core.cwd()),\n            entries: Arc::new(entries),\n        }\n    }\n\n    /// Dynamic scopes that are physically published to future model-step\n    /// snapshots right now. Inactive MCP scopes are deliberately excluded.\n    #[must_use]\n    pub(crate) fn published_scopes(&self) -> BTreeSet<String> {\n        let active_mcp_scopes = self\n            .active_mcp_scopes\n            .read()\n            .expect("active MCP scope set poisoned")\n            .clone();\n        self.dynamic\n            .read()\n            .expect("tool catalog poisoned")\n            .keys()\n            .filter(|scope| {\n                !scope.starts_with("mcp:") || active_mcp_scopes.contains(scope.as_str())\n            })\n            .cloned()\n            .collect()\n    }\n\n    /// Snapshot core tools plus only dynamic scopes structurally admitted to\n    /// the addressed lane. Tool-name narrowing is applied afterward.\n    #[must_use]\n    pub(crate) fn snapshot_for_scopes(\n        &self,\n        scopes: &crate::session::lane::ScopeGrant,\n    ) -> ToolRegistry {\n        self.snapshot_matching(|scope| scopes.allows(scope))\n    }\n\n    /// The merged immutable snapshot: core plus every currently published\n    /// scope. Name collisions resolve in favor of core tools.\n    #[must_use]\n    pub fn snapshot(&self) -> ToolRegistry {\n        self.snapshot_matching(|_| true)\n    }\n''',
)
replace(
    "crates/ion-core/src/tool/catalog.rs",
    "    #[test]\n    fn core_tools_win_name_collisions() {\n",
    '''    #[test]\n    fn scoped_snapshot_requires_structural_grant() {\n        let catalog = ToolCatalog::with_cwd("/tmp");\n        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);\n        let none = crate::session::lane::ScopeGrant::none();\n        let absent = catalog.snapshot_for_scopes(&none);\n        assert!(absent.get("mcp_echo").is_none());\n        assert!(absent.get("read").is_some());\n\n        let admitted = crate::session::lane::ScopeGrant::from_published(BTreeSet::from([\n            "server-a".to_owned(),\n        ]));\n        assert!(\n            catalog\n                .snapshot_for_scopes(&admitted)\n                .get("mcp_echo")\n                .is_some()\n        );\n    }\n\n    #[test]\n    fn unrelated_scope_registered_later_is_not_in_admitted_snapshot() {\n        let catalog = ToolCatalog::with_cwd("/tmp");\n        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);\n        let admitted = crate::session::lane::ScopeGrant::from_published(BTreeSet::from([\n            "server-a".to_owned(),\n        ]));\n        let before = catalog.snapshot_for_scopes(&admitted).capability_snapshot();\n\n        catalog.register_scope("server-b", vec![Arc::new(EchoTool)]);\n        let after = catalog.snapshot_for_scopes(&admitted).capability_snapshot();\n        assert_eq!(before.id, after.id);\n        assert_eq!(before.identities, after.identities);\n    }\n\n    #[test]\n    fn admitted_scope_refreshes_to_a_new_generation() {\n        let catalog = ToolCatalog::with_cwd("/tmp");\n        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);\n        let admitted = crate::session::lane::ScopeGrant::from_published(BTreeSet::from([\n            "server-a".to_owned(),\n        ]));\n        let first = catalog.snapshot_for_scopes(&admitted).capability_snapshot();\n        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);\n        let second = catalog.snapshot_for_scopes(&admitted).capability_snapshot();\n        assert_ne!(first.id, second.id);\n        assert_ne!(first.identities, second.identities);\n    }\n\n    #[test]\n    fn core_tools_win_name_collisions() {\n''',
)

# Child admission must enforce scope authority in the same durable transaction
# that already enforces tool-name narrowing.
replace(
    "crates/ion-core/src/store/sql.rs",
    "    if lane.config.model_ref != parent_config.model_ref\n        || !lane.config.tools.is_subset_of(&parent_config.tools)\n",
    "    if lane.config.model_ref != parent_config.model_ref\n        || !lane.config.tools.is_subset_of(&parent_config.tools)\n        || !lane.config.scopes.is_subset_of(&parent_config.scopes)\n",
)
replace(
    "crates/ion-core/src/store/sql.rs",
    "    if !config.tools.is_subset_of(&parent_config.tools) {\n",
    "    if !config.tools.is_subset_of(&parent_config.tools)\n        || !config.scopes.is_subset_of(&parent_config.scopes)\n    {\n",
)

# Family can attach from an already-durable resumed session without issuing a
# session command that would prematurely trigger recovery.
replace(
    "crates/ion-core/src/agent.rs",
    "impl Family {\n    pub(crate) async fn attach(\n        session_id: SessionId,\n        session: SessionHandle,\n        store: SessionStore,\n        max_active: usize,\n    ) -> Result<Self, Error> {\n        // A command round-trip establishes that a newly spawned session has\n        // committed its root record before we read family topology.\n        let _ = session.snapshot().await?;\n",
    '''impl Family {\n    pub(crate) async fn attach(\n        session_id: SessionId,\n        session: SessionHandle,\n        store: SessionStore,\n        max_active: usize,\n    ) -> Result<Self, Error> {\n        Self::attach_inner(session_id, session, store, max_active, true).await\n    }\n\n    pub(crate) async fn attach_durable(\n        session_id: SessionId,\n        session: SessionHandle,\n        store: SessionStore,\n        max_active: usize,\n    ) -> Result<Self, Error> {\n        Self::attach_inner(session_id, session, store, max_active, false).await\n    }\n\n    async fn attach_inner(\n        session_id: SessionId,\n        session: SessionHandle,\n        store: SessionStore,\n        max_active: usize,\n        await_session_start: bool,\n    ) -> Result<Self, Error> {\n        // New sessions need a command round-trip so the root row is durable. A\n        // resumed interactive runtime already has that row and may deliberately\n        // be waiting for host scope reattachment before recovery starts.\n        if await_session_start {\n            let _ = session.snapshot().await?;\n        }\n''',
)

# Runtime: defer resumed interactive recovery until the first command so host
# scopes can be reattached, materialize legacy grants once, scope every lane
# snapshot, and make context manifests use the same admitted registry.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    cwd: Option<String>,\n}\n",
    "    cwd: Option<String>,\n    defer_loaded_start: bool,\n}\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "            effect_gate: None,\n            cwd: None,\n",
    "            effect_gate: None,\n            cwd: None,\n            defer_loaded_start: false,\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    fn spawn(mut self, session_id: SessionId, loaded: Option<LoadedSession>) -> Runtime {\n        let initial_model_ref = self.provider.initial_model_ref();\n",
    "    fn spawn(mut self, session_id: SessionId, loaded: Option<LoadedSession>) -> Runtime {\n        let initial_model_ref = self.provider.initial_model_ref();\n        let deferred_loaded_start = self.defer_loaded_start && loaded.is_some();\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "        let provider = Arc::new(self.provider);\n        let tools = Arc::new(self.tools);\n        let runtime_store = self.store.clone();\n",
    "        let provider = Arc::new(self.provider);\n        let runtime_tools = self.tools.clone();\n        let tools = Arc::new(self.tools);\n        let runtime_store = self.store.clone();\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "                    fork_source: self.fork_source,\n                },\n",
    "                    fork_source: self.fork_source,\n                    defer_loaded_start: deferred_loaded_start,\n                },\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "        Runtime {\n            session,\n            session_id,\n            store: runtime_store,\n            join,\n        }\n",
    "        Runtime {\n            session,\n            session_id,\n            store: runtime_store,\n            tools: runtime_tools,\n            deferred_loaded_start,\n            join,\n        }\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "pub struct Runtime {\n    session: SessionHandle,\n    session_id: SessionId,\n    store: SessionStore,\n    join: JoinHandle<()>,\n}\n",
    "pub struct Runtime {\n    session: SessionHandle,\n    session_id: SessionId,\n    store: SessionStore,\n    tools: ToolCatalog,\n    deferred_loaded_start: bool,\n    join: JoinHandle<()>,\n}\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "        composition.interactive_approvals = true;\n        composition.trusted_resources = trusted_resources;\n        Ok(composition.spawn(session_id, Some(loaded)))\n",
    "        composition.interactive_approvals = true;\n        composition.trusted_resources = trusted_resources;\n        composition.defer_loaded_start = true;\n        Ok(composition.spawn(session_id, Some(loaded)))\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    pub async fn agent_family(\n        &self,\n        max_active: usize,\n    ) -> Result<crate::agent::Family, crate::agent::Error> {\n        crate::agent::Family::attach(\n            self.session_id,\n            self.session.clone(),\n            self.store.clone(),\n            max_active,\n        )\n        .await\n    }\n",
    '''    pub async fn agent_family(\n        &self,\n        max_active: usize,\n    ) -> Result<crate::agent::Family, crate::agent::Error> {\n        if self.deferred_loaded_start {\n            crate::agent::Family::attach_durable(\n                self.session_id,\n                self.session.clone(),\n                self.store.clone(),\n                max_active,\n            )\n            .await\n        } else {\n            crate::agent::Family::attach(\n                self.session_id,\n                self.session.clone(),\n                self.store.clone(),\n                max_active,\n            )\n            .await\n        }\n    }\n\n    pub(crate) async fn admit_structural_scope(\n        &self,\n        scope: &str,\n    ) -> Result<(), crate::agent::Error> {\n        if self.deferred_loaded_start {\n            // The session task has not restored its loaded copy yet. Update the\n            // durable lanes directly; its first command reloads this state before\n            // recovery, so no stale in-memory grant can win.\n            let mut loaded = self.store.load(self.session_id).await?;\n            let published = self.tools.published_scopes();\n            for lane in &mut loaded.lanes {\n                let materialized = lane.config.scopes.materialize(&published);\n                let inserted = lane.config.scopes.insert(scope.to_owned());\n                if materialized || inserted {\n                    self.store\n                        .set_lane_config(self.session_id, &lane.name, lane.config.clone())\n                        .await?;\n                }\n            }\n            return Ok(());\n        }\n        self.session.admit_structural_scope(scope).await?;\n        Ok(())\n    }\n''',
)
# Session command and handle path for live structural admission.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    AdmitAgentLane {\n        agent_id: AgentId,\n        control_parent_id: AgentId,\n        source_lane_name: String,\n        reply: oneshot::Sender<Result<String, CommandError>>,\n    },\n",
    "    AdmitAgentLane {\n        agent_id: AgentId,\n        control_parent_id: AgentId,\n        source_lane_name: String,\n        reply: oneshot::Sender<Result<String, CommandError>>,\n    },\n    AdmitStructuralScope {\n        scope: String,\n        reply: oneshot::Sender<Result<(), CommandError>>,\n    },\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    pub(crate) async fn admit_agent_lane(\n        &self,\n        agent_id: AgentId,\n        control_parent_id: AgentId,\n        source_lane_name: impl Into<String>,\n    ) -> Result<String, CommandError> {\n",
    "    pub(crate) async fn admit_agent_lane(\n        &self,\n        agent_id: AgentId,\n        control_parent_id: AgentId,\n        source_lane_name: impl Into<String>,\n    ) -> Result<String, CommandError> {\n",
)
# Insert method immediately after admit_agent_lane's closing block using the next doc comment.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "        rx.await.map_err(|_| CommandError::RuntimeDropped)?\n    }\n\n    /// Accept a prompt durably on main and open a new operation only when idle.\n",
    '''        rx.await.map_err(|_| CommandError::RuntimeDropped)?\n    }\n\n    pub(crate) async fn admit_structural_scope(\n        &self,\n        scope: impl Into<String>,\n    ) -> Result<(), CommandError> {\n        let (reply, rx) = oneshot::channel();\n        self.tx\n            .try_send(SessionCommand::AdmitStructuralScope {\n                scope: scope.into(),\n                reply,\n            })\n            .map_err(command_send_error)?;\n        rx.await.map_err(|_| CommandError::RuntimeDropped)?\n    }\n\n    /// Accept a prompt durably on main and open a new operation only when idle.\n''',
)
# Session deps and live runtime retain loaded state until startup ordering is safe.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    /// Explicit history lineage; independent from control parentage.\n    fork_source: Option<(SessionId, Option<EntryId>)>,\n}\n",
    "    /// Explicit history lineage; independent from control parentage.\n    fork_source: Option<(SessionId, Option<EntryId>)>,\n    defer_loaded_start: bool,\n}\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    /// True when reopened from the store; the session row already exists.\n    resumed: bool,\n",
    "    /// True when reopened from the store; the session row already exists.\n    resumed: bool,\n    loaded: Option<LoadedSession>,\n    defer_loaded_start: bool,\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "            parent,\n            fork_source,\n        } = deps;\n",
    "            parent,\n            fork_source,\n            defer_loaded_start,\n        } = deps;\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "        let mut runtime = Self {\n",
    "        let resumed = loaded.is_some();\n        Self {\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "            closed: false,\n            resumed: false,\n            reopen_entry_count: None,\n        };\n        if let Some(loaded) = loaded {\n            runtime.resumed = true;\n            runtime.restore_from(loaded);\n        }\n        runtime\n",
    "            closed: false,\n            resumed,\n            loaded,\n            defer_loaded_start,\n            reopen_entry_count: None,\n        }\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    fn tool_registry_for_lane(&self, lane_name: &str) -> Option<ToolRegistry> {\n        let selection = &self.lane(lane_name)?.config.tools;\n        Some(self.tools.snapshot().selected(selection))\n    }\n",
    '''    fn tool_registry_for_lane(&self, lane_name: &str) -> Option<ToolRegistry> {\n        let config = &self.lane(lane_name)?.config;\n        Some(\n            self.tools\n                .snapshot_for_scopes(&config.scopes)\n                .selected(&config.tools),\n        )\n    }\n''',
)
# Startup scope materialization helpers before run.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    async fn run(mut self) {\n        if !self.resumed && !self.closed {\n",
    '''    async fn materialize_loaded_scope_grants(\n        &self,\n        loaded: &mut LoadedSession,\n    ) -> Result<(), StoreError> {\n        let published = self.tools.published_scopes();\n        for lane in &mut loaded.lanes {\n            if lane.config.scopes.materialize(&published) {\n                self.store\n                    .set_lane_config(self.session_id, &lane.name, lane.config.clone())\n                    .await?;\n            }\n        }\n        Ok(())\n    }\n\n    async fn run(mut self) {\n        let mut startup_command = None;\n        if self.resumed {\n            if self.defer_loaded_start {\n                startup_command = self.commands.recv().await;\n                if startup_command.is_none() {\n                    return;\n                }\n                match self.store.load(self.session_id).await {\n                    Ok(loaded) => self.loaded = Some(loaded),\n                    Err(err) => {\n                        error!(session = %self.session_id, %err, "could not reload deferred session");\n                        return;\n                    }\n                }\n            }\n            let Some(mut loaded) = self.loaded.take() else {\n                error!(session = %self.session_id, "resumed runtime has no loaded state");\n                return;\n            };\n            if let Err(err) = self.materialize_loaded_scope_grants(&mut loaded).await {\n                error!(session = %self.session_id, %err, "could not materialize structural scope grants");\n                return;\n            }\n            self.restore_from(loaded);\n        } else {\n            let published = self.tools.published_scopes();\n            self.lane_mut(crate::session::lane::MAIN)\n                .expect("new runtime has a main lane")\n                .config\n                .scopes = crate::session::lane::ScopeGrant::from_published(published);\n        }\n\n        if !self.resumed && !self.closed {\n''',
)
# Persist initial exact scope grant after root/session creation and before commands.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "            if let Err(err) = self.store.create_session(record).await {\n                error!(\n                    session = %self.session_id,\n                    %err,\n                    \"session row not durable; session will not start\"\n                );\n                self.closed = true;\n                return;\n            }\n        }\n        info!(session = %self.session_id, \"session opened\");\n",
    '''            if let Err(err) = self.store.create_session(record).await {\n                error!(\n                    session = %self.session_id,\n                    %err,\n                    "session row not durable; session will not start"\n                );\n                self.closed = true;\n                return;\n            }\n            let config = self.main_lane().config.clone();\n            if let Err(err) = self\n                .store\n                .set_lane_config(self.session_id, crate::session::lane::MAIN, config)\n                .await\n            {\n                error!(session = %self.session_id, %err, "initial scope grant not durable");\n                self.closed = true;\n                return;\n            }\n        }\n        info!(session = %self.session_id, "session opened");\n''',
)
# Process the command that released deferred startup only after recovery is complete.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "        loop {\n            tokio::select! {\n",
    "        if let Some(command) = startup_command\n            && self.handle_command(command).await\n        {\n            return;\n        }\n        loop {\n            tokio::select! {\n",
)
# Runtime command dispatch + persistence for structural grants.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "            SessionCommand::SubmitIfIdle {\n",
    "            SessionCommand::AdmitStructuralScope { scope, reply } => {\n                let _ = reply.send(self.admit_structural_scope(scope).await);\n                false\n            }\n            SessionCommand::SubmitIfIdle {\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    async fn submit_if_idle_on_lane(\n",
    '''    async fn admit_structural_scope(&mut self, scope: String) -> Result<(), CommandError> {\n        if self.closed {\n            return Err(CommandError::Closed);\n        }\n        let lane_names = self.lanes.keys().cloned().collect::<Vec<_>>();\n        for lane_name in lane_names {\n            let mut config = self\n                .lane(&lane_name)\n                .expect("resident lane remains available")\n                .config\n                .clone();\n            if !config.scopes.insert(scope.clone()) {\n                continue;\n            }\n            self.store\n                .set_lane_config(self.session_id, &lane_name, config.clone())\n                .await\n                .map_err(persistence_command_error)?;\n            self.lane_mut(&lane_name)\n                .expect("persisted lane remains resident")\n                .config = config;\n        }\n        Ok(())\n    }\n\n    async fn submit_if_idle_on_lane(\n''',
)
# Context manifests and model-step registry are one frozen admitted projection.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "    fn current_context_manifest(&self) -> (CapabilitySnapshot, ContextManifest) {\n        let snapshot = self.tools.snapshot().capability_snapshot();\n        let manifest = ContextManifest::new(&snapshot, self.trusted_resources.clone());\n        (snapshot, manifest)\n    }\n",
    '''    fn current_context_manifest(\n        &self,\n        operation_id: OperationId,\n    ) -> (ToolRegistry, CapabilitySnapshot, ContextManifest) {\n        let registry = self\n            .tool_registry_for_operation(operation_id)\n            .expect("context manifest operation has an owning lane");\n        let snapshot = registry.capability_snapshot();\n        let manifest = ContextManifest::new(&snapshot, self.trusted_resources.clone());\n        (registry, snapshot, manifest)\n    }\n''',
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "        let model = self.current_model_config(operation_id).await;\n        let (_, manifest) = self.current_context_manifest();\n",
    "        let model = self.current_model_config(operation_id).await;\n        let (_, _, manifest) = self.current_context_manifest(operation_id);\n",
)
replace(
    "crates/ion-core/src/runtime/mod.rs",
    "        let (_, planning_manifest) = self.current_context_manifest();\n        let plan = self\n            .project_model_step_plan(operation_id, &planning_manifest)\n            .await;\n        let model = self.current_model_config(operation_id).await;\n        let mut staged = self\n            .active(operation_id)\n            .cloned()\n            .expect(\"step needs an operation\");\n        let step_registry = self\n            .tool_registry_for_operation(operation_id)\n            .expect(\"model step operation has an owning lane\");\n        let capability_snapshot = step_registry.capability_snapshot();\n",
    "        let (step_registry, capability_snapshot, planning_manifest) =\n            self.current_context_manifest(operation_id);\n        let plan = self\n            .project_model_step_plan(operation_id, &planning_manifest)\n            .await;\n        let model = self.current_model_config(operation_id).await;\n        let mut staged = self\n            .active(operation_id)\n            .cloned()\n            .expect(\"step needs an operation\");\n",
)

# Host publication is ordered around the durable grant. A deferred resume has
# no live model execution yet, so it can register first and let the first-command
# reload materialize/observe the pre-recovery durable grant.
replace(
    "crates/ion-core/src/agent_host.rs",
    "/// Publish the unified agent namespace as structural host capabilities.\npub fn install_agent_host_tools<P: Provider + 'static>(\n    catalog: &crate::tool::ToolCatalog,\n    family: Arc<crate::agent::Family>,\n    hosted: Arc<HostedAgentRuntimes<P>>,\n) {\n    catalog.register_structural_scope(\"agents\", agent_host_tools(family, hosted));\n}\n",
    '''const AGENT_SCOPE: &str = "agents";\n\n/// Publish the unified agent namespace as structural host capabilities.\npub async fn install_agent_host_tools<P: Provider + 'static>(\n    catalog: &crate::tool::ToolCatalog,\n    runtime: &Runtime,\n    family: Arc<crate::agent::Family>,\n    hosted: Arc<HostedAgentRuntimes<P>>,\n) -> Result<(), crate::agent::Error> {\n    runtime.admit_structural_scope(AGENT_SCOPE).await?;\n    catalog.register_structural_scope(AGENT_SCOPE, agent_host_tools(family, hosted));\n    Ok(())\n}\n''',
)
replace(
    "crates/ion/src/lib.rs",
    "    ion_core::install_agent_host_tools(tools, Arc::clone(&family), Arc::clone(&hosted));\n    Ok(AgentHost { family, hosted })\n",
    "    ion_core::install_agent_host_tools(\n        tools,\n        runtime,\n        Arc::clone(&family),\n        Arc::clone(&hosted),\n    )\n    .await?;\n    Ok(AgentHost { family, hosted })\n",
)

# Record the landed semantics in the normative design checkpoint.
replace(
    "DESIGN.md",
    "Recovery reconstructs an operation registry by intersecting its immutable capability snapshot with the lane's current structural selection; it never reacquires an executor from a later live catalog snapshot. The current client snapshot still projects `main`; scoped capability lifecycle beyond this admission boundary remains active work.\n",
    "Recovery reconstructs an operation registry by intersecting its immutable capability snapshot with the lane's current structural selection; it never reacquires an executor from a later live catalog snapshot. Durable lane configuration now separates dynamic structural-scope grants from tool-name narrowing: core tools are inherent, a lane sees only admitted dynamic scopes, and later generations inside an admitted scope may appear at a future model-step boundary without granting unrelated scopes. Pre-Step-7 lane rows materialize their legacy ambient scope set once before resumed work; new sessions persist the currently published scope set before accepting commands. Model-step context manifests and tool descriptions come from the same admitted registry snapshot. Resumed interactive sessions defer restoration/recovery until the first session command so the host can reattach durable structural scopes such as `agents` before exact recovery runs. The current client snapshot still projects `main`; scoped capability teardown beyond this admission boundary remains active work.\n",
)

print("step7 structural scope patch applied")
