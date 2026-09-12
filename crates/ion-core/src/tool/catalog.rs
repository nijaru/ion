use super::*;
use std::collections::{BTreeMap, HashSet};
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
    configuration: crate::HostConfiguration,
    peer_revisions: Arc<std::sync::RwLock<Option<BTreeMap<String, u64>>>>,
    /// Structural scope identities owned by the host, independent of whether
    /// a peer currently has a live tool generation published.
    declared_scopes: Arc<std::sync::RwLock<BTreeSet<String>>>,
    dynamic: Arc<std::sync::RwLock<HashMap<String, Vec<ToolEntry>>>>,
    generations: Arc<std::sync::RwLock<HashMap<String, u64>>>,
    active_mcp_scopes: Arc<std::sync::RwLock<HashSet<String>>>,
    lifetime: Arc<CatalogLifetime>,
}

struct CatalogLifetime {
    cancel: CancellationToken,
    tasks: Mutex<Option<JoinSet<Result<(), crate::peer::PeerCleanupError>>>>,
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
        F: Future<Output = Result<(), crate::peer::PeerCleanupError>> + Send + 'static,
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
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        first_error.get_or_insert_with(|| err.to_string());
                    }
                    Err(err) => {
                        first_error.get_or_insert_with(|| err.to_string());
                    }
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
    declared_scopes: Arc<std::sync::RwLock<BTreeSet<String>>>,
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
    /// Declare the stable structural identity before discovery. A peer may be
    /// temporarily unavailable without turning its later generation into a
    /// newly ambient capability.
    pub(crate) fn declare_scope(&self, scope: String) {
        self.declared_scopes
            .write()
            .expect("tool catalog poisoned")
            .insert(scope);
    }

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
        F: Future<Output = Result<(), crate::peer::PeerCleanupError>> + Send + 'static,
    {
        self.lifetime
            .upgrade()
            .is_some_and(|lifetime| lifetime.spawn(task))
    }
}

impl ToolCatalog {
    /// Admission lifetime shared with every runtime composed by this host.
    pub fn configuration(&self) -> &crate::HostConfiguration {
        &self.configuration
    }

    #[must_use]
    pub fn with_configuration(mut self, configuration: crate::HostConfiguration) -> Self {
        self.configuration = configuration;
        self
    }

    /// Persist explicit peer configuration before changing availability. This
    /// host-derived view invalidates stale lane grants without mutating them.
    pub async fn reconcile_peer_authority(
        &self,
        store: &crate::SessionStore,
        desired: BTreeMap<String, String>,
        update: &crate::ConfigurationUpdate,
    ) -> Result<(), crate::StoreError> {
        if !update.belongs_to(&self.configuration) {
            return Err(crate::StoreError::Sqlite(
                "configuration guard belongs to another host".into(),
            ));
        }
        let workspace = self
            .cwd()
            .canonicalize()
            .map_err(|err| crate::StoreError::Sqlite(format!("workspace: {err}")))?
            .to_string_lossy()
            .into_owned();
        let revisions = store.reconcile_host_scopes(workspace, desired).await?;
        *self
            .peer_revisions
            .write()
            .expect("peer authority poisoned") = Some(revisions);
        Ok(())
    }

    /// The revision a lane grant must carry for `scope` to stay authoritative.
    /// Core scopes are unrevisioned. A catalog that has not reconciled host
    /// authority keeps peer scopes at their first revision, so client-scoped
    /// catalogs (ACP, tests) stay usable; a reconciled catalog denies any
    /// scope the host no longer configures.
    pub(crate) fn scope_revision(&self, scope: &str) -> Option<u64> {
        if !scope.starts_with("mcp:") && !scope.starts_with("ext:") {
            return Some(0);
        }
        match &*self.peer_revisions.read().expect("peer authority poisoned") {
            Some(revisions) => revisions.get(scope).copied(),
            None => Some(0),
        }
    }

    /// A catalog over `cwd` with only the core tool set.
    #[must_use]
    pub fn with_cwd(cwd: impl AsRef<Path>) -> Self {
        Self::with_cwd_and_sandbox(cwd, SandboxMode::Auto)
    }

