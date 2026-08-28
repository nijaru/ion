//! Bounded child delegation (DESIGN.md §20, Step 7).
//!
//! A child is the same primitive as a root session - a full
//! `SessionRuntime` with its own durable store record - never separate
//! runtime code. [`ChildManager`] owns live child incarnations while
//! [`ChildHandle`] and store-derived observations remain durable across
//! process boundaries. The model-facing control surface returns handles
//! and uses status/wait/cancel/resume operations; child transcripts remain
//! in their own sessions and are not injected into the parent automatically.
//!
//! Child control is a structural capability like `compact`: the gate does
//! not require a grant, because every effect a child can produce is
//! individually gated inside the child (§20.4). Nesting is disabled
//! structurally: child catalogs are read-only and never contain a
//! child-control tool.

use std::{
    collections::HashMap,
    fmt,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::context::{ContextMessage, ContextPlan, TrustedResource, project};
use crate::ids::{OperationId, SessionId};
use crate::provider::Provider;
use crate::runtime::{Runtime, RuntimeBudget};
use crate::session::{OperationOutcome, OperationState};
use crate::store::SessionStore;
use crate::tool::{
    Tool, ToolOutcome, ToolProgress, ToolProgressSender, ToolSpec, bounded_progress_output,
};

/// Conservative default bounds for children (§20.5): exact numbers are
/// host configuration; these exist so hosts that do not tune budgets
/// still cannot loop forever.
#[must_use]
pub fn child_budget_default() -> RuntimeBudget {
    RuntimeBudget {
        max_model_steps: Some(16),
        max_tool_calls: Some(64),
    }
}

/// How a child receives parent context. `Fresh` is the safe default; the
/// parent transcript is never copied unless the caller explicitly selects
/// `ForkContext`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildContextMode {
    Fresh,
    ForkContext,
}

/// One requested child in a delegation call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSpec {
    pub objective: String,
    /// Explicit context seed appended after the objective (§20.3):
    /// never an implicit copy of parent state.
    pub context_seed: Option<String>,
    /// Explicit parent-context projection mode (§20.3).
    pub context_mode: ChildContextMode,
    /// Optional host-resolved model for this child only.
    pub model_override: Option<String>,
}

/// Configuration and bounds for children spawned by one delegate tool.
pub struct DelegateConfig<P> {
    pub store: SessionStore,
    pub make_provider: Arc<dyn Fn() -> P + Send + Sync>,
    /// Optional resolver for explicit per-call model overrides. Unsupported
    /// overrides fail visibly instead of silently using the launch model.
    pub make_provider_for_model: Option<Arc<dyn Fn(String) -> P + Send + Sync>>,
    /// Maximum number of live child runtimes retained by this service.
    pub max_active_children: usize,
    /// Budget applied to every child.
    pub child_budget: RuntimeBudget,
    /// Explicitly inherited project resources; empty when the host did not
    /// grant project trust.
    pub trusted_resources: Vec<TrustedResource>,
    /// Host-owned workspace root used for the child catalog and durable
    /// session identity.
    pub cwd: PathBuf,
}

/// Stable durable identity for a child session (§20.7). The session id is
/// the authority; no task handle or process incarnation is embedded here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ChildHandle {
    pub session_id: SessionId,
    pub parent_session_id: SessionId,
}

impl fmt::Display for ChildHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.session_id.fmt(f)
    }
}

