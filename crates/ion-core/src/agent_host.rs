//! Hosted fresh/fork agent runtime composition.
//!
//! Durable agent identity, topology, status, waiting, cancellation, and exact
//! results belong to `agent::Family`. This module owns only process-local
//! provider/runtime/catalog residency for agents whose history lives in a
//! separate session. Closing residency never deletes durable agent state.
//!

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::context::{ContextMessage, ContextPlan, TrustedResource, project};
use crate::ids::{AgentId, OperationId, SessionId};
use crate::provider::Provider;
use crate::runtime::{Runtime, RuntimeBudget};
use crate::session::OperationState;
use crate::store::SessionStore;
use crate::tool::{
    Tool, ToolOutcome, ToolProgress, ToolProgressSender, ToolSpec, bounded_progress_output,
};

/// Conservative default bounds for separately hosted agents (§20.5): exact numbers are
/// host configuration; these exist so hosts that do not tune budgets
/// still cannot loop forever.
#[must_use]
pub fn hosted_agent_budget_default() -> RuntimeBudget {
    RuntimeBudget {
        max_model_steps: Some(16),
        max_tool_calls: Some(64),
    }
}

/// How a separately hosted agent receives parent context. `Fresh` is the safe default; the
/// parent transcript is never copied unless the caller explicitly selects
/// `ForkContext`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostedHistory {
    Fresh,
    ForkContext,
}

/// One requested separately hosted agent execution.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostedAgentSpec {
    pub objective: String,
    /// Explicit context seed appended after the objective (§13.4):
    /// never an implicit copy of parent state.
    pub context_seed: Option<String>,
    /// Explicit parent-context projection mode (§13.4).
    pub context_mode: HostedHistory,
    /// Optional host-resolved model for this agent only.
    pub model_override: Option<String>,
}

/// Configuration and bounds for separately hosted agents in one family.
pub struct HostedAgentConfig<P> {
    pub store: SessionStore,
    pub make_provider: Arc<dyn Fn() -> P + Send + Sync>,
    /// Optional resolver for explicit per-call model overrides. Unsupported
    /// overrides fail visibly instead of silently using the launch model.
    pub make_provider_for_model: Option<Arc<dyn Fn(String) -> P + Send + Sync>>,
    /// Maximum number of live hosted runtimes retained by this service.
    pub max_active: usize,
    /// Budget applied to every hosted agent operation.
    pub budget: RuntimeBudget,
    /// Explicitly inherited project resources; empty when the host did not
    /// grant project trust.
    pub trusted_resources: Vec<TrustedResource>,
    /// Host-owned workspace root used for the hosted catalog and durable
    /// session identity.
    pub cwd: PathBuf,
}

struct HostedRuntime {
    session: crate::runtime::SessionHandle,
    runtime: Option<Runtime>,
    catalog: crate::tool::ToolCatalog,
    cancel: CancellationToken,
    cancel_watch: JoinHandle<()>,
}

/// Process-owned residency for separately hosted fresh/fork agents.
/// Family/store state is authoritative; this registry owns only live runtime
/// resources and registers their `SessionHandle`s with the family authority.
pub struct HostedAgentRuntimes<P> {
    config: Arc<HostedAgentConfig<P>>,
    parent_id: SessionId,
    runtimes: Mutex<HashMap<SessionId, HostedRuntime>>,
    family: Mutex<Option<Arc<crate::agent::Family>>>,
}

/// Construct the process-owned fresh/fork runtime residency for one root family.
pub fn hosted_agent_runtimes<P: Provider + 'static>(
    config: HostedAgentConfig<P>,
    parent_id: SessionId,
) -> Arc<HostedAgentRuntimes<P>> {
    HostedAgentRuntimes::new(Arc::new(config), parent_id)
}

impl<P> HostedAgentRuntimes<P> {
    fn new(config: Arc<HostedAgentConfig<P>>, parent_id: SessionId) -> Arc<Self> {
        Arc::new(Self {
            config,
            parent_id,
            runtimes: Mutex::new(HashMap::new()),
            family: Mutex::new(None),
        })
    }

