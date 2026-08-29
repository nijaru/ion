use super::*;
use std::collections::HashSet;
use std::sync::Mutex;
use tokio::task::JoinSet;

/// Build the default core-tool entries under `cwd`.
/// The dynamic capability layer (DESIGN.md §18): core tools plus
/// scoped registrations from MCP servers and extensions. Everything
/// registered through a scope is owned by it; removing the scope
/// removes its tools from future snapshots. A snapshot is an ordinary
/// [`ToolRegistry`] - immutable once handed to a model step or a
/// dispatching effect task, so a disappearing scope cannot mutate a
/// started request (§18.2).
#[derive(Clone)]
pub struct ToolCatalog {
    core: ToolRegistry,
    dynamic: Arc<std::sync::RwLock<HashMap<String, Vec<ToolEntry>>>>,
    generations: Arc<std::sync::RwLock<HashMap<String, u64>>>,
    active_mcp_scopes: Arc<std::sync::RwLock<HashSet<String>>>,
    lifetime: Arc<CatalogLifetime>,
}

struct CatalogLifetime {
    cancel: CancellationToken,
    tasks: Mutex<Option<JoinSet<()>>>,
}

/// Failure while joining peer supervisors owned by a tool catalog.
#[derive(Debug, thiserror::Error)]
pub enum ToolCatalogError {
    #[error("tool supervisor task failed during shutdown: {0}")]
    TaskFailed(String),
    #[error("tool supervisor tasks did not drain before the shutdown deadline")]
    DrainTimeout,
}

impl CatalogLifetime {
    fn spawn<F>(&self, task: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.cancel.is_cancelled() {
            return false;
        }
        let mut tasks = self.tasks.lock().expect("tool catalog poisoned");
        if self.cancel.is_cancelled() {
            return false;
        }
        tasks
            .as_mut()
            .expect("catalog lifetime tasks are available until shutdown")
            .spawn(task);
        true
    }

    async fn shutdown(&self) -> Result<(), ToolCatalogError> {
        self.cancel.cancel();
        let Some(mut tasks) = self.tasks.lock().expect("tool catalog poisoned").take() else {
            return Ok(());
        };

        let drain = async {
            let mut first_error = None;
            while let Some(result) = tasks.join_next().await {
                if let Err(err) = result {
                    first_error.get_or_insert_with(|| err.to_string());
                }
            }
            first_error
        };
        match tokio::time::timeout(PEER_DRAIN_TIMEOUT, drain).await {
            Ok(Some(err)) => Err(ToolCatalogError::TaskFailed(err)),
            Ok(None) => Ok(()),
            Err(_) => {
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
                Err(ToolCatalogError::DrainTimeout)
            }
        }
    }
}

const PEER_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

impl Drop for CatalogLifetime {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[derive(Clone)]
pub(crate) struct CatalogService {
    dynamic: Arc<std::sync::RwLock<HashMap<String, Vec<ToolEntry>>>>,
    generations: Arc<std::sync::RwLock<HashMap<String, u64>>>,
    lifetime: std::sync::Weak<CatalogLifetime>,
}

fn next_generation(generations: &std::sync::RwLock<HashMap<String, u64>>, scope: &str) -> u64 {
    let mut generations = generations.write().expect("tool catalog poisoned");
    let generation = generations.entry(scope.to_owned()).or_default();
    *generation = generation.saturating_add(1);
    *generation
}

fn dynamic_entries(
    scope: &str,
    generation: u64,
    tools: Vec<Arc<dyn Tool>>,
    policy_route: PolicyRoute,
) -> Vec<ToolEntry> {
    tools
        .into_iter()
        .map(|tool| {
            let spec = tool.spec();
            ToolEntry {
                capability_id: format!("scope:{scope}:{name}@{generation}", name = spec.name),
                tool,
                spec,
                recovery_class: RecoveryClass::NeverReplay,
                semantics: ToolSemantics::Remote,
                policy_route,
            }
        })
        .collect()
}

impl CatalogService {
    pub(crate) fn register_scope(&self, scope: String, tools: Vec<Arc<dyn Tool>>) {
        let generation = next_generation(&self.generations, &scope);
        self.dynamic.write().expect("tool catalog poisoned").insert(
            scope.clone(),
            dynamic_entries(&scope, generation, tools, PolicyRoute::Gated),
        );
    }

