from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected one match, found {count}")
    file.write_text(text.replace(old, new, 1))


catalog = "crates/ion-core/src/tool/catalog.rs"
replace_once(
    catalog,
    '''pub struct ToolCatalog {
    core: ToolRegistry,
    dynamic: Arc<std::sync::RwLock<HashMap<String, Vec<ToolEntry>>>>,
    generations: Arc<std::sync::RwLock<HashMap<String, u64>>>,
    active_mcp_scopes: Arc<std::sync::RwLock<HashSet<String>>>,
    lifetime: Arc<CatalogLifetime>,
}''',
    '''pub struct ToolCatalog {
    core: ToolRegistry,
    /// Structural scope identities owned by the host, independent of whether
    /// a peer currently has a live tool generation published.
    declared_scopes: Arc<std::sync::RwLock<BTreeSet<String>>>,
    dynamic: Arc<std::sync::RwLock<HashMap<String, Vec<ToolEntry>>>>,
    generations: Arc<std::sync::RwLock<HashMap<String, u64>>>,
    active_mcp_scopes: Arc<std::sync::RwLock<HashSet<String>>>,
    lifetime: Arc<CatalogLifetime>,
}''',
)
replace_once(
    catalog,
    '''pub(crate) struct CatalogService {
    dynamic: Arc<std::sync::RwLock<HashMap<String, Vec<ToolEntry>>>>,
    generations: Arc<std::sync::RwLock<HashMap<String, u64>>>,
    lifetime: std::sync::Weak<CatalogLifetime>,
}''',
    '''pub(crate) struct CatalogService {
    declared_scopes: Arc<std::sync::RwLock<BTreeSet<String>>>,
    dynamic: Arc<std::sync::RwLock<HashMap<String, Vec<ToolEntry>>>>,
    generations: Arc<std::sync::RwLock<HashMap<String, u64>>>,
    lifetime: std::sync::Weak<CatalogLifetime>,
}''',
)
replace_once(
    catalog,
    '''impl CatalogService {
    pub(crate) fn register_scope(&self, scope: String, tools: Vec<Arc<dyn Tool>>) {''',
    '''impl CatalogService {
    /// Declare the stable structural identity before discovery. A peer may be
    /// temporarily unavailable without turning its later generation into a
    /// newly ambient capability.
    pub(crate) fn declare_scope(&self, scope: String) {
        self.declared_scopes
            .write()
            .expect("tool catalog poisoned")
            .insert(scope);
    }

    pub(crate) fn register_scope(&self, scope: String, tools: Vec<Arc<dyn Tool>>) {''',
)
replace_once(
    catalog,
    '''    pub fn register_scope(&self, scope: impl Into<String>, tools: Vec<Arc<dyn Tool>>) {
        let scope = scope.into();
        let generation = next_generation(&self.generations, &scope);''',
    '''    pub fn register_scope(&self, scope: impl Into<String>, tools: Vec<Arc<dyn Tool>>) {
        let scope = scope.into();
        self.declared_scopes
            .write()
            .expect("tool catalog poisoned")
            .insert(scope.clone());
        let generation = next_generation(&self.generations, &scope);''',
)
replace_once(
    catalog,
    '''    ) {
        let scope = scope.into();
        let generation = next_generation(&self.generations, &scope);
        self.dynamic.write().expect("tool catalog poisoned").insert(
            scope.clone(),
            dynamic_entries(&scope, generation, tools, PolicyRoute::Structural),
        );
    }

    /// Remove a scope; future snapshots no longer include its tools.''',
    '''    ) {
        let scope = scope.into();
        self.declared_scopes
            .write()
            .expect("tool catalog poisoned")
            .insert(scope.clone());
        let generation = next_generation(&self.generations, &scope);
        self.dynamic.write().expect("tool catalog poisoned").insert(
            scope.clone(),
            dynamic_entries(&scope, generation, tools, PolicyRoute::Structural),
        );
    }

    /// Unpublish a scope's current live generation. Its structural identity
    /// remains declared: temporary peer loss must not revoke lane authority.''',
)
replace_once(
    catalog,
    '''        CatalogService {
            dynamic: Arc::clone(&self.dynamic),
            generations: Arc::clone(&self.generations),
            lifetime: Arc::downgrade(&self.lifetime),
        }''',
    '''        CatalogService {
            declared_scopes: Arc::clone(&self.declared_scopes),
            dynamic: Arc::clone(&self.dynamic),
            generations: Arc::clone(&self.generations),
            lifetime: Arc::downgrade(&self.lifetime),
        }''',
)
replace_once(
    catalog,
    '''    /// Dynamic scopes that are physically published to future model-step
    /// snapshots right now. Inactive MCP scopes are deliberately excluded.
    #[must_use]
    pub(crate) fn published_scopes(&self) -> BTreeSet<String> {
        let active_mcp_scopes = self
            .active_mcp_scopes
            .read()
            .expect("active MCP scope set poisoned")
            .clone();
        self.dynamic
            .read()
            .expect("tool catalog poisoned")
            .keys()
            .filter(|scope| {
                !scope.starts_with("mcp:") || active_mcp_scopes.contains(scope.as_str())
            })
            .cloned()
            .collect()
    }''',
    '''    /// Structural identities the current host may admit to a lane. This is
    /// deliberately independent of live discovery: a configured peer that is
    /// temporarily unavailable keeps the same authority across restart, while
    /// model-step snapshots still require a live generation.
    #[must_use]
    pub(crate) fn admission_scopes(&self) -> BTreeSet<String> {
        let active_mcp_scopes = self
            .active_mcp_scopes
            .read()
            .expect("active MCP scope set poisoned")
            .clone();
        self.declared_scopes
            .read()
            .expect("tool catalog poisoned")
            .iter()
            .filter(|scope| {
                !scope.starts_with("mcp:") || active_mcp_scopes.contains(scope.as_str())
            })
            .cloned()
            .collect()
    }''',
)
replace_once(
    catalog,
    '''        Self {
            core,
            dynamic: Arc::new(std::sync::RwLock::new(HashMap::new())),''',
    '''        Self {
            core,
            declared_scopes: Arc::new(std::sync::RwLock::new(BTreeSet::new())),
            dynamic: Arc::new(std::sync::RwLock::new(HashMap::new())),''',
)
replace_once(
    catalog,
    '''    #[test]
    fn structural_policy_route_is_available_only_through_core_composition() {''',
    '''    #[test]
    fn declared_scope_survives_transient_unpublication() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        let service = catalog.service_handle();
        service.declare_scope("server-a".to_owned());
        let admitted = crate::session::lane::ScopeGrant::from_published(
            catalog.admission_scopes(),
        );
        assert!(catalog.snapshot_for_scopes(&admitted).get("mcp_echo").is_none());

        service.register_scope("server-a".to_owned(), vec![Arc::new(EchoTool)]);
        assert!(catalog.snapshot_for_scopes(&admitted).get("mcp_echo").is_some());
        assert!(service.remove_scope("server-a"));
        assert!(catalog.snapshot_for_scopes(&admitted).get("mcp_echo").is_none());
        assert!(catalog.admission_scopes().contains("server-a"));

        service.register_scope("server-a".to_owned(), vec![Arc::new(EchoTool)]);
        assert!(catalog.snapshot_for_scopes(&admitted).get("mcp_echo").is_some());
    }

    #[test]
    fn structural_policy_route_is_available_only_through_core_composition() {''',
)