    fn bind_family(&self, family: Arc<crate::agent::Family>) {
        debug_assert_eq!(
            SessionId::from_uuid(family.root().as_uuid()),
            self.parent_id,
            "hosted runtime service must bind to its durable root family",
        );
        *self.family.lock().expect("hosted agent runtimes poisoned") = Some(Arc::clone(&family));
        let live = self
            .runtimes
            .lock()
            .expect("hosted agent runtimes poisoned")
            .iter()
            .map(|(session_id, hosted)| (*session_id, hosted.session.clone()))
            .collect::<Vec<_>>();
        for (session_id, session) in live {
            family.register_hosted_session(session_id, session);
        }
    }

    fn family(&self) -> Option<Arc<crate::agent::Family>> {
        self.family
            .lock()
            .expect("hosted agent runtimes poisoned")
            .clone()
    }

    fn is_live(&self, session_id: SessionId) -> bool {
        self.runtimes
            .lock()
            .expect("hosted agent runtimes poisoned")
            .contains_key(&session_id)
    }

    /// Remove one live incarnation from the registry, then drain every owned
    /// resource without holding the residency mutex. The durable session remains
    /// in SQLite and stays observable/resumable by semantic agent address.
    async fn release_hosted_runtime(&self, session_id: SessionId) -> Result<(), String> {
        let Some(mut hosted) = self
            .runtimes
            .lock()
            .expect("hosted agent runtimes poisoned")
            .remove(&session_id)
        else {
            return Ok(());
        };
        if let Some(family) = self.family() {
            family.unregister_hosted_session(session_id);
        }
        hosted.cancel.cancel();
        let mut first_error = None;
        if let Err(err) = hosted.session.close().await {
            first_error.get_or_insert_with(|| format!("hosted agent close failed: {err}"));
        }
        if let Some(runtime) = hosted.runtime.take()
            && let Err(err) = runtime.join().await
        {
            first_error.get_or_insert_with(|| format!("hosted agent join failed: {err}"));
        }
        if let Err(err) = hosted.catalog.close().await {
            first_error.get_or_insert_with(|| format!("hosted agent catalog close failed: {err}"));
        }
        if let Err(err) = hosted.cancel_watch.await {
            first_error
                .get_or_insert_with(|| format!("hosted agent cancellation watcher failed: {err}"));
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Reclaim completed live runtimes before applying the hosted-runtime bound.
    /// Completion comes from the durable store, not from task liveness.
    async fn reap_finished(&self) -> Result<(), String> {
        let ids: Vec<SessionId> = self
            .runtimes
            .lock()
            .expect("hosted agent runtimes poisoned")
            .keys()
            .copied()
            .collect();
        for session_id in ids {
            let loaded = self.config.store.load(session_id).await.map_err(|err| {
                format!("hosted agent {session_id} unavailable while reaping: {err}")
            })?;
            let finished = loaded
                .operations
                .iter()
                .max_by_key(|operation| operation.accepted_seq)
                .is_some_and(|operation| {
                    matches!(operation.latest.1.state, OperationState::Finished(_))
                });
            if finished {
                self.release_hosted_runtime(session_id).await?;
            }
        }
        Ok(())
    }

    async fn initial_lane_config(
        &self,
        model_ref: &str,
    ) -> Result<crate::session::lane::Config, String> {
        let loaded = self
            .config
            .store
            .load(self.parent_id)
            .await
            .map_err(|err| format!("agent family configuration unavailable: {err}"))?;
        let root = AgentId::root(self.parent_id);
        let parent = loaded
            .agents
            .iter()
            .find(|agent| agent.id == root)
            .ok_or_else(|| "agent family root is missing".to_owned())?;
        let lane = loaded
            .lanes
            .iter()
            .find(|lane| lane.name == parent.lane_name)
            .ok_or_else(|| "agent family root lane is missing".to_owned())?;
        let mut config = lane.config.clone();
        config.model_ref = model_ref.to_owned();
        config.tools = config
            .tools
            .narrowed_by(&crate::tool::ToolSelection::read_only());
        Ok(config)
    }

    async fn spawn(
        self: &Arc<Self>,
        spec: HostedAgentSpec,
        parent_cancel: CancellationToken,
        progress: Option<&ToolProgressSender>,
    ) -> Result<AgentId, String>
    where
        P: Provider,
    {
        if parent_cancel.is_cancelled() {
            return Err("cancelled".to_owned());
        }
        self.reap_finished().await?;
        {
            let runtimes = self
                .runtimes
                .lock()
                .expect("hosted agent runtimes poisoned");
            if runtimes.len() >= self.config.max_active.max(1) {
                return Err(format!(
                    "hosted agent runtime limit reached (max {})",
                    self.config.max_active.max(1)
                ));
            }
        }
        let provider = match spec.model_override.as_deref() {
            Some(model_ref) => {
                let Some(make_provider) = self.config.make_provider_for_model.as_ref() else {
                    return Err(format!("model override `{model_ref}` is unavailable"));
                };
                make_provider(model_ref.to_owned())
            }
            None => (self.config.make_provider)(),
        };
        let fork_context = match spec.context_mode {
            HostedHistory::Fresh => None,
            HostedHistory::ForkContext => Some(
                fork_context(&self.config.store, self.parent_id)
                    .await
                    .map_err(|err| format!("could not load parent context: {err}"))?,
            ),
        };
        let prompt = compose_hosted_prompt(
            &spec,
            fork_context
                .as_ref()
                .and_then(|fork| fork.rendered.as_deref()),
        );
        let fork_source = fork_context
            .as_ref()
            .map(|fork| (self.parent_id, fork.source_entry_id));
        let catalog = crate::tool::ToolCatalog::read_only(&self.config.cwd);
        let session_id = SessionId::generate();
        let agent_id = crate::ids::AgentId::root(session_id);
        let initial_model_ref = provider.initial_model_ref();
        let lane_config = self.initial_lane_config(&initial_model_ref).await?;
        self.config
            .store
            .admit_session_agent(
                crate::store::SessionRecord {
                    id: session_id,
                    cwd: self.config.cwd.to_string_lossy().into_owned(),
                    title: String::new(),
                    initial_model_ref,
                    control_parent_session_id: Some(self.parent_id),
                    fork_source_session_id: fork_source.map(|(session_id, _)| session_id),
                    fork_source_entry_id: fork_source.and_then(|(_, entry_id)| entry_id),
                },
                crate::ids::AgentId::root(self.parent_id),
                lane_config,
            )
            .await
            .map_err(|err| format!("agent admission failed: {err}"))?;
        let runtime = match Runtime::open_hosted(
            provider,
            catalog.clone(),
            self.config.store.clone(),
            session_id,
            crate::runtime::HostedRuntimeConfig {
                policy: Arc::new(crate::policy::DefaultPolicy),
                budget: self.config.budget,
                control_parent: self.parent_id,
                trusted_resources: self.config.trusted_resources.clone(),
            },
        )
        .await
        {
            Ok(runtime) => runtime,
            Err(err) => {
                let _ = catalog.close().await;
                return Err(format!(
                    "agent {agent_id} was admitted, but runtime open failed: {err}"
                ));
            }
        };
        let session = runtime.session();
        if let Some(family) = self.family() {
            family.register_hosted_session(session_id, session.clone());
        }
        let operation_id = match session.submit_if_idle(prompt).await {
            Ok(operation_id) => operation_id,
            Err(err) => {
                if let Some(family) = self.family() {
                    family.unregister_hosted_session(session_id);
                }
                let _ = session.close().await;
                let _ = runtime.join().await;
                let _ = catalog.close().await;
                return Err(format!(
                    "agent {agent_id} was admitted, but start failed: {err}"
                ));
            }
        };
        let cancel = parent_cancel.child_token();
        let cancel_for_watch = cancel.clone();
        let session_for_watch = session.clone();
        let cancel_watch = tokio::spawn(async move {
            cancel_for_watch.cancelled().await;
            let _ = session_for_watch.cancel(operation_id).await;
        });
        let objective = spec.objective;
        self.runtimes
            .lock()
            .expect("hosted agent runtimes poisoned")
            .insert(
                session_id,
                HostedRuntime {
                    session,
                    runtime: Some(runtime),
                    catalog,
                    cancel,
                    cancel_watch,
                },
            );
        report_progress(progress, format!("agent {agent_id} accepted: {objective}")).await;
        Ok(agent_id)
    }

    async fn resume(
        self: &Arc<Self>,
        agent_id: AgentId,
        session_id: SessionId,
        parent_cancel: CancellationToken,
        progress: Option<&ToolProgressSender>,
    ) -> Result<(), String>
    where
        P: Provider,
    {
        if self.is_live(session_id) {
            return Ok(());
        }
        let loaded = self
            .config
            .store
            .load(session_id)
            .await
            .map_err(|err| format!("agent {agent_id} unavailable: {err}"))?;
        let belongs_to_family = loaded.agents.iter().any(|agent| {
            agent.id == agent_id
                && agent.family_session_id == self.parent_id
                && agent.session_id == session_id
        });
        if loaded.session.control_parent_session_id != Some(self.parent_id) || !belongs_to_family {
            return Err(format!("agent {agent_id} is not owned by this family"));
        }
        let Some(operation) = loaded.operations.iter().max_by_key(|op| op.accepted_seq) else {
            return Err(format!("agent {agent_id} has no operation to resume"));
        };
        if matches!(operation.latest.1.state, OperationState::Finished(_)) {
            return Ok(());
        }
        let model_ref = loaded.session.initial_model_ref.clone();
        let provider = match self.config.make_provider_for_model.as_ref() {
            Some(make_provider) => make_provider(model_ref.clone()),
            None => {
                let provider = (self.config.make_provider)();
                if !provider.supports_model(&model_ref) {
                    return Err(format!(
                        "model `{model_ref}` is unavailable for this hosted agent"
                    ));
                }
                provider
            }
        };
        let catalog = crate::tool::ToolCatalog::read_only(&loaded.session.cwd);
        let runtime = Runtime::open_hosted(
            provider,
            catalog.clone(),
            self.config.store.clone(),
            session_id,
            crate::runtime::HostedRuntimeConfig {
                policy: Arc::new(crate::policy::DefaultPolicy),
                budget: self.config.budget,
                control_parent: self.parent_id,
                trusted_resources: self.config.trusted_resources.clone(),
            },
        )
        .await
        .map_err(|err| format!("agent {agent_id} resume failed: {err}"))?;
        let session = runtime.session();
        if let Some(family) = self.family() {
            family.register_hosted_session(session_id, session.clone());
        }
        let cancel = parent_cancel.child_token();
        let cancel_for_watch = cancel.clone();
        let session_for_watch = session.clone();
        let operation_id = operation.id;
        let cancel_watch = tokio::spawn(async move {
            cancel_for_watch.cancelled().await;
            let _ = session_for_watch.cancel(operation_id).await;
        });
        self.runtimes
            .lock()
            .expect("hosted agent runtimes poisoned")
            .insert(
                session_id,
                HostedRuntime {
                    session,
                    runtime: Some(runtime),
                    catalog,
                    cancel,
                    cancel_watch,
                },
            );
        report_progress(progress, format!("agent {agent_id} resumed")).await;
        Ok(())
    }

    pub async fn close(&self) -> Result<(), String> {
        let ids: Vec<SessionId> = self
            .runtimes
            .lock()
            .expect("hosted agent runtimes poisoned")
            .keys()
            .copied()
            .collect();
        let mut first_error = None;
        for session_id in ids {
            if let Err(err) = self.release_hosted_runtime(session_id).await {
                first_error.get_or_insert(err);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[derive(Clone, Copy)]
enum HostAgentToolKind {
    Spawn,
    Start,
    Status,
    Wait,
    Cancel,
    Resume,
    Send,
}

struct HostAgentTool<P> {
    family: Arc<crate::agent::Family>,
    hosted: Arc<HostedAgentRuntimes<P>>,
    kind: HostAgentToolKind,
}

/// Compose one model-facing agent control namespace across shared-history
/// lane agents and separately hosted fresh/fork runtimes. Durable control is
/// always family-owned; this layer contributes only host residency mechanics.
pub fn agent_host_tools<P: Provider + 'static>(
    family: Arc<crate::agent::Family>,
    hosted: Arc<HostedAgentRuntimes<P>>,
) -> Vec<Arc<dyn Tool>> {
    hosted.bind_family(Arc::clone(&family));
    [
        HostAgentToolKind::Spawn,
        HostAgentToolKind::Start,
        HostAgentToolKind::Status,
        HostAgentToolKind::Wait,
        HostAgentToolKind::Cancel,
        HostAgentToolKind::Resume,
        HostAgentToolKind::Send,
    ]
    .into_iter()
    .map(|kind| {
        Arc::new(HostAgentTool {
            family: Arc::clone(&family),
            hosted: Arc::clone(&hosted),
            kind,
        }) as Arc<dyn Tool>
    })
    .collect()
}

const AGENT_SCOPE: &str = "agents";

/// Publish the unified agent namespace as structural host capabilities.
pub async fn install_agent_host_tools<P: Provider + 'static>(
    catalog: &crate::tool::ToolCatalog,
    runtime: &Runtime,
    family: Arc<crate::agent::Family>,
    hosted: Arc<HostedAgentRuntimes<P>>,
) -> Result<(), crate::agent::Error> {
    catalog.register_structural_scope(AGENT_SCOPE, agent_host_tools(family, hosted));
    runtime.admit_structural_scope(AGENT_SCOPE).await?;
    Ok(())
}

impl<P: Provider + 'static> Tool for HostAgentTool<P> {
    fn spec(&self) -> ToolSpec {
        match self.kind {
            HostAgentToolKind::Spawn => ToolSpec {
                name: "spawn_agent".to_owned(),
                description: "Admit an agent with lane, fresh, or fork topology and start it when applicable. Returns one durable agent handle.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "objective": {"type": "string"},
                        "topology": {"type": "string", "enum": ["lane", "fresh", "fork"]},
                        "context": {"type": "string", "description": "optional explicit context seed for fresh/fork agents"},
                        "model_override": {"type": "string", "description": "optional host-resolved model for fresh/fork agents"}
                    },
                    "required": ["objective"]
                }),
            },
            HostAgentToolKind::Start => ToolSpec {
                name: "agent_start".to_owned(),
                description: "Start a previously admitted idle lane agent with an objective.".to_owned(),
                input_schema: host_agent_handle_schema(Some(("objective", "string"))),
            },
            HostAgentToolKind::Status => ToolSpec {
                name: "agent_status".to_owned(),
                description: "Inspect durable status and the latest exact result for any retained agent.".to_owned(),
                input_schema: host_agent_handle_schema(None),
            },
            HostAgentToolKind::Wait => ToolSpec {
                name: "agent_wait".to_owned(),
                description: "Wait for an agent's current durable operation and return its exact result.".to_owned(),
                input_schema: host_agent_handle_schema(None),
            },
            HostAgentToolKind::Cancel => ToolSpec {
                name: "agent_cancel".to_owned(),
                description: "Cancel the running operation of a retained agent.".to_owned(),
                input_schema: host_agent_handle_schema(None),
            },
            HostAgentToolKind::Resume => ToolSpec {
                name: "agent_resume".to_owned(),
                description: "Reattach a non-terminal fresh/fork agent after process loss.".to_owned(),
                input_schema: host_agent_handle_schema(None),
            },
            HostAgentToolKind::Send => ToolSpec {
                name: "agent_send".to_owned(),
                description: "Send durable input from the root to a retained lane agent.".to_owned(),
                input_schema: host_agent_handle_schema(Some(("message", "string"))),
            },
        }
    }