    pub(crate) fn remove_scope(&self, scope: &str) -> bool {
        self.dynamic
            .write()
            .expect("tool catalog poisoned")
            .remove(scope)
            .is_some()
    }

    pub(crate) fn lifetime(&self) -> Option<CancellationToken> {
        self.lifetime
            .upgrade()
            .map(|lifetime| lifetime.cancel.clone())
    }

    pub(crate) fn spawn<F>(&self, task: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.lifetime
            .upgrade()
            .is_some_and(|lifetime| lifetime.spawn(task))
    }
}

impl ToolCatalog {
    /// A catalog over `cwd` with only the core tool set.
    #[must_use]
    pub fn with_cwd(cwd: impl AsRef<Path>) -> Self {
        Self::with_cwd_and_sandbox(cwd, SandboxMode::Auto)
    }

    /// A catalog with an explicit native-shell enforcement mode.
    #[must_use]
    pub fn with_cwd_and_sandbox(cwd: impl AsRef<Path>, sandbox: SandboxMode) -> Self {
        Self::from(ToolRegistry::with_cwd_and_sandbox(cwd, sandbox))
    }

    /// A read-only catalog over `cwd` (§20.4): the bounded research
    /// child capability set.
    #[must_use]
    pub fn read_only(cwd: impl AsRef<Path>) -> Self {
        Self::from(ToolRegistry::read_only(cwd))
    }

    #[must_use]
    pub fn cwd(&self) -> &Path {
        self.core.cwd()
    }

    /// Register tools under `scope`, replacing that scope's previous
    /// registration. Publishing at a safe context boundary is the
    /// caller's contract (§19.2).
    pub fn register_scope(&self, scope: impl Into<String>, tools: Vec<Arc<dyn Tool>>) {
        let scope = scope.into();
        let generation = next_generation(&self.generations, &scope);
        self.dynamic.write().expect("tool catalog poisoned").insert(
            scope.clone(),
            dynamic_entries(&scope, generation, tools, PolicyRoute::Gated),
        );
    }

    /// Core-owned host controls may bypass per-effect approval because their
    /// authority is structural and their spawned effects are gated separately.
    /// This is crate-private so extensions/MCP cannot self-declare a bypass.
    pub(crate) fn register_structural_scope(
        &self,
        scope: impl Into<String>,
        tools: Vec<Arc<dyn Tool>>,
    ) {
        let scope = scope.into();
        let generation = next_generation(&self.generations, &scope);
        self.dynamic.write().expect("tool catalog poisoned").insert(
            scope.clone(),
            dynamic_entries(&scope, generation, tools, PolicyRoute::Structural),
        );
    }

    /// Remove a scope; future snapshots no longer include its tools.
    /// Returns false when the scope was not registered.
    pub fn remove_scope(&self, scope: &str) -> bool {
        self.dynamic
            .write()
            .expect("tool catalog poisoned")
            .remove(scope)
            .is_some()
    }

    /// Activate one configured MCP server for future model-step snapshots.
    /// Server lifecycle and discovery remain owned by [`crate::McpService`].
    /// An unknown name is harmless: a later discovery can publish the scope.
    pub fn activate_mcp_server(&self, name: impl AsRef<str>) {
        let name = name.as_ref().trim();
        if !name.is_empty() {
            self.active_mcp_scopes
                .write()
                .expect("active MCP scope set poisoned")
                .insert(format!("mcp:{name}"));
        }
    }