    /// A catalog with an explicit native-shell enforcement mode.
    #[must_use]
    pub fn with_cwd_and_sandbox(cwd: impl AsRef<Path>, sandbox: SandboxMode) -> Self {
        Self::with_cwd_sandbox_and_paths(cwd, sandbox, WorkspacePolicy::Unrestricted)
    }

    /// A catalog with an explicit native-shell enforcement mode and
    /// workspace path policy.
    #[must_use]
    pub fn with_cwd_sandbox_and_paths(
        cwd: impl AsRef<Path>,
        sandbox: SandboxMode,
        paths: WorkspacePolicy,
    ) -> Self {
        Self::from(ToolRegistry::with_cwd_sandbox_and_paths(
            cwd, sandbox, paths,
        ))
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

    /// The workspace path policy the core registry resolves under.
    #[must_use]
    pub fn paths(&self) -> WorkspacePolicy {
        self.core.paths()
    }

    /// Register tools under `scope`, replacing that scope's previous
    /// registration. Publishing at a safe context boundary is the
    /// caller's contract (§19.2).
    pub fn register_scope(&self, scope: impl Into<String>, tools: Vec<Arc<dyn Tool>>) {
        let scope = scope.into();
        self.declared_scopes
            .write()
            .expect("tool catalog poisoned")
            .insert(scope.clone());
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
    /// remains declared: temporary peer loss must not revoke lane authority.
    /// Returns false when the scope was not registered.
    pub fn remove_scope(&self, scope: &str) -> bool {
        self.dynamic
            .write()
            .expect("tool catalog poisoned")
            .remove(scope)
            .is_some()
    }

    /// Set the host-selected MCP server set used by future model-step
    /// snapshots. Current composition calls this before session admission;
    /// durable lane authority remains the structural grant, not this live
    /// publication filter. The set is intentionally explicit and may be empty.
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

    /// A detached capability-registration handle for service supervisors.
    /// It holds only a weak catalog lifetime, so a supervisor cannot keep a
    /// dropped catalog or its subprocesses alive. Peer tasks are registered
    /// with the catalog lifetime and are drained by [`Self::close`].
    pub(crate) fn service_handle(&self) -> CatalogService {
        CatalogService {
            declared_scopes: Arc::clone(&self.declared_scopes),
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
            paths: self.core.paths(),
            entries: Arc::new(entries),
        }
    }

    /// Structural identities the current host may admit to a lane. This is
    /// deliberately independent of live discovery: a configured peer that is
    /// temporarily unavailable keeps the same authority across restart, while
    /// model-step snapshots still require a live generation.
    #[must_use]
    pub(crate) fn admission_scopes(&self) -> BTreeMap<String, u64> {
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
            .filter_map(|scope| {
                self.scope_revision(scope)
                    .map(|revision| (scope.clone(), revision))
            })
            .collect()
    }

    /// Snapshot core tools plus only dynamic scopes structurally admitted to
    /// the addressed lane. Tool-name narrowing is applied afterward.
    #[must_use]
    pub(crate) fn snapshot_for_scopes(
        &self,
        scopes: &crate::session::lane::ScopeGrant,
    ) -> ToolRegistry {
        self.snapshot_matching(|scope| {
            self.scope_revision(scope)
                .is_some_and(|revision| scopes.allows(scope, revision))
        })
    }

    /// The merged immutable snapshot: core plus every currently published
    /// scope. Name collisions resolve in favor of core tools.
    #[must_use]
    pub fn snapshot(&self) -> ToolRegistry {
        self.snapshot_matching(|scope| self.scope_revision(scope).is_some())
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

    /// Execute with a live progress stream (§16.1). Same capability
    /// snapshot as [`Self::execute`]; used by user shell passthrough so
    /// its streaming matches model-initiated tool execution.
    pub(crate) async fn execute_with_progress(
        &self,
        name: &str,
        arguments: &Value,
        cancel: CancellationToken,
        progress: Option<crate::tool::ToolProgressSender>,
    ) -> ToolOutcome {
        self.snapshot()
            .execute_with_progress(name, arguments, cancel, progress)
            .await
    }
}

impl From<ToolRegistry> for ToolCatalog {
    fn from(core: ToolRegistry) -> Self {
        Self {
            core,
            configuration: crate::HostConfiguration::default(),
            peer_revisions: Arc::new(std::sync::RwLock::new(None)),
            declared_scopes: Arc::new(std::sync::RwLock::new(BTreeSet::new())),
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
    fn declared_scope_survives_transient_unpublication() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        let service = catalog.service_handle();
        service.declare_scope("server-a".to_owned());
        let admitted = crate::session::lane::ScopeGrant::from_published(catalog.admission_scopes());
        assert!(
            catalog
                .snapshot_for_scopes(&admitted)
                .get("mcp_echo")
                .is_none()
        );

        service.register_scope("server-a".to_owned(), vec![Arc::new(EchoTool)]);
        assert!(
            catalog
                .snapshot_for_scopes(&admitted)
                .get("mcp_echo")
                .is_some()
        );
        assert!(service.remove_scope("server-a"));
        assert!(
            catalog
                .snapshot_for_scopes(&admitted)
                .get("mcp_echo")
                .is_none()
        );
        assert!(catalog.admission_scopes().contains_key("server-a"));

        service.register_scope("server-a".to_owned(), vec![Arc::new(EchoTool)]);
        assert!(
            catalog
                .snapshot_for_scopes(&admitted)
                .get("mcp_echo")
                .is_some()
        );
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

        catalog.set_active_mcp_servers(["docs"]);
        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        catalog.set_active_mcp_servers(std::iter::empty::<&str>());
        assert!(!catalog.specs().iter().any(|s| s.name == "mcp_echo"));

        catalog.set_active_mcp_servers(["docs", "unknown", " "]);
        assert!(catalog.specs().iter().any(|s| s.name == "mcp_echo"));
        assert!(catalog.admission_scopes().contains_key("mcp:docs"));
        assert!(!catalog.admission_scopes().contains_key("mcp:unknown"));
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

        let admitted = crate::session::lane::ScopeGrant::from_published(BTreeMap::from([(
            "server-a".to_owned(),
            0,
        )]));
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
        let admitted = crate::session::lane::ScopeGrant::from_published(BTreeMap::from([(
            "server-a".to_owned(),
            0,
        )]));
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
        let admitted = crate::session::lane::ScopeGrant::from_published(BTreeMap::from([(
            "server-a".to_owned(),
            0,
        )]));
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
        assert_eq!(
            read.description,
            "Read a file's contents. Images (png, jpg, gif, webp, bmp) are returned \
             as image attachments the model can see; absolute paths are readable."
        );
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
    async fn close_reports_returned_peer_cleanup_failure() {
        let catalog = ToolCatalog::with_cwd("/tmp");
        assert!(
            catalog
                .service_handle()
                .spawn(async { Err(crate::peer::PeerCleanupError::DrainTimeout) })
        );
        let error = catalog
            .close()
            .await
            .expect_err("cleanup failure must propagate");
        assert!(
            matches!(error, ToolCatalogError::TaskFailed(message) if message.contains("shutdown deadline"))
        );
    }

    #[tokio::test]
    async fn peer_removal_and_readd_do_not_resurrect_dormant_lane_grants() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::SessionStore::open_in_memory().unwrap();
        let catalog = ToolCatalog::with_cwd(root.path());
        let desired = BTreeMap::from([("ext:echo".to_owned(), "definition-a".to_owned())]);
        let update = catalog.configuration().try_update().unwrap();
        catalog
            .reconcile_peer_authority(&store, desired.clone(), &update)
            .await
            .unwrap();
        catalog.register_scope("ext:echo", vec![Arc::new(EchoTool)]);
        update.finish();
        let runtime = crate::Runtime::start_with_store(
            crate::ScriptedProvider::echo(),
            catalog.clone(),
            store.clone(),
        );
        let session = runtime.session();
        let id = runtime.session_id();
        session.create_lane("sibling").await.unwrap();
        session.close().await.unwrap();
        runtime.join().await.unwrap();
        let original = store.load(id).await.unwrap();
        assert!(original.lanes.iter().all(|lane| {
            catalog
                .snapshot_for_scopes(&lane.config.scopes)
                .get("mcp_echo")
                .is_some()
        }));
        let update = catalog.configuration().try_update().unwrap();
        catalog
            .reconcile_peer_authority(&store, BTreeMap::new(), &update)
            .await
            .unwrap();
        assert!(catalog.snapshot().get("mcp_echo").is_none());
        update.finish();
        // Reopen the host after removal; the tombstone survives reconfiguration.
        let reopened_catalog = ToolCatalog::with_cwd(root.path());
        let update = reopened_catalog.configuration().try_update().unwrap();
        reopened_catalog
            .reconcile_peer_authority(&store, desired, &update)
            .await
            .unwrap();
        reopened_catalog.register_scope("ext:echo", vec![Arc::new(EchoTool)]);
        update.finish();
        let dormant = store.load(id).await.unwrap();
        assert!(dormant.lanes.iter().all(|lane| {
            reopened_catalog
                .snapshot_for_scopes(&lane.config.scopes)
                .get("mcp_echo")
                .is_none()
        }));
        // Restrict a dormant main lane, then explicitly adopt new authority.
        let mut main = dormant
            .lanes
            .iter()
            .find(|lane| lane.name == "main")
            .unwrap()
            .config
            .clone();
        main.tools = ToolSelection::Only(BTreeSet::from(["read".to_owned()]));
        store
            .set_lane_config(id, "main", main.clone())
            .await
            .unwrap();
        let runtime = crate::Runtime::open_session(
            crate::ScriptedProvider::echo(),
            reopened_catalog.clone(),
            store.clone(),
            id,
        )
        .await
        .unwrap();
        runtime
            .session()
            .admit_structural_scope("ext:echo")
            .await
            .unwrap();
        let loaded = store.load(id).await.unwrap();
        let adopted = &loaded
            .lanes
            .iter()
            .find(|lane| lane.name == "main")
            .unwrap()
            .config;
        assert_eq!(adopted.tools, main.tools);
        assert!(
            reopened_catalog
                .snapshot_for_scopes(&adopted.scopes)
                .get("mcp_echo")
                .is_some()
        );
        assert!(
            reopened_catalog
                .snapshot_for_scopes(&adopted.scopes)
                .selected(&adopted.tools)
                .get("mcp_echo")
                .is_none()
        );
        let sibling = &loaded
            .lanes
            .iter()
            .find(|lane| lane.name == "sibling")
            .unwrap()
            .config;
        assert!(
            reopened_catalog
                .snapshot_for_scopes(&sibling.scopes)
                .get("mcp_echo")
                .is_none()
        );
        runtime.session().close().await.unwrap();
        runtime.join().await.unwrap();
        catalog.close().await.unwrap();
        reopened_catalog.close().await.unwrap();
    }

    #[tokio::test]
    async fn failed_authority_write_keeps_the_previous_revision_and_configuration() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::SessionStore::open_in_memory().unwrap();
        let catalog = ToolCatalog::with_cwd(root.path());
        let desired = BTreeMap::from([("ext:echo".to_owned(), "a".to_owned())]);
        let update = catalog.configuration().try_update().unwrap();
        catalog
            .reconcile_peer_authority(&store, desired, &update)
            .await
            .unwrap();
        catalog.register_scope("ext:echo", vec![Arc::new(EchoTool)]);
        update.finish();
        let grant = crate::session::lane::ScopeGrant::from_published(catalog.admission_scopes());
        let update = catalog.configuration().try_update().unwrap();
        store.fail_next_write();
        assert!(
            catalog
                .reconcile_peer_authority(&store, BTreeMap::new(), &update)
                .await
                .is_err()
        );
        update.unchanged();
        assert!(catalog.configuration().try_enter().is_ok());
        assert!(
            catalog
                .snapshot_for_scopes(&grant)
                .get("mcp_echo")
                .is_some()
        );
        let update = catalog.configuration().try_update().unwrap();
        catalog
            .reconcile_peer_authority(
                &store,
                BTreeMap::from([("ext:echo".to_owned(), "replacement".to_owned())]),
                &update,
            )
            .await
            .unwrap();
        update.finish();
        assert!(
            catalog
                .snapshot_for_scopes(&grant)
                .get("mcp_echo")
                .is_none()
        );
        catalog.close().await.unwrap();
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
            .spawn(std::future::pending::<
                Result<(), crate::peer::PeerCleanupError>,
            >());
        task.abort();

        let error = catalog
            .close()
            .await
            .expect_err("task failure must surface");
        assert!(matches!(error, ToolCatalogError::TaskFailed(_)));
        catalog.close().await.expect("a second close is a no-op");
    }
}