    fn call<'a>(
        &'a self,
        arguments: Value,
        cancel: CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        self.call_with_progress(arguments, cancel, None)
    }

    fn call_with_progress<'a>(
        &'a self,
        arguments: Value,
        cancel: CancellationToken,
        progress: Option<ToolProgressSender>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ToolOutcome> + Send + 'a>> {
        Box::pin(async move {
            match self.kind {
                HostAgentToolKind::Spawn => {
                    let spec = match parse_agent_spawn(&arguments) {
                        Ok(spec) => spec,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match spec.topology {
                        AgentTopology::Lane => {
                            if spec.context_seed.is_some() || spec.model_override.is_some() {
                                return ToolOutcome::error(
                                    "lane topology does not accept `context` or `model_override`",
                                );
                            }
                            let agent_id = match self.family.admit_lane(self.family.root()).await {
                                Ok(agent_id) => agent_id,
                                Err(err) => {
                                    return ToolOutcome::error(format!(
                                        "agent admission failed: {err}"
                                    ));
                                }
                            };
                            match self.family.start(agent_id, spec.objective).await {
                                Ok(operation_id) => ToolOutcome::text(format!(
                                    "agent handle: {agent_id}\nstarted: {operation_id}"
                                )),
                                Err(crate::agent::Error::Capacity) => ToolOutcome::text(format!(
                                    "agent handle: {agent_id}\nadmitted; execution capacity is exhausted; use agent_start later"
                                )),
                                Err(err) => ToolOutcome::error(format!(
                                    "agent handle: {agent_id}\nadmitted, but start failed: {err}"
                                )),
                            }
                        }
                        AgentTopology::Fresh | AgentTopology::Fork => {
                            let hosted_spec = HostedAgentSpec {
                                objective: spec.objective,
                                context_seed: spec.context_seed,
                                context_mode: match spec.topology {
                                    AgentTopology::Fresh => HostedHistory::Fresh,
                                    AgentTopology::Fork => HostedHistory::ForkContext,
                                    AgentTopology::Lane => unreachable!(),
                                },
                                model_override: spec.model_override,
                            };
                            match self
                                .hosted
                                .spawn(hosted_spec, cancel, progress.as_ref())
                                .await
                            {
                                Ok(agent_id) => {
                                    ToolOutcome::text(format!("agent handle: {agent_id}\nstarted"))
                                }
                                Err(err) => {
                                    ToolOutcome::error(format!("agent admission failed: {err}"))
                                }
                            }
                        }
                    }
                }
                HostAgentToolKind::Start => {
                    let agent_id = match parse_host_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    let objective = match host_string_arg(&arguments, "objective") {
                        Ok(value) => value.to_owned(),
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.target(agent_id).await {
                        Ok(crate::agent::AgentTarget::SharedHistory { .. }) => {
                            match self.family.start(agent_id, objective).await {
                                Ok(operation_id) => ToolOutcome::text(format!(
                                    "agent {agent_id} started: {operation_id}"
                                )),
                                Err(err) => ToolOutcome::error(err.to_string()),
                            }
                        }
                        Ok(crate::agent::AgentTarget::SeparateSession { .. }) => {
                            ToolOutcome::error(
                                "agent_start currently applies only to admitted lane agents; fresh/fork agents start at admission and use agent_resume after process loss",
                            )
                        }
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
                HostAgentToolKind::Status => {
                    let agent_id = match parse_host_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.observe(agent_id).await {
                        Ok(observation) => {
                            ToolOutcome::text(render_family_observation(&observation))
                        }
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
                HostAgentToolKind::Wait => {
                    let agent_id = match parse_host_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    let target = match self.family.target(agent_id).await {
                        Ok(target) => target,
                        Err(err) => return ToolOutcome::error(err.to_string()),
                    };
                    let status = match self.family.wait_one(agent_id, cancel, None).await {
                        Ok(status) => status,
                        Err(err) => return ToolOutcome::error(err.to_string()),
                    };
                    let operation_id = host_status_operation_id(&status)
                        .expect("family wait rejects admitted agents");
                    let observation =
                        match self.family.observe_operation(agent_id, operation_id).await {
                            Ok(observation) => observation,
                            Err(err) => return ToolOutcome::error(err.to_string()),
                        };
                    if let crate::agent::AgentTarget::SeparateSession { session_id } = target
                        && matches!(status, crate::agent::Status::Finished { .. })
                    {
                        if let Err(err) = self.hosted.release_hosted_runtime(session_id).await {
                            return ToolOutcome::error(err);
                        }
                        report_progress(progress.as_ref(), format!("agent {agent_id} finished"))
                            .await;
                    }
                    ToolOutcome::text(render_family_observation(&observation))
                }
                HostAgentToolKind::Cancel => {
                    let agent_id = match parse_host_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.cancel(agent_id).await {
                        Ok(()) => {
                            ToolOutcome::text(format!("cancellation accepted for agent {agent_id}"))
                        }
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
                HostAgentToolKind::Resume => {
                    let agent_id = match parse_host_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.target(agent_id).await {
                        Ok(crate::agent::AgentTarget::SharedHistory { .. }) => ToolOutcome::error(
                            "lane agents reattach with their root session and do not require agent_resume",
                        ),
                        Ok(crate::agent::AgentTarget::SeparateSession { session_id }) => match self
                            .hosted
                            .resume(agent_id, session_id, cancel, progress.as_ref())
                            .await
                        {
                            Ok(()) => match self.family.observe(agent_id).await {
                                Ok(observation) => {
                                    ToolOutcome::text(render_family_observation(&observation))
                                }
                                Err(err) => ToolOutcome::error(err.to_string()),
                            },
                            Err(err) => ToolOutcome::error(err),
                        },
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
                HostAgentToolKind::Send => {
                    let agent_id = match parse_host_agent_handle(&arguments) {
                        Ok(agent_id) => agent_id,
                        Err(err) => return ToolOutcome::error(err),
                    };
                    let message = match host_string_arg(&arguments, "message") {
                        Ok(value) => value.to_owned(),
                        Err(err) => return ToolOutcome::error(err),
                    };
                    match self.family.target(agent_id).await {
                        Ok(crate::agent::AgentTarget::SharedHistory { .. }) => match self
                            .family
                            .send(self.family.root(), agent_id, message)
                            .await
                        {
                            Ok(operation_id) => ToolOutcome::text(format!(
                                "message accepted for agent {agent_id}: {operation_id}"
                            )),
                            Err(err) => ToolOutcome::error(err.to_string()),
                        },
                        Ok(crate::agent::AgentTarget::SeparateSession { .. }) => {
                            ToolOutcome::error(
                                "agent_send for fresh/fork agents awaits cross-session durable input routing",
                            )
                        }
                        Err(err) => ToolOutcome::error(err.to_string()),
                    }
                }
            }
        })
    }
}

#[derive(Clone, Copy)]
enum AgentTopology {
    Lane,
    Fresh,
    Fork,
}

struct AgentSpawnSpec {
    objective: String,
    topology: AgentTopology,
    context_seed: Option<String>,
    model_override: Option<String>,
}

fn parse_agent_spawn(arguments: &Value) -> Result<AgentSpawnSpec, String> {
    let objective = host_string_arg(arguments, "objective")?.to_owned();
    let topology = match arguments.get("topology").and_then(Value::as_str) {
        None | Some("lane") => AgentTopology::Lane,
        Some("fresh") => AgentTopology::Fresh,
        Some("fork") => AgentTopology::Fork,
        Some(other) => {
            return Err(format!(
                "malformed arguments: unsupported topology {other:?}"
            ));
        }
    };
    let context_seed = optional_host_string(arguments, "context")?;
    let model_override = optional_host_string(arguments, "model_override")?;
    Ok(AgentSpawnSpec {
        objective,
        topology,
        context_seed,
        model_override,
    })
}

fn optional_host_string(arguments: &Value, name: &str) -> Result<Option<String>, String> {
    match arguments.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .map(|value| Some(value.to_owned()))
            .ok_or_else(|| format!("malformed arguments: `{name}` must be a non-empty string")),
    }
}

fn host_string_arg<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, String> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("malformed arguments: `{name}` must be a non-empty string"))
}

fn host_agent_handle_schema(extra: Option<(&str, &str)>) -> Value {
    let mut properties = serde_json::Map::from_iter([(
        "handle".to_owned(),
        json!({"type": "string", "description": "agent-<uuid> returned by spawn_agent"}),
    )]);
    let mut required = vec![Value::String("handle".to_owned())];
    if let Some((name, kind)) = extra {
        properties.insert(name.to_owned(), json!({"type": kind}));
        required.push(Value::String(name.to_owned()));
    }
    json!({"type": "object", "properties": properties, "required": required})
}

fn parse_host_agent_handle(arguments: &Value) -> Result<crate::ids::AgentId, String> {
    let raw = host_string_arg(arguments, "handle")?;
    let uuid = raw.strip_prefix("agent-").unwrap_or(raw);
    crate::ids::AgentId::parse(uuid).ok_or_else(|| format!("malformed agent handle {raw:?}"))
}

fn host_status_operation_id(status: &crate::agent::Status) -> Option<OperationId> {
    match status {
        crate::agent::Status::Admitted => None,
        crate::agent::Status::Active { operation_id, .. }
        | crate::agent::Status::Suspended { operation_id }
        | crate::agent::Status::Finished { operation_id, .. } => Some(*operation_id),
    }
}

fn render_family_observation(observation: &crate::agent::Observation) -> String {
    let status = match &observation.status {
        crate::agent::Status::Admitted => "admitted".to_owned(),
        crate::agent::Status::Active {
            operation_id,
            state,
        } => {
            format!("active ({operation_id}, {state:?})")
        }
        crate::agent::Status::Suspended { operation_id } => {
            format!("suspended ({operation_id})")
        }
        crate::agent::Status::Finished {
            operation_id,
            outcome,
        } => {
            format!("finished ({operation_id}, {outcome:?})")
        }
    };
    match &observation.result {
        Some(result) => format!("agent {}: {status}\n\n{result}", observation.agent_id),
        None => format!("agent {}: {status}", observation.agent_id),
    }
}

struct ForkContext {
    rendered: Option<String>,
    source_entry_id: Option<crate::ids::EntryId>,
}

async fn fork_context(store: &SessionStore, parent_id: SessionId) -> Result<ForkContext, String> {
    let loaded = store
        .load(parent_id)
        .await
        .map_err(|err| format!("could not load parent context: {err}"))?;
    let main = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .ok_or_else(|| "parent session has no main lane".to_owned())?;
    let source_entry_id = main.state.leaf;
    let Some(mut cursor) = source_entry_id else {
        return Ok(ForkContext {
            rendered: None,
            source_entry_id: None,
        });
    };
    let index = loaded
        .entries
        .iter()
        .map(|record| (record.id, record))
        .collect::<HashMap<_, _>>();
    let mut branch = Vec::new();
    loop {
        let record = index
            .get(&cursor)
            .copied()
            .ok_or_else(|| format!("parent main branch references missing entry {cursor}"))?;
        branch.push(record);
        let Some(parent) = record.parent else {
            break;
        };
        cursor = parent;
    }
    branch.reverse();
    let first_seq = branch.first().map_or(1, |record| record.seq);
    let plan = project(branch.iter().map(|record| &record.entry), first_seq);
    Ok(ForkContext {
        rendered: Some(render_fork_context(&plan)),
        source_entry_id,
    })
}

fn compose_hosted_prompt(spec: &HostedAgentSpec, fork: Option<&str>) -> String {
    let mut prompt = spec.objective.clone();
    if let Some(fork) = fork {
        prompt.push_str("\n\n[Explicit fork of parent semantic context]\n");
        prompt.push_str(fork);
    }
    if let Some(seed) = &spec.context_seed {
        prompt.push_str("\n\n[Explicit agent context seed]\n");
        prompt.push_str(seed);
    }
    prompt
}

fn render_fork_context(plan: &ContextPlan) -> String {
    const MAX_BYTES: usize = 16 * 1024;
    let mut rendered = String::new();
    for message in &plan.messages {
        match message {
            ContextMessage::User { content } => {
                rendered.push_str("User:\n");
                rendered.push_str(content);
                rendered.push('\n');
            }
            ContextMessage::Assistant {
                content,
                tool_calls,
            } => {
                rendered.push_str("Assistant:\n");
                rendered.push_str(content);
                for call in tool_calls {
                    rendered.push_str("\n[tool call ");
                    rendered.push_str(&call.name);
                    rendered.push_str("] ");
                    rendered.push_str(&call.arguments.to_string());
                }
                rendered.push('\n');
            }
            ContextMessage::Tool { call_id, content } => {
                rendered.push_str("Tool result ");
                rendered.push_str(&call_id.to_string());
                rendered.push_str(":\n");
                rendered.push_str(content);
                rendered.push('\n');
            }
        }
    }
    truncate_context(&rendered, MAX_BYTES)
}

fn truncate_context(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let marker = "\n[… parent context truncated …]\n";
    let budget = max_bytes.saturating_sub(marker.len());
    let head_limit = budget / 2;
    let head_end = text
        .char_indices()
        .take_while(|(index, ch)| *index + ch.len_utf8() <= head_limit)
        .map(|(index, ch)| index + ch.len_utf8())
        .last()
        .unwrap_or(0);
    let tail_start_limit = text.len().saturating_sub(budget - head_end);
    let tail_start = text
        .char_indices()
        .find(|(index, _)| *index >= tail_start_limit)
        .map_or(text.len(), |(index, _)| index);
    format!("{}{}{}", &text[..head_end], marker, &text[tail_start..])
}

async fn report_progress(progress: Option<&ToolProgressSender>, output: String) {
    if let Some(progress) = progress {
        let _ = progress
            .send(ToolProgress {
                output: bounded_progress_output(output),
            })
            .await;
    }
}