replace_once(
    "crates/ion-core/src/mcp.rs",
    '''            let service = catalog.service_handle();
            let name = def.name.clone();
            let peer_service = service.clone();
            let spawned = service.spawn(async move {
                supervise_tool_peer(
                    PeerDef {
                        name: name.clone(),
                        command: def.command,
                        args: def.args,
                    },
                    // Namespaced scope so two servers cannot collide.
                    format!("mcp:{name}"),''',
    '''            let service = catalog.service_handle();
            let name = def.name.clone();
            // Structural identity belongs to configuration, not successful
            // discovery. A later supervisor restart republishes a generation
            // inside the same already-admitted scope.
            let scope = format!("mcp:{name}");
            service.declare_scope(scope.clone());
            let peer_service = service.clone();
            let spawned = service.spawn(async move {
                supervise_tool_peer(
                    PeerDef {
                        name: name.clone(),
                        command: def.command,
                        args: def.args,
                    },
                    // Namespaced scope so two servers cannot collide.
                    scope,''',
)
replace_once(
    "crates/ion-core/src/extensions.rs",
    '''            let service = catalog.service_handle();
            let name = def.name.clone();
            let peer_service = service.clone();
            let spawned = service.spawn(async move {
                supervise_tool_peer(
                    PeerDef {
                        name: name.clone(),
                        command: def.command,
                        args: def.args,
                    },
                    format!("ext:{name}"),''',
    '''            let service = catalog.service_handle();
            let name = def.name.clone();
            // The configured extension owns this structural identity before
            // its first successful tools/list. Live generations may come and
            // go without changing the lane's admitted scope.
            let scope = format!("ext:{name}");
            service.declare_scope(scope.clone());
            let peer_service = service.clone();
            let spawned = service.spawn(async move {
                supervise_tool_peer(
                    PeerDef {
                        name: name.clone(),
                        command: def.command,
                        args: def.args,
                    },
                    scope,''',
)