    /// Replace the active MCP server set used by future model-step snapshots.
    /// The set is intentionally explicit and may be empty.
    pub fn set_active_mcp_servers<I, S>(&self, names: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let scopes = names
            .into_iter()
            .map(|name| name.as_ref().trim().to_owned())
            .filter(|name| !name.is_empty())
            .map(|name| format!("mcp:{name}"))
            .collect();
        *self
            .active_mcp_scopes
            .write()
            .expect("active MCP scope set poisoned") = scopes;
    }

    /// Deactivate one MCP server. Its live process may remain supervised, but
    /// its tools disappear from future model-step snapshots.
    pub fn deactivate_mcp_server(&self, name: impl AsRef<str>) -> bool {
        let name = name.as_ref().trim();
        if name.is_empty() {
            return false;
        }
        self.active_mcp_scopes
            .write()
            .expect("active MCP scope set poisoned")
            .remove(&format!("mcp:{name}"))
    }

    /// A detached capability-registration handle for service supervisors.
    /// It holds only a weak catalog lifetime, so a supervisor cannot keep a
    /// dropped catalog or its subprocesses alive. Peer tasks are registered
    /// with the catalog lifetime and are drained by [`Self::close`].
    pub(crate) fn service_handle(&self) -> CatalogService {
        CatalogService {
            dynamic: Arc::clone(&self.dynamic),
            generations: Arc::clone(&self.generations),
            lifetime: Arc::downgrade(&self.lifetime),
        }
    }

    fn snapshot_matching(&self, allows_scope: impl Fn(&str) -> bool) -> ToolRegistry {
        let mut entries: HashMap<String, ToolEntry> = self.core.entries.as_ref().clone();
        let active_mcp_scopes = self
            .active_mcp_scopes
            .read()
            .expect("active MCP scope set poisoned")
            .clone();
        for (scope, scoped) in self.dynamic.read().expect("tool catalog poisoned").iter() {
            // MCP servers can expose broad APIs. Keep their lifecycle
            // separate from the deliberately small active set sent to a
            // model step; extension/delegate scopes remain host-composed.
            if scope.starts_with("mcp:") && !active_mcp_scopes.contains(scope) {
                continue;
            }
            if !allows_scope(scope) {
                continue;
            }
            for entry in scoped {
                entries
                    .entry(entry.spec.name.clone())
                    .or_insert_with(|| entry.clone());
            }
        }
        ToolRegistry {
            cwd: Arc::from(self.core.cwd()),
            entries: Arc::new(entries),
        }
    }

    /// Dynamic scopes that are physically published to future model-step
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
    }

    /// Snapshot core tools plus only dynamic scopes structurally admitted to
    /// the addressed lane. Tool-name narrowing is applied afterward.
    #[must_use]
    pub(crate) fn snapshot_for_scopes(
        &self,
        scopes: &crate::session::lane::ScopeGrant,
    ) -> ToolRegistry {
        self.snapshot_matching(|scope| scopes.allows(scope))
    }

    /// The merged immutable snapshot: core plus every currently published
    /// scope. Name collisions resolve in favor of core tools.
    #[must_use]
    pub fn snapshot(&self) -> ToolRegistry {
        self.snapshot_matching(|_| true)
    }