/// Runtime/store-derived child state. A terminal state is read from durable
/// session state, so it remains observable after the live task is gone.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChildStatus {
    Starting,
    Active {
        operation_id: OperationId,
        state: OperationState,
    },
    Suspended {
        operation_id: OperationId,
    },
    Finished {
        operation_id: OperationId,
        outcome: OperationOutcome,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildObservation {
    pub handle: ChildHandle,
    pub status: ChildStatus,
    pub objective: Option<String>,
    pub result: Option<String>,
}

impl ChildObservation {
    fn render(&self) -> String {
        let status = match &self.status {
            ChildStatus::Starting => "starting".to_owned(),
            ChildStatus::Active { state, .. } => format!("active ({state:?})"),
            ChildStatus::Suspended { .. } => "suspended".to_owned(),
            ChildStatus::Finished { outcome, .. } => format!("finished ({outcome:?})"),
        };
        let mut output = format!("child {}: {status}", self.handle);
        if let Some(result) = &self.result {
            output.push_str("\n\n");
            output.push_str(result);
        }
        output
    }
}

struct ManagedChild {
    session: crate::runtime::SessionHandle,
    runtime: Option<Runtime>,
    catalog: crate::tool::ToolCatalog,
    operation_id: OperationId,
    objective: String,
    cancel: CancellationToken,
    cancel_watch: JoinHandle<()>,
}

/// Process-owned manager for durable child runtimes. The store is the
/// authority for status; this registry only retains live runtime incarnations
/// so cancellation, waiting, and shutdown can be routed to them.
pub struct ChildManager<P> {
    config: Arc<DelegateConfig<P>>,
    parent_id: SessionId,
    children: Mutex<HashMap<SessionId, ManagedChild>>,
}

impl<P> ChildManager<P> {
    fn new(config: Arc<DelegateConfig<P>>, parent_id: SessionId) -> Arc<Self> {
        Arc::new(Self {
            config,
            parent_id,
            children: Mutex::new(HashMap::new()),
        })
    }

    fn live_session(&self, session_id: SessionId) -> Option<crate::runtime::SessionHandle> {
        self.children
            .lock()
            .expect("child manager poisoned")
            .get(&session_id)
            .map(|child| child.session.clone())
    }

    /// Remove one live incarnation from the registry, then drain every owned
    /// resource without holding the manager mutex. The durable session remains
    /// in SQLite and is still observable/resumable by handle.
    async fn release_live_child(&self, session_id: SessionId) -> Result<(), String> {
        let Some(mut child) = self
            .children
            .lock()
            .expect("child manager poisoned")
            .remove(&session_id)
        else {
            return Ok(());
        };
        child.cancel.cancel();
        let mut first_error = None;
        if let Err(err) = child.session.close().await {
            first_error.get_or_insert_with(|| format!("child close failed: {err}"));
        }
        if let Some(runtime) = child.runtime.take()
            && let Err(err) = runtime.join().await
        {
            first_error.get_or_insert_with(|| format!("child join failed: {err}"));
        }
        if let Err(err) = child.catalog.close().await {
            first_error.get_or_insert_with(|| format!("child catalog close failed: {err}"));
        }
        if let Err(err) = child.cancel_watch.await {
            first_error.get_or_insert_with(|| format!("child cancellation watcher failed: {err}"));
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    /// Reclaim completed live runtimes before applying the live-child bound.
    /// Status comes from the durable store, not from task liveness.
    async fn reap_finished_children(&self) -> Result<(), String> {
        let ids: Vec<SessionId> = self
            .children
            .lock()
            .expect("child manager poisoned")
            .keys()
            .copied()
            .collect();
        for session_id in ids {
            let loaded =
                self.config.store.load(session_id).await.map_err(|err| {
                    format!("child {session_id} unavailable while reaping: {err}")
                })?;
            let finished = loaded
                .operations
                .iter()
                .max_by_key(|operation| operation.accepted_seq)
                .is_some_and(|operation| {
                    matches!(operation.latest.1.state, OperationState::Finished(_))
                });
            if finished {
                self.release_live_child(session_id).await?;
            }
        }
        Ok(())
    }

    async fn spawn(
        self: &Arc<Self>,
        spec: ChildSpec,
        parent_cancel: CancellationToken,
        progress: Option<&ToolProgressSender>,
    ) -> Result<ChildHandle, String>
    where
        P: Provider,
    {
        if parent_cancel.is_cancelled() {
            return Err("cancelled".to_owned());
        }
        self.reap_finished_children().await?;
        {
            let children = self.children.lock().expect("child manager poisoned");
            if children.len() >= self.config.max_active_children.max(1) {
                return Err(format!(
                    "child limit reached (max {})",
                    self.config.max_active_children.max(1)
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
            ChildContextMode::Fresh => None,
            ChildContextMode::ForkContext => fork_context(&self.config.store, self.parent_id)
                .await
                .map_err(|err| format!("could not load parent context: {err}"))?,
        };
        let prompt = compose_child_prompt(&spec, fork_context.as_deref());
        let catalog = crate::tool::ToolCatalog::read_only(&self.config.cwd);
        let runtime = Runtime::start_child_with_resources(
            provider,
            catalog.clone(),
            self.config.store.clone(),
            Arc::new(crate::policy::DefaultPolicy),
            self.config.child_budget,
            self.parent_id,
            self.config.trusted_resources.clone(),
        );
        let session_id = runtime.session_id();
        let session = runtime.session();
        let operation_id = match session.submit_if_idle(prompt).await {
            Ok(operation_id) => operation_id,
            Err(err) => {
                let _ = session.close().await;
                let _ = runtime.join().await;
                let _ = catalog.close().await;
                return Err(format!("child submit failed: {err}"));
            }
        };
        let handle = ChildHandle {
            session_id,
            parent_session_id: self.parent_id,
        };
        let cancel = parent_cancel.child_token();
        let cancel_for_watch = cancel.clone();
        let session_for_watch = session.clone();
        let cancel_watch = tokio::spawn(async move {
            cancel_for_watch.cancelled().await;
            let _ = session_for_watch.cancel(operation_id).await;
        });
        let objective = spec.objective;
        self.children
            .lock()
            .expect("child manager poisoned")
            .insert(
                session_id,
                ManagedChild {
                    session,
                    runtime: Some(runtime),
                    catalog,
                    operation_id,
                    objective: objective.clone(),
                    cancel,
                    cancel_watch,
                },
            );
        report_progress(progress, format!("child {handle} accepted: {objective}")).await;
        Ok(handle)
    }

    async fn observe(&self, handle: ChildHandle) -> Result<ChildObservation, String> {
        let loaded = self
            .config
            .store
            .load(handle.session_id)
            .await
            .map_err(|err| format!("child {} unavailable: {err}", handle.session_id))?;
        if loaded.session.parent_session_id != Some(self.parent_id)
            || handle.parent_session_id != self.parent_id
        {
            return Err(format!(
                "child {} is not owned by this parent",
                handle.session_id
            ));
        }
        let managed = self
            .children
            .lock()
            .expect("child manager poisoned")
            .get(&handle.session_id)
            .map(|child| (child.objective.clone(), child.operation_id));
        let objective = managed.as_ref().map(|(objective, _)| objective.clone());
        let Some(operation) = loaded.operations.iter().max_by_key(|op| op.accepted_seq) else {
            return Ok(ChildObservation {
                handle,
                status: ChildStatus::Starting,
                objective,
                result: None,
            });
        };
        let operation_id = operation.id;
        let state = &operation.latest.1.state;
        let result = loaded
            .entries
            .iter()
            .rev()
            .find_map(|record| match &record.entry {
                crate::session::SessionEntry::AssistantMessage { text }
                    if matches!(state, OperationState::Finished(OperationOutcome::Completed)) =>
                {
                    Some(text.clone())
                }
                _ => None,
            });
        let status = match state {
            OperationState::Finished(outcome) => ChildStatus::Finished {
                operation_id,
                outcome: outcome.clone(),
            },
            OperationState::Suspended => ChildStatus::Suspended { operation_id },
            state => ChildStatus::Active {
                operation_id,
                state: state.clone(),
            },
        };
        Ok(ChildObservation {
            handle,
            status,
            objective,
            result,
        })
    }

    async fn wait(
        &self,
        handle: ChildHandle,
        cancel: CancellationToken,
        progress: Option<&ToolProgressSender>,
    ) -> Result<ChildObservation, String> {
        let initial = self.observe(handle).await?;
        if matches!(initial.status, ChildStatus::Finished { .. }) {
            self.release_live_child(handle.session_id).await?;
            return Ok(initial);
        }
        if matches!(initial.status, ChildStatus::Suspended { .. }) {
            return Ok(initial);
        }
        let Some(session) = self.live_session(handle.session_id) else {
            return Err(format!("child {} is not loaded", handle.session_id));
        };
        let (snapshot, mut events) = session
            .subscribe()
            .await
            .map_err(|err| format!("child {} subscribe failed: {err}", handle.session_id))?;
        if matches!(snapshot.operation, crate::runtime::OperationStatus::Idle) {
            let observation = self.observe(handle).await?;
            if matches!(observation.status, ChildStatus::Finished { .. }) {
                self.release_live_child(handle.session_id).await?;
            }
            return Ok(observation);
        }
        loop {
            tokio::select! {
                () = cancel.cancelled() => return Err("cancelled".to_owned()),
                event = events.recv() => {
                    let event = event.map_err(|err| format!("child {} event stream failed: {err}", handle.session_id))?;
                    if event.operation_id() != initial.operation_id() {
                        continue;
                    }
                    if event_is_terminal(&event) {
                        let observation = self.observe(handle).await?;
                        if matches!(observation.status, ChildStatus::Finished { .. }) {
                            self.release_live_child(handle.session_id).await?;
                        }
                        report_progress(progress, format!("child {} finished", handle.session_id)).await;
                        return Ok(observation);
                    }
                }
            }
        }
    }

    async fn resume(
        self: &Arc<Self>,
        handle: ChildHandle,
        parent_cancel: CancellationToken,
        progress: Option<&ToolProgressSender>,
    ) -> Result<ChildObservation, String>
    where
        P: Provider,
    {
        if self.live_session(handle.session_id).is_some() {
            return self.observe(handle).await;
        }
        let loaded = self
            .config
            .store
            .load(handle.session_id)
            .await
            .map_err(|err| format!("child {} unavailable: {err}", handle.session_id))?;
        if loaded.session.parent_session_id != Some(self.parent_id)
            || handle.parent_session_id != self.parent_id
        {
            return Err(format!(
                "child {} is not owned by this parent",
                handle.session_id
            ));
        }
        let Some(operation) = loaded.operations.iter().max_by_key(|op| op.accepted_seq) else {
            return Err(format!(
                "child {} has no operation to resume",
                handle.session_id
            ));
        };
        if matches!(operation.latest.1.state, OperationState::Finished(_)) {
            return self.observe(handle).await;
        }
        let model_ref = loaded.session.initial_model_ref.clone();
        let provider = match self.config.make_provider_for_model.as_ref() {
            Some(make_provider) => make_provider(model_ref.clone()),
            None => {
                let provider = (self.config.make_provider)();
                if !provider.supports_model(&model_ref) {
                    return Err(format!("model `{model_ref}` is unavailable for this child"));
                }
                provider
            }
        };
        let catalog = crate::tool::ToolCatalog::read_only(&loaded.session.cwd);
        let runtime = Runtime::open_child(
            provider,
            catalog.clone(),
            self.config.store.clone(),
            handle.session_id,
            crate::runtime::ChildRuntimeConfig {
                policy: Arc::new(crate::policy::DefaultPolicy),
                budget: self.config.child_budget,
                parent: self.parent_id,
                trusted_resources: self.config.trusted_resources.clone(),
            },
        )
        .await
        .map_err(|err| format!("child {} resume failed: {err}", handle.session_id))?;
        let session = runtime.session();
        let cancel = parent_cancel.child_token();
        let cancel_for_watch = cancel.clone();
        let session_for_watch = session.clone();
        let operation_id = operation.id;
        let cancel_watch = tokio::spawn(async move {
            cancel_for_watch.cancelled().await;
            let _ = session_for_watch.cancel(operation_id).await;
        });
        let objective = loaded
            .entries
            .iter()
            .find_map(|record| match &record.entry {
                crate::session::SessionEntry::UserMessage { text } => Some(text.clone()),
                _ => None,
            });
        self.children
            .lock()
            .expect("child manager poisoned")
            .insert(
                handle.session_id,
                ManagedChild {
                    session,
                    runtime: Some(runtime),
                    catalog,
                    operation_id,
                    objective: objective.unwrap_or_else(|| "resumed child".to_owned()),
                    cancel,
                    cancel_watch,
                },
            );
        report_progress(progress, format!("child {} resumed", handle.session_id)).await;
        self.observe(handle).await
    }

    async fn cancel(&self, handle: ChildHandle) -> Result<(), String> {
        let observation = self.observe(handle).await?;
        let Some(operation_id) = observation.operation_id() else {
            return Err(format!("child {} has not started", handle.session_id));
        };
        let Some(session) = self.live_session(handle.session_id) else {
            return Err(format!("child {} is not loaded", handle.session_id));
        };
        session
            .cancel(operation_id)
            .await
            .map_err(|err| format!("child {} cancellation failed: {err}", handle.session_id))
    }

    pub async fn close(&self) -> Result<(), String> {
        let ids: Vec<SessionId> = self
            .children
            .lock()
            .expect("child manager poisoned")
            .keys()
            .copied()
            .collect();
        let mut first_error = None;
        for session_id in ids {
            if let Err(err) = self.release_live_child(session_id).await {
                first_error.get_or_insert(err);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl ChildObservation {
    fn operation_id(&self) -> Option<OperationId> {
        match self.status {
            ChildStatus::Active { operation_id, .. }
            | ChildStatus::Suspended { operation_id }
            | ChildStatus::Finished { operation_id, .. } => Some(operation_id),
            ChildStatus::Starting => None,
        }
    }
}

fn event_is_terminal(event: &crate::runtime::RuntimeEvent) -> bool {
    matches!(
        event,
        crate::runtime::RuntimeEvent::OperationFinished { .. }
            | crate::runtime::RuntimeEvent::OperationFailed { .. }
            | crate::runtime::RuntimeEvent::OperationCancelled { .. }
            | crate::runtime::RuntimeEvent::OperationIndeterminate { .. }
            | crate::runtime::RuntimeEvent::OperationApprovalRequired { .. }
    )
}

/// One model-facing child control tool. Keeping the control surface in one
/// service makes the shared manager and its lifecycle explicit.
struct ChildTool<P> {
    manager: Arc<ChildManager<P>>,
    kind: ChildToolKind,
}

#[derive(Clone, Copy)]
enum ChildToolKind {
    Spawn,
    Status,
    Wait,
    Cancel,
    Resume,
}

/// Compose the durable child control surface and its process-owned manager.
/// The host must close the returned manager before dropping the catalog.
pub fn child_tools<P: Provider + 'static>(
    config: DelegateConfig<P>,
    parent_id: SessionId,
) -> (Arc<ChildManager<P>>, Vec<Arc<dyn Tool>>) {
    let manager = ChildManager::new(Arc::new(config), parent_id);
    let tools = [
        ChildToolKind::Spawn,
        ChildToolKind::Status,
        ChildToolKind::Wait,
        ChildToolKind::Cancel,
        ChildToolKind::Resume,
    ]
    .into_iter()
    .map(|kind| {
        Arc::new(ChildTool {
            manager: Arc::clone(&manager),
            kind,
        }) as Arc<dyn Tool>
    })
    .collect();
    (manager, tools)
}

impl<P: Provider + 'static> Tool for ChildTool<P> {
    fn spec(&self) -> ToolSpec {
        match self.kind {
            ChildToolKind::Spawn => ToolSpec {
                name: "spawn_child".to_owned(),
                description: "Start bounded research children and return durable child handles. Children use read-only tools; inspect them with child_status or child_wait.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "children": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "objective": {"type": "string"},
                                    "context": {"type": "string"},
                                    "context_mode": {"type": "string", "enum": ["fresh", "fork_context"]},
                                    "model_override": {"type": "string"}
                                },
                                "required": ["objective"]
                            },
                            "minItems": 1
                        }
                    },
                    "required": ["children"]
                }),
            },
            ChildToolKind::Status
            | ChildToolKind::Wait
            | ChildToolKind::Cancel
            | ChildToolKind::Resume => {
                let (name, description) = match self.kind {
                    ChildToolKind::Status => ("child_status", "Inspect a durable child handle without waiting."),
                    ChildToolKind::Wait => ("child_wait", "Wait for a durable child handle to finish and return its result."),
                    ChildToolKind::Cancel => ("child_cancel", "Cancel a running durable child handle."),
                    ChildToolKind::Resume => ("child_resume", "Load a non-terminal durable child handle after process loss."),
                    ChildToolKind::Spawn => unreachable!(),
                };
                ToolSpec {
                    name: name.to_owned(),
                    description: description.to_owned(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {"handle": {"type": "string", "description": "session-<uuid> returned by spawn_child"}},
                        "required": ["handle"]
                    }),
                }
            }
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
                ChildToolKind::Spawn => {
                    let specs = match parse_children(&arguments) {
                        Ok(specs) => specs,
                        Err(message) => return ToolOutcome::error(message),
                    };
                    let mut output = String::new();
                    for spec in specs {
                        match self
                            .manager
                            .spawn(spec, cancel.clone(), progress.as_ref())
                            .await
                        {
                            Ok(handle) => {
                                if !output.is_empty() {
                                    output.push('\n');
                                }
                                output.push_str(&format!("child handle: {handle}"));
                            }
                            Err(err) => return ToolOutcome::error(err),
                        }
                    }
                    ToolOutcome::text(output)
                }
                ChildToolKind::Status
                | ChildToolKind::Wait
                | ChildToolKind::Cancel
                | ChildToolKind::Resume => {
                    let session_id = match parse_handle(&arguments) {
                        Ok(session_id) => session_id,
                        Err(message) => return ToolOutcome::error(message),
                    };
                    let handle = ChildHandle {
                        session_id,
                        parent_session_id: self.manager.parent_id,
                    };
                    match self.kind {
                        ChildToolKind::Status => match self.manager.observe(handle).await {
                            Ok(observation) => ToolOutcome::text(observation.render()),
                            Err(err) => ToolOutcome::error(err),
                        },
                        ChildToolKind::Wait => {
                            match self.manager.wait(handle, cancel, progress.as_ref()).await {
                                Ok(observation) => ToolOutcome::text(observation.render()),
                                Err(err) => ToolOutcome::error(err),
                            }
                        }
                        ChildToolKind::Cancel => match self.manager.cancel(handle).await {
                            Ok(()) => ToolOutcome::text(format!(
                                "cancellation accepted for child {handle}"
                            )),
                            Err(err) => ToolOutcome::error(err),
                        },
                        ChildToolKind::Resume => match self
                            .manager
                            .resume(handle, cancel.clone(), progress.as_ref())
                            .await
                        {
                            Ok(observation) => ToolOutcome::text(observation.render()),
                            Err(err) => ToolOutcome::error(err),
                        },
                        ChildToolKind::Spawn => unreachable!(),
                    }
                }
            }
        })
    }
}