runtime = Path("crates/ion-core/src/runtime/mod.rs")
runtime_text = runtime.read_text()
count = runtime_text.count("published_scopes()")
if count < 2:
    raise SystemExit(f"runtime: expected at least two published_scopes callers, found {count}")
runtime.write_text(runtime_text.replace("published_scopes()", "admission_scopes()"))

flow = "crates/ion-core/src/tests/operation_flow.rs"
replace_once(
    flow,
    '''#[tokio::test]
async fn steer_projection_reaches_the_next_model_step() {''',
    '''#[tokio::test]
async fn declared_scope_can_publish_after_session_start() {
    struct LateTool;
    impl Tool for LateTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "late".to_owned(),
                description: "late discovered capability".to_owned(),
                input_schema: json!({"type": "object", "required": []}),
            }
        }

        fn call<'a>(
            &'a self,
            _arguments: serde_json::Value,
            _cancel: CancellationToken,
        ) -> std::pin::Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
            Box::pin(async { ToolOutcome::text("late") })
        }
    }

    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(100),
        ..SharedLogProvider::default()
    };
    let catalog = ToolCatalog::default();
    let service = catalog.service_handle();
    service.declare_scope("late-scope".to_owned());
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_policy(
        provider.clone(),
        catalog,
        store,
        permissive_policy(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    assert!(
        !provider.requests()[0]
            .tools
            .iter()
            .any(|tool| tool.name == "late")
    );

    service.register_scope("late-scope".to_owned(), vec![Arc::new(LateTool)]);
    session.steer("continue").await.expect("steer");
    collect_until_terminal(&mut events).await.expect("collect");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].tools.iter().any(|tool| tool.name == "late"));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn steer_projection_reaches_the_next_model_step() {''',
)

replace_once(
    "DESIGN.md",
    '''Resumed interactive sessions defer restoration/recovery until the first session command so the host can reattach durable structural scopes such as `agents` before exact recovery runs. The current client snapshot still projects `main`; scoped capability teardown beyond this admission boundary remains active work.''',
    '''Resumed interactive sessions defer restoration/recovery until the first session command so the host can reattach durable structural scopes such as `agents` before exact recovery runs. Configured MCP/extension structural identities are declared before discovery and remain distinct from their currently live tool generation, so transient peer loss/restart does not accidentally revoke or ambiently re-grant authority. The current client snapshot still projects `main`; explicit host-driven scope deconfiguration/teardown beyond this declared/live split remains active work.''',
)

print("step7 declared-scope lifecycle patch applied")