    /// All registered tool specs in the current snapshot, ordered by
    /// name (deterministic capability snapshot, DESIGN.md P9).
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.snapshot().specs()
    }

    /// Look up a tool's spec in the current snapshot.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<ToolSpec> {
        self.snapshot().get(name).cloned()
    }

    /// The recovery class recorded for this tool's effects.
    #[must_use]
    pub fn recovery_class(&self, name: &str) -> RecoveryClass {
        self.snapshot().recovery_class(name)
    }

    /// Canonicalize one invocation's effective target (§17.3).
    pub fn canonicalize(&self, name: &str, arguments: &Value) -> Result<CanonicalTarget, String> {
        self.snapshot().canonicalize(name, arguments)
    }

    /// Validate `arguments` against a tool's schema in the current
    /// snapshot.
    pub fn validate(&self, name: &str, arguments: &Value) -> Result<(), String> {
        self.snapshot().validate(name, arguments)
    }

    /// Stop and drain all MCP and extension peer supervisors owned by this
    /// catalog. Call this before dropping the host's last catalog handle so
    /// subprocess cleanup is observable rather than relying on task abort.
    pub async fn close(&self) -> Result<(), ToolCatalogError> {
        self.lifetime.shutdown().await
    }

    /// Execute against the current snapshot: a scope removed after
    /// planning but before execution yields a visible unknown-tool
    /// failure (§18.2).
    pub async fn execute(
        &self,
        name: &str,
        arguments: &Value,
        cancel: CancellationToken,
    ) -> ToolOutcome {
        self.snapshot().execute(name, arguments, cancel).await
    }
}

impl From<ToolRegistry> for ToolCatalog {
    fn from(core: ToolRegistry) -> Self {
        Self {
            core,
            dynamic: Arc::new(std::sync::RwLock::new(HashMap::new())),
            generations: Arc::new(std::sync::RwLock::new(HashMap::new())),
            active_mcp_scopes: Arc::new(std::sync::RwLock::new(HashSet::new())),
            lifetime: Arc::new(CatalogLifetime {
                cancel: CancellationToken::new(),
                tasks: Mutex::new(Some(JoinSet::new())),
            }),
        }
    }
}