fn parse_handle(arguments: &Value) -> Result<SessionId, String> {
    let raw = arguments
        .get("handle")
        .and_then(Value::as_str)
        .ok_or_else(|| "malformed arguments: `handle` must be a session id".to_owned())?;
    let uuid = raw.strip_prefix("session-").unwrap_or(raw);
    SessionId::parse(uuid).ok_or_else(|| format!("malformed child handle {raw:?}"))
}

/// Legacy synchronous delegation fixture retained only by unit tests while
/// existing transition coverage is migrated to durable child handles.
#[cfg(test)]
pub struct DelegateTool<P> {
    config: Arc<DelegateConfig<P>>,
    parent_id: SessionId,
}

#[cfg(test)]
impl<P> DelegateTool<P> {
    #[must_use]
    pub fn new(config: DelegateConfig<P>, parent_id: SessionId) -> Self {
        Self {
            config: Arc::new(config),
            parent_id,
        }
    }
}

#[cfg(test)]
impl<P: Provider + 'static> Tool for DelegateTool<P> {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delegate".to_owned(),
            description: "Run bounded research children with read-only tools. \
Each child gets an explicit objective and cannot widen capabilities; \
their results return as text. Use for parallel investigation."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "children": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "objective": { "type": "string" },
                                "context": {
                                    "type": "string",
                                    "description": "optional context seed"
                                },
                                "context_mode": {
                                    "type": "string",
                                    "enum": ["fresh", "fork_context"],
                                    "description": "fresh by default; explicitly fork durable parent context"
                                },
                                "model_override": {
                                    "type": "string",
                                    "description": "optional host-resolved model for this child"
                                }
                            },
                            "required": ["objective"]
                        },
                        "minItems": 1,
                        "description": "children to run concurrently"
                    }
                },
                "required": ["children"]
            }),
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
            let children = match parse_children(&arguments) {
                Ok(children) => children,
                Err(message) => return ToolOutcome::error(message),
            };
            let semaphore = Arc::new(tokio::sync::Semaphore::new(self.config.max_active_children));
            let mut handles = Vec::with_capacity(children.len());
            for spec in children {
                let semaphore = Arc::clone(&semaphore);
                let config = Arc::clone(&self.config);
                let parent_id = self.parent_id;
                let cancel = cancel.child_token();
                let progress = progress.clone();
                handles.push(tokio::spawn(async move {
                    let _permit = semaphore.acquire().await;
                    run_child(config, parent_id, spec, cancel, progress).await
                }));
            }
            // Parent cancellation cancels descendants (§20.6): the
            // child token above fires, each child's operation cancels,
            // and the results report it - the parent turn continues.
            let mut output = String::new();
            for handle in handles {
                let result = handle
                    .await
                    .unwrap_or_else(|err| format!("child task failed: {err}"));
                if !output.is_empty() {
                    output.push_str("\n\n");
                }
                output.push_str(&result);
            }
            if cancel.is_cancelled() {
                return ToolOutcome::error("cancelled");
            }
            ToolOutcome::text(output)
        })
    }
}

fn parse_children(arguments: &Value) -> Result<Vec<ChildSpec>, String> {
    let entries = arguments
        .get("children")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "malformed arguments: `children` must be a non-empty array of objects".to_owned()
        })?;
    if entries.is_empty() {
        return Err("malformed arguments: `children` cannot be empty".to_owned());
    }
    let mut specs = Vec::with_capacity(entries.len());
    for entry in entries {
        let objective = entry
            .get("objective")
            .and_then(Value::as_str)
            .ok_or_else(|| "malformed child: `objective` must be a string".to_owned())?
            .trim();
        if objective.is_empty() {
            return Err("malformed child: `objective` cannot be empty".to_owned());
        }
        let context_mode = match entry.get("context_mode").and_then(Value::as_str) {
            None | Some("fresh") => ChildContextMode::Fresh,
            Some("fork_context") => ChildContextMode::ForkContext,
            Some(other) => {
                return Err(format!(
                    "malformed child: unsupported `context_mode` {other:?}"
                ));
            }
        };
        let model_override = match entry.get("model_override") {
            None => None,
            Some(value) => {
                let model = value
                    .as_str()
                    .ok_or_else(|| "malformed child: `model_override` must be a string".to_owned())?
                    .trim();
                if model.is_empty() {
                    return Err("malformed child: `model_override` cannot be empty".to_owned());
                }
                Some(model.to_owned())
            }
        };
        specs.push(ChildSpec {
            objective: objective.to_owned(),
            context_seed: entry
                .get("context")
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            context_mode,
            model_override,
        });
    }
    Ok(specs)
}