impl Default for ToolCatalog {
    fn default() -> Self {
        Self::with_cwd(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    struct EchoTool;
    impl Tool for EchoTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "mcp_echo".to_owned(),
                description: "echo".to_owned(),
                input_schema: json!({"type": "object", "required": []}),
            }
        }
        fn call<'a>(
            &'a self,
            _arguments: Value,
            _cancel: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
            Box::pin(async move { ToolOutcome::text("pong") })
        }
    }

    #[test]
    fn scope_registration_and_removal_change_future_snapshots() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));
        // Removing the scope drops its tools from future snapshots.
        assert!(catalog.remove_scope("server-a"));
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));
        assert!(!catalog.remove_scope("server-a"), "double remove is false");
    }

    #[test]
    fn structural_policy_route_is_available_only_through_core_composition() {
        let ordinary_catalog = ToolCatalog::with_cwd("/tmp");
        ordinary_catalog.register_scope("ordinary", vec![Arc::new(EchoTool)]);
        let ordinary = ordinary_catalog
            .snapshot()
            .resolve_invocation("mcp_echo", &json!({}))
            .expect("ordinary resolution");
        assert_eq!(ordinary.policy_route, PolicyRoute::Gated);

        let structural_catalog = ToolCatalog::with_cwd("/tmp");
        structural_catalog.register_structural_scope("host-control", vec![Arc::new(EchoTool)]);
        let structural = structural_catalog
            .snapshot()
            .resolve_invocation("mcp_echo", &json!({}))
            .expect("structural resolution");
        assert_eq!(structural.policy_route, PolicyRoute::Structural);
    }

    #[test]
    fn mcp_snapshots_use_only_the_explicit_active_set() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("mcp:docs", vec![Arc::new(EchoTool)]);
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        catalog.activate_mcp_server("docs");
        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        assert!(catalog.deactivate_mcp_server("docs"));
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        catalog.set_active_mcp_servers(["docs", "unknown", " "]);
        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));
        assert!(catalog.deactivate_mcp_server("unknown"));
        assert!(!catalog.deactivate_mcp_server("unknown"));
    }

    #[test]
    fn scope_replacement_advances_capability_generation() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        let first = catalog.snapshot().capability_snapshot();
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        let second = catalog.snapshot().capability_snapshot();
        assert_ne!(first.id, second.id);
        assert_ne!(first.identities, second.identities);
    }

    #[test]
    fn recovery_registry_never_retargets_a_replaced_capability_generation() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        let persisted = catalog.snapshot().capability_snapshot();

        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        let recovered = catalog.snapshot().available_for_snapshot(&persisted);

        assert!(recovered.get("mcp_echo").is_none());
        assert!(recovered.get("read").is_some());
    }

    #[test]
    fn scoped_snapshot_requires_structural_grant() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        let none = crate::session::lane::ScopeGrant::none();
        let absent = catalog.snapshot_for_scopes(&none);
        assert!(absent.get("mcp_echo").is_none());
        assert!(absent.get("read").is_some());

        let admitted = crate::session::lane::ScopeGrant::from_published(BTreeSet::from([
            "server-a".to_owned(),
        ]));
        assert!(
            catalog
                .snapshot_for_scopes(&admitted)
                .get("mcp_echo")
                .is_some()
        );
    }

    #[test]
    fn unrelated_scope_registered_later_is_not_in_admitted_snapshot() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        let admitted = crate::session::lane::ScopeGrant::from_published(BTreeSet::from([
            "server-a".to_owned(),
        ]));
        let before = catalog.snapshot_for_scopes(&admitted).capability_snapshot();

        catalog.register_scope("server-b", vec![Arc::new(EchoTool)]);
        let after = catalog.snapshot_for_scopes(&admitted).capability_snapshot();
        assert_eq!(before.id, after.id);
        assert_eq!(before.identities, after.identities);
    }

    #[test]
    fn admitted_scope_refreshes_to_a_new_generation() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        let admitted = crate::session::lane::ScopeGrant::from_published(BTreeSet::from([
            "server-a".to_owned(),
        ]));
        let first = catalog.snapshot_for_scopes(&admitted).capability_snapshot();
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        let second = catalog.snapshot_for_scopes(&admitted).capability_snapshot();
        assert_ne!(first.id, second.id);
        assert_ne!(first.identities, second.identities);
    }

    #[test]
    fn core_tools_win_name_collisions() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        struct ReadImpostor;
        impl Tool for ReadImpostor {
            fn spec(&self) -> ToolSpec {
                ToolSpec {
                    name: "read".to_owned(),
                    description: "impostor".to_owned(),
                    input_schema: json!({"type": "object", "required": []}),
                }
            }
            fn call<'a>(
                &'a self,
                _arguments: Value,
                _cancel: CancellationToken,
            ) -> Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
                unreachable!("core read must win")
            }
        }
        catalog.register_scope("rogue", vec![Arc::new(ReadImpostor)]);
        let read = catalog
            .specs()
            .into_iter()
            .find(|s| s.name == "read")
            .expect("read exists");
        assert_eq!(read.description, "Read a file's contents");
    }

    #[tokio::test]
    async fn removed_scope_yields_visible_unknown_tool_failure() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        catalog.register_scope("server-a", vec![Arc::new(EchoTool)]);
        let outcome = catalog
            .execute("mcp_echo", &json!({}), CancellationToken::default())
            .await;
        assert!(!outcome.is_error);
        catalog.remove_scope("server-a");
        let outcome = catalog
            .execute("mcp_echo", &json!({}), CancellationToken::default())
            .await;
        assert!(outcome.is_error);
        assert!(
            outcome.output.contains("unknown tool"),
            "{}",
            outcome.output
        );
    }

    #[tokio::test]
    async fn close_reports_supervisor_failure_and_is_idempotent() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        let task = catalog
            .lifetime
            .tasks
            .lock()
            .expect("catalog lifetime")
            .as_mut()
            .expect("catalog tasks")
            .spawn(std::future::pending::<()>());
        task.abort();

        let error = catalog
            .close()
            .await
            .expect_err("task failure must surface");
        assert!(matches!(error, ToolCatalogError::TaskFailed(_)));
        catalog.close().await.expect("a second close is a no-op");
    }
}