/// Run one child to its terminal outcome and render the compact
/// result: final assistant text plus the child session reference.
#[cfg(test)]
async fn run_child<P>(
    config: Arc<DelegateConfig<P>>,
    parent_id: SessionId,
    spec: ChildSpec,
    cancel: CancellationToken,
    progress: Option<ToolProgressSender>,
) -> String
where
    P: Provider,
{
    let catalog = crate::tool::ToolCatalog::read_only(&config.cwd);
    let provider = match spec.model_override.as_deref() {
        Some(model_ref) => {
            let Some(make_provider) = config.make_provider_for_model.as_ref() else {
                return format!(
                    "child failed: model override `{model_ref}` is unavailable [child parent: {parent_id}]"
                );
            };
            make_provider(model_ref.to_owned())
        }
        None => (config.make_provider)(),
    };
    let fork_context_result = match spec.context_mode {
        ChildContextMode::Fresh => Ok(None),
        ChildContextMode::ForkContext => fork_context(&config.store, parent_id).await,
    };
    let fork_context = match fork_context_result {
        Ok(context) => context,
        Err(err) => return format!("child failed: {err} [child parent: {parent_id}]"),
    };
    let prompt = compose_child_prompt(&spec, fork_context.as_deref());
    let runtime = Runtime::start_child_with_resources(
        provider,
        catalog.clone(),
        config.store.clone(),
        Arc::new(crate::policy::DefaultPolicy),
        config.child_budget,
        parent_id,
        config.trusted_resources.clone(),
    );
    let child_id = runtime.session_id();
    let session = runtime.session();
    report_progress(
        progress.as_ref(),
        format!("child {child_id} started: {}", spec.objective),
    )
    .await;

    // Subscribe before submit: live events predate subscribers.
    let (_snapshot, mut events) = match session.subscribe().await {
        Ok(subscription) => subscription,
        Err(_) => {
            let _ = session.close().await;
            let _ = runtime.join().await;
            let catalog_error = catalog.close().await.err();
            return match catalog_error {
                Some(err) => format!(
                    "child failed: could not subscribe; catalog close error: {err} ({child_id})"
                ),
                None => format!("child failed: could not subscribe ({child_id})"),
            };
        }
    };
    let operation_id = match session.submit_if_idle(prompt).await {
        Ok(operation_id) => operation_id,
        Err(_) => {
            let _ = session.close().await;
            let _ = runtime.join().await;
            let catalog_error = catalog.close().await.err();
            return match catalog_error {
                Some(err) => format!(
                    "child failed: submit rejected; catalog close error: {err} ({child_id})"
                ),
                None => format!("child failed: submit rejected ({child_id})"),
            };
        }
    };

    let terminal = tokio::select! {
        outcome = pump_child(&mut events, operation_id, child_id, progress.as_ref()) => outcome,
        () = cancel.cancelled() => {
            // §20.6: cancelling the parent cancels descendants; the
            // child settles durably as cancelled on its own.
            let _ = session.cancel(operation_id).await;
            pump_child(&mut events, operation_id, child_id, progress.as_ref()).await
        }
    };

    let close_result = session.close().await;
    let join_result = runtime.join().await;
    if let Err(err) = close_result {
        return format!("child failed: close error: {err} [child session: {child_id}]");
    }
    if let Err(err) = join_result {
        return format!("child failed: runtime join error: {err} [child session: {child_id}]");
    }
    if let Err(err) = catalog.close().await {
        return format!("child failed: catalog close error: {err} [child session: {child_id}]");
    }
    match terminal {
        ChildTerminal::Completed(text) => {
            format!("{text}\n\n[child session: {child_id}]")
        }
        ChildTerminal::Failed(message) => {
            format!("child failed: {message} [child session: {child_id}]")
        }
        ChildTerminal::Cancelled => {
            format!("child cancelled [child session: {child_id}]")
        }
    }
}

async fn fork_context(
    store: &SessionStore,
    parent_id: SessionId,
) -> Result<Option<String>, String> {
    let loaded = store
        .load(parent_id)
        .await
        .map_err(|err| format!("could not load parent context: {err}"))?;
    if loaded.entries.is_empty() {
        return Ok(None);
    }
    let first_seq = loaded.entries.first().map_or(1, |record| record.seq);
    let plan = project(loaded.entries.iter().map(|record| &record.entry), first_seq);
    Ok(Some(render_fork_context(&plan)))
}

fn compose_child_prompt(spec: &ChildSpec, fork: Option<&str>) -> String {
    let mut prompt = spec.objective.clone();
    if let Some(fork) = fork {
        prompt.push_str("\n\n[Explicit fork of parent semantic context]\n");
        prompt.push_str(fork);
    }
    if let Some(seed) = &spec.context_seed {
        prompt.push_str("\n\n[Explicit child context seed]\n");
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

#[cfg(test)]
enum ChildTerminal {
    Completed(String),
    Failed(String),
    Cancelled,
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

/// Drain child events until the operation terminates, keeping the last
/// assistant draft as the compact result.
#[cfg(test)]
async fn pump_child(
    events: &mut crate::runtime::EventSubscription,
    operation_id: crate::ids::OperationId,
    child_id: SessionId,
    progress: Option<&ToolProgressSender>,
) -> ChildTerminal {
    let mut draft = String::new();
    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(crate::RuntimeError::SubscriptionLagged) => {
                // The compact result must not present silently
                // incomplete deltas as the child's answer (§21.4).
                report_progress(progress, format!("child {child_id} event stream lagged")).await;
                return ChildTerminal::Failed("child event stream lagged".to_owned());
            }
            Err(_) => {
                report_progress(progress, format!("child {child_id} event stream closed")).await;
                return ChildTerminal::Failed("event stream closed".to_owned());
            }
        };
        if event.operation_id() != Some(operation_id) {
            continue;
        }
        match event {
            crate::RuntimeEvent::AssistantTextDelta { text, .. } => {
                draft.push_str(&text);
            }
            // Thinking and tool previews are parent-display-only; a
            // child's terminal draft is its final assistant text.
            crate::RuntimeEvent::ThinkingDelta { .. }
            | crate::RuntimeEvent::ToolProgress { .. } => {}
            crate::RuntimeEvent::OperationFinished { .. } => {
                report_progress(progress, format!("child {child_id} finished")).await;
                let result = if draft.is_empty() {
                    "(no output)".to_owned()
                } else {
                    draft
                };
                return ChildTerminal::Completed(result);
            }
            crate::RuntimeEvent::OperationCancelled { .. } => {
                report_progress(progress, format!("child {child_id} cancelled")).await;
                return ChildTerminal::Cancelled;
            }
            crate::RuntimeEvent::OperationFailed { message, .. } => {
                report_progress(progress, format!("child {child_id} failed: {message}")).await;
                return ChildTerminal::Failed(message);
            }
            crate::RuntimeEvent::OperationIndeterminate { message, .. } => {
                report_progress(
                    progress,
                    format!("child {child_id} indeterminate: {message}"),
                )
                .await;
                return ChildTerminal::Failed(format!("indeterminate operation: {message}"));
            }
            crate::RuntimeEvent::OperationApprovalRequired { tool, .. } => {
                report_progress(
                    progress,
                    format!("child {child_id} needs approval for `{tool}`"),
                )
                .await;
                return ChildTerminal::Failed(format!(
                    "approval required for `{tool}` (read-only child)"
                ));
            }
            // Children are non-interactive and read-only: a parked
            // approval cannot occur, so surface it as a failure if it
            // ever does (defense, not a normal path).
            crate::RuntimeEvent::ApprovalPending { tool, .. } => {
                report_progress(
                    progress,
                    format!("child {child_id} needs approval for `{tool}`"),
                )
                .await;
                return ChildTerminal::Failed(format!(
                    "approval pending for `{tool}` (read-only child)"
                ));
            }
            crate::RuntimeEvent::ToolStarted { tool, target, .. } => {
                let target = target.map_or_else(String::new, |target| format!(" → {target}"));
                report_progress(progress, format!("child {child_id} running {tool}{target}")).await;
            }
            crate::RuntimeEvent::ToolSettled { .. }
            | crate::RuntimeEvent::UsageUpdate { .. }
            | crate::RuntimeEvent::OperationStarted { .. }
            | crate::RuntimeEvent::SessionClosed { .. } => {}
        }
    }
}
