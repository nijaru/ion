//! Retained blocking preflight and prepare-before-publish binding replacement.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use ion_ai::{ModelRef, Reasoning};
use ion_core::{Agent, ContextPosition, ForkPoint, Session};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::{
    Application, ApplicationRuntime, CommandCatalog, QuestionCustody, Resources, SessionCatalog,
    SessionControl, preparation::Preparation,
};

/// Configuration, not execution. Consume this to create the sole control owner.
pub struct SessionSetup {
    pub(crate) initial_completion_delivery: crate::CompletionDelivery,
    application: Arc<dyn Application>,
    pub(crate) catalog: SessionCatalog,
    pub(crate) session: Arc<Session>,
    pub(crate) runtime: ApplicationRuntime,
    pub(crate) resources: Resources,
    pub(crate) commands: CommandCatalog,
    pub(crate) questions: Option<Arc<QuestionCustody>>,
    retirements: crate::SessionRetirements,
}

struct PreparedBinding {
    resources: Resources,
    commands: CommandCatalog,
    runtime: ApplicationRuntime,
}

/// Owned request; starting it does not borrow the Controller until completion.
#[derive(Debug)]
pub enum BindingChange {
    ReloadResources,
    SelectModel(ModelRef),
    SelectReasoning(Reasoning),
    SetName(Option<String>),
    NewSession,
    CloneSession,
    ForkSession(ForkPoint),
    NavigateContext {
        expected: ContextPosition,
        head_entry: u64,
    },
    SwitchSession(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BindingOperationId(pub uuid::Uuid);

#[derive(Debug, Clone, Serialize)]
pub enum BindingResult {
    ResourcesReloaded,
    ModelSelected,
    ReasoningSelected,
    SessionNamed { name: Option<String> },
    SessionCreated,
    SessionCloned { session_id: String },
    SessionForked { session_id: String },
    ContextNavigated(ContextPosition),
    SessionSwitched,
}

/// The sole bounded result slot, consumed by SDK join or ControlUpdate delivery.
#[derive(Debug)]
pub struct BindingFinished {
    pub operation: BindingOperationId,
    pub result: Result<BindingResult>,
    pub stop_requested: bool,
}

pub(crate) enum BindingWork {
    Running {
        operation: BindingOperationId,
        work: Preparation<BindingDelta>,
        launch: Option<BindingLaunch>,
    },
    Finished(BindingFinished),
}

impl BindingWork {
    pub(crate) fn operation(&self) -> BindingOperationId {
        match self {
            Self::Running { operation, .. } => *operation,
            Self::Finished(finished) => finished.operation,
        }
    }
    pub(crate) fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }
    pub(crate) fn request_stop(&self) {
        if let Self::Running { work, .. } = self {
            work.request_stop();
        }
    }
    pub(crate) fn is_stopped(&self) -> bool {
        match self {
            Self::Running { work, .. } => work.is_stopped(),
            Self::Finished(finished) => finished.stop_requested,
        }
    }
    pub(crate) async fn join(&mut self) -> Result<BindingDelta> {
        match self {
            Self::Running { work, launch, .. } => {
                if let Some(pending) = launch.as_ref() {
                    for retirement in &pending.retirements {
                        // Readiness is physical completion, not success of an
                        // unrelated historical read. The scope retains errors
                        // for final cleanup rather than poisoning every switch.
                        let _ = retirement.join().await;
                    }
                }
                if let Some(BindingLaunch { source, change, .. }) = launch.take() {
                    work.launch_blocking(move |stop| source.run(change, stop));
                }
                work.join().await
            }
            Self::Finished(_) => unreachable!("only running bindings are driven"),
        }
    }
}

// No Session registry owns this outcome: it may itself retain the same Session.
// Registering it as SessionIo would create Session -> outcome -> Session custody.
pub(crate) struct BindingDelta {
    prepared: PreparedChange,
    result: BindingResult,
}

enum PreparedChange {
    Resources(Resources, CommandCatalog),
    Runtime(ApplicationRuntime, CommandCatalog),
    Preference,
    Replacement(Arc<Session>, PreparedBinding),
}

pub(crate) struct BindingLaunch {
    source: BindingSource,
    change: BindingChange,
    retirements: Vec<ion_core::SessionRetirement>,
}

struct BindingSource {
    application: Arc<dyn Application>,
    catalog: SessionCatalog,
    session: Arc<Session>,
    runtime: ApplicationRuntime,
    questions: Option<Arc<QuestionCustody>>,
    background: Option<Arc<crate::BackgroundOwner>>,
    retirements: crate::SessionRetirements,
}

/// Retained, owned setup for async application callers. Its actual blocking
/// worker and outcome live outside the Session registry: a setup can itself
/// retain that Session without creating a registry/outcome ownership cycle.
#[must_use = "retain setup preparation and join its outcome"]
pub struct SetupPreparation {
    work: Preparation<SessionSetup>,
}
impl SetupPreparation {
    pub(crate) fn start(
        stop: CancellationToken,
        work: impl FnOnce(CancellationToken) -> Result<SessionSetup> + Send + 'static,
    ) -> Self {
        Self {
            work: Preparation::start_blocking(stop, move |stop| {
                check_stop(&stop)?;
                work(stop)
            }),
        }
    }

    /// Cooperative intent, not proof that an in-flight native write stopped.
    pub fn request_stop(&self) {
        self.work.request_stop();
    }

    /// Abandoning this borrowed wait retains the actual worker and its outcome.
    pub async fn join(&mut self) -> Result<SessionSetup> {
        self.work.join().await
    }
}

impl SessionSetup {
    /// Initial receiving policy; live changes belong to SessionControl.
    pub fn with_completion_delivery(mut self, delivery: crate::CompletionDelivery) -> Self {
        self.initial_completion_delivery = delivery;
        self
    }

    /// Explicit blocking construction for offline factories and blocking workers.
    /// An omitted model restores the Session preference, then the application's
    /// default for a fresh Session. Omitted reasoning restores its saved value.
    pub fn new(
        session: Arc<Session>,
        catalog: SessionCatalog,
        application: Arc<dyn Application>,
        model: Option<&ModelRef>,
        reasoning: Option<Reasoning>,
        questions: Option<QuestionCustody>,
    ) -> Result<Self> {
        let questions = questions.map(Arc::new);
        let model = model.cloned().or(session.model_preference()?);
        let resources = application.resources(session.cwd())?;
        let runtime = application.runtime(session.cwd(), model.as_ref(), questions.clone())?;
        validate_model(&runtime, model.as_ref())?;
        Self::from_prepared(
            session,
            catalog,
            application,
            resources,
            runtime,
            reasoning,
            questions,
        )
    }

    /// Explicit blocking completion of a preflighted application recipe.
    pub(crate) fn from_prepared(
        session: Arc<Session>,
        catalog: SessionCatalog,
        application: Arc<dyn Application>,
        resources: Resources,
        runtime: ApplicationRuntime,
        reasoning: Option<Reasoning>,
        questions: Option<Arc<QuestionCustody>>,
    ) -> Result<Self> {
        ensure!(
            catalog.cwd() == session.cwd(),
            "catalog and Session working directories differ"
        );
        let commands = application.commands(session.cwd())?;
        if let Some(reasoning) = reasoning {
            runtime.agent.select_reasoning(&session, reasoning)?;
        } else {
            runtime.agent.validate_reasoning(session.reasoning()?)?;
        }
        let retirements = application.session_retirements().unwrap_or_default();
        retirements.register(&session)?;
        Ok(Self {
            retirements,
            initial_completion_delivery: crate::CompletionDelivery::Inbox,
            application,
            catalog,
            session,
            runtime,
            resources,
            commands,
            questions,
        })
    }

    pub(crate) fn mcp_tools(&self) -> Option<Arc<crate::McpTools>> {
        self.application.mcp_tools()
    }

    /// Retain this outside control, release all Session/read capabilities, then
    /// join it after control shutdown to finish actual native Store destruction.
    pub fn retirements(&self) -> crate::SessionRetirements {
        self.retirements.clone()
    }

    pub fn owner(&self) -> Option<Arc<crate::ApplicationOwner>> {
        self.application.owner()
    }
    pub fn background(&self) -> Option<Arc<crate::BackgroundOwner>> {
        self.owner().map(|owner| owner.background().clone())
    }
    pub fn catalog(&self) -> &SessionCatalog {
        &self.catalog
    }
    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }
    pub fn agent(&self) -> &Arc<Agent> {
        &self.runtime.agent
    }
    pub fn resources(&self) -> &Resources {
        &self.resources
    }
    pub fn commands(&self) -> &CommandCatalog {
        &self.commands
    }
    pub fn instructions(&self) -> &str {
        self.resources.instructions()
    }
    pub fn session_id(&self) -> String {
        self.session.id().to_string()
    }
    pub fn into_control<M: Serialize + Send + 'static>(self) -> SessionControl<M> {
        SessionControl::from_setup(self)
    }
}

pub(crate) fn validate_model(
    runtime: &ApplicationRuntime,
    requested: Option<&ModelRef>,
) -> Result<()> {
    ensure!(
        requested.is_none_or(|model| model == runtime.agent.model()),
        "application returned a different model than requested"
    );
    Ok(())
}

fn check_stop(stop: &CancellationToken) -> Result<()> {
    ensure!(!stop.is_cancelled(), "binding preparation stopped");
    Ok(())
}

impl BindingSource {
    fn prepare(
        &self,
        cwd: Option<&Path>,
        model: Option<&ModelRef>,
        reasoning: Reasoning,
    ) -> Result<PreparedBinding> {
        let resources = self.application.resources(cwd)?;
        let commands = self.application.commands(cwd)?;
        let runtime = self
            .application
            .runtime(cwd, model, self.questions.clone())?;
        validate_model(&runtime, model)?;
        runtime.agent.validate_reasoning(reasoning)?;
        Ok(PreparedBinding {
            resources,
            commands,
            runtime,
        })
    }

    fn reload(&self, stop: &CancellationToken, result: BindingResult) -> Result<BindingDelta> {
        let resources = self.application.resources(self.session.cwd())?;
        let commands = self.application.commands(self.session.cwd())?;
        check_stop(stop)?;
        Ok(BindingDelta {
            prepared: PreparedChange::Resources(resources, commands),
            result,
        })
    }

    // Every native lease, Store access and user callback lives in this actual
    // blocking worker. Never move an IdleSession guard into an async future.
    fn run(self, change: BindingChange, stop: CancellationToken) -> Result<BindingDelta> {
        check_stop(&stop)?;
        let delta: Result<_> = match change {
            BindingChange::ReloadResources => self.reload(&stop, BindingResult::ResourcesReloaded),
            BindingChange::SelectModel(model) => {
                self.session.ensure_idle()?;
                let commands = self.application.commands(self.session.cwd())?;
                let runtime = self.application.runtime(
                    self.session.cwd(),
                    Some(&model),
                    self.questions.clone(),
                )?;
                validate_model(&runtime, Some(&model))?;
                check_stop(&stop)?;
                // Agent validates saved reasoning and commits under Core's gate.
                runtime.agent.select_model(&self.session)?;
                Ok(BindingDelta {
                    prepared: PreparedChange::Runtime(runtime, commands),
                    result: BindingResult::ModelSelected,
                })
            }
            BindingChange::SelectReasoning(reasoning) => {
                check_stop(&stop)?;
                self.runtime
                    .agent
                    .select_reasoning(&self.session, reasoning)?;
                Ok(BindingDelta {
                    prepared: PreparedChange::Preference,
                    result: BindingResult::ReasoningSelected,
                })
            }
            BindingChange::SetName(name) => {
                let name = self.session.set_name(name.as_deref())?;
                Ok(BindingDelta {
                    prepared: PreparedChange::Preference,
                    result: BindingResult::SessionNamed { name },
                })
            }
            BindingChange::NewSession => {
                let _idle = self.session.reserve_idle()?;
                // New Sessions reset reasoning, unlike conversation copies.
                let prepared =
                    self.prepare(self.session.cwd(), None, Reasoning::ProviderDefault)?;
                check_stop(&stop)?;
                let path = self.catalog.new_path()?;
                let session = Arc::new(Session::create(path, self.session.cwd())?);
                prepared.runtime.agent.select_model(&session)?;
                Ok(BindingDelta {
                    prepared: PreparedChange::Replacement(session, prepared),
                    result: BindingResult::SessionCreated,
                })
            }
            BindingChange::CloneSession => {
                let _idle = self.session.reserve_idle()?;
                // Keep the resolved runtime, including an unsaved route. A store
                // lookup could otherwise redirect or invalidate a valid copy.
                let prepared = PreparedBinding {
                    resources: self.application.resources(self.session.cwd())?,
                    commands: self.application.commands(self.session.cwd())?,
                    runtime: self.runtime.clone(),
                };
                prepared
                    .runtime
                    .agent
                    .validate_reasoning(self.session.reasoning()?)?;
                check_stop(&stop)?;
                let path = self.catalog.new_path()?;
                let session = Arc::new(self.session.clone_to(path)?);
                let session_id = session.id().to_string();
                Ok(BindingDelta {
                    prepared: PreparedChange::Replacement(session, prepared),
                    result: BindingResult::SessionCloned { session_id },
                })
            }
            BindingChange::ForkSession(point) => {
                let _idle = self.session.reserve_idle()?;
                let (model, reasoning) = self.session.fork_preferences(point)?;
                let prepared = self.prepare(self.session.cwd(), Some(&model), reasoning)?;
                check_stop(&stop)?;
                let path = self.catalog.new_path()?;
                let session = Arc::new(self.session.fork_to(path, point)?);
                let session_id = session.id().to_string();
                Ok(BindingDelta {
                    prepared: PreparedChange::Replacement(session, prepared),
                    result: BindingResult::SessionForked { session_id },
                })
            }
            BindingChange::NavigateContext {
                expected,
                head_entry,
            } => {
                let mut navigation = self.session.prepare_navigation(expected, head_entry)?;
                let prepared = self.prepare(
                    self.session.cwd(),
                    navigation.model(),
                    navigation.reasoning(),
                )?;
                check_stop(&stop)?;
                let position = navigation.commit()?;
                Ok(BindingDelta {
                    prepared: PreparedChange::Replacement(self.session.clone(), prepared),
                    result: BindingResult::ContextNavigated(position),
                })
            }
            BindingChange::SwitchSession(path) => {
                let _idle = self.session.reserve_idle()?;
                let path = fs::canonicalize(self.catalog.resolve_explicit(path)?)?;
                if self.session.path() == path {
                    return self.reload(&stop, BindingResult::SessionSwitched);
                }
                let session = match self
                    .background
                    .as_ref()
                    .and_then(|owner| owner.parent(&path))
                {
                    Some(parent) => parent,
                    None => Arc::new(Session::open(path)?),
                };
                let target_idle = session.reserve_idle()?;
                ensure!(
                    session.cwd() == self.session.cwd(),
                    "session belongs to another working directory"
                );
                let model = session.model_preference()?;
                let prepared = self.prepare(session.cwd(), model.as_ref(), session.reasoning()?)?;
                check_stop(&stop)?;
                drop(target_idle);
                Ok(BindingDelta {
                    prepared: PreparedChange::Replacement(session, prepared),
                    result: BindingResult::SessionSwitched,
                })
            }
        };
        let delta = delta?;
        if let PreparedChange::Replacement(session, _) = &delta.prepared {
            self.retirements.register(session)?;
        }
        Ok(delta)
    }
}

impl<M: Serialize + Send + 'static> SessionControl<M> {
    pub fn retirements(&self) -> crate::SessionRetirements {
        self.setup.retirements()
    }

    pub fn owner(&self) -> Option<Arc<crate::ApplicationOwner>> {
        self.setup.owner()
    }
    pub fn background(&self) -> Option<Arc<crate::BackgroundOwner>> {
        self.setup.background()
    }
    pub fn catalog(&self) -> &SessionCatalog {
        self.setup.catalog()
    }
    pub fn session(&self) -> &Arc<Session> {
        self.setup.session()
    }
    pub fn agent(&self) -> &Arc<Agent> {
        self.setup.agent()
    }
    pub fn resources(&self) -> &Resources {
        self.setup.resources()
    }
    pub fn commands(&self) -> &CommandCatalog {
        self.setup.commands()
    }
    pub fn instructions(&self) -> &str {
        self.setup.instructions()
    }
    pub fn session_id(&self) -> String {
        self.setup.session_id()
    }

    /// Reserve the one binding/result slot before any worker or write starts.
    /// No metadata is retained here; RPC keeps its one bounded command credit
    /// until BindingFinished is transferred. Reload alone may overlap live work.
    pub fn start_binding(&mut self, change: BindingChange) -> Result<BindingOperationId> {
        ensure!(
            self.binding.is_none(),
            "join or take the previous binding result first"
        );
        self.ensure_binding_start(matches!(&change, BindingChange::ReloadResources))?;
        tokio::runtime::Handle::try_current()
            .context("binding preparation requires a Tokio runtime")?;
        let source = BindingSource {
            application: self.setup.application.clone(),
            catalog: self.setup.catalog.clone(),
            session: self.setup.session.clone(),
            runtime: self.setup.runtime.clone(),
            questions: self.setup.questions.clone(),
            background: self.background(),
            retirements: self.setup.retirements.clone(),
        };
        let operation = BindingOperationId(uuid::Uuid::now_v7());
        let stop = source
            .background
            .as_ref()
            .map_or_else(CancellationToken::new, |owner| owner.work_stop());
        self.reserve_binding_result()?;
        // Reopening an already-released Session must wait for its physical
        // writer close, not fail spuriously or block a native worker behind
        // another task on the same bounded pool. Live external leases are not
        // awaited and remain ordinary AlreadyOpen refusals.
        let retirements = if matches!(change, BindingChange::SwitchSession(_)) {
            self.setup.retirements.released()
        } else {
            Vec::new()
        };
        self.binding = Some(BindingWork::Running {
            operation,
            work: Preparation::pending(stop),
            launch: Some(BindingLaunch {
                source,
                change,
                retirements,
            }),
        });
        if let Some(BindingWork::Running { work, launch, .. }) = self.binding.as_mut()
            && launch
                .as_ref()
                .is_some_and(|pending| pending.retirements.is_empty())
        {
            let BindingLaunch { source, change, .. } = launch.take().expect("pending launch");
            work.launch_blocking(move |stop| source.run(change, stop));
        }
        Ok(operation)
    }

    /// Borrow the retained worker. Abandoning this wait leaves both the worker
    /// and its eventual committed delta in the Controller's one binding slot.
    pub async fn join_binding(&mut self, operation: BindingOperationId) -> Result<BindingFinished> {
        let binding = self
            .binding
            .as_mut()
            .context("no retained binding operation")?;
        ensure!(
            binding.operation() == operation,
            "binding operation identity differs"
        );
        if binding.is_running() {
            let result = binding.join().await;
            // No await between consuming the worker and retaining its outcome.
            self.finish_binding(result);
        }
        Ok(self
            .take_binding_result()
            .expect("joined binding outcome retained"))
    }

    /// Transfer a finished result once. Running work is never removed here.
    pub fn take_binding_result(&mut self) -> Option<BindingFinished> {
        if !matches!(self.binding, Some(BindingWork::Finished(_))) {
            return None;
        }
        self.release_binding_result();
        match self.binding.take().expect("finished binding") {
            BindingWork::Finished(finished) => Some(finished),
            BindingWork::Running { .. } => unreachable!("finished binding checked"),
        }
    }

    /// Controller drive calls this immediately after BindingWork::join, even
    /// when stopped or closed. A successful Core commit cannot be rolled back
    /// by suppressing Host adoption; readers may sample a causal earlier cut.
    pub(crate) fn finish_binding(&mut self, result: Result<BindingDelta>) {
        let binding = self.binding.as_ref().expect("retained binding worker");
        let operation = binding.operation();
        let stop_requested = binding.is_stopped();
        let result = result.map(|delta| {
            match delta.prepared {
                PreparedChange::Resources(resources, commands) => {
                    self.completions.invalidate_binding();
                    self.setup.resources = resources;
                    self.setup.commands = commands;
                }
                PreparedChange::Runtime(runtime, commands) => {
                    self.completions.invalidate_binding();
                    self.setup.runtime = runtime;
                    self.setup.commands = commands;
                }
                PreparedChange::Preference => {
                    if matches!(delta.result, BindingResult::ReasoningSelected) {
                        self.completions.invalidate_binding();
                    }
                }
                PreparedChange::Replacement(session, prepared) => self.publish(session, prepared),
            }
            delta.result
        });
        self.binding = Some(BindingWork::Finished(BindingFinished {
            operation,
            result,
            stop_requested,
        }));
    }

    async fn change_binding(&mut self, change: BindingChange) -> Result<BindingResult> {
        let operation = self.start_binding(change)?;
        self.join_binding(operation).await?.result
    }

    /// Active/queued inputs retain their admitted instruction snapshots.
    pub async fn reload_resources(&mut self) -> Result<()> {
        self.change_binding(BindingChange::ReloadResources).await?;
        Ok(())
    }
    pub async fn select_model(&mut self, model: ModelRef) -> Result<()> {
        self.change_binding(BindingChange::SelectModel(model))
            .await?;
        Ok(())
    }
    pub async fn select_reasoning(&mut self, reasoning: Reasoning) -> Result<()> {
        self.change_binding(BindingChange::SelectReasoning(reasoning))
            .await?;
        Ok(())
    }
    pub async fn set_name(&mut self, name: Option<String>) -> Result<()> {
        self.change_binding(BindingChange::SetName(name)).await?;
        Ok(())
    }
    pub async fn new_session(&mut self) -> Result<()> {
        self.change_binding(BindingChange::NewSession).await?;
        Ok(())
    }
    pub async fn clone_session(&mut self) -> Result<String> {
        match self.change_binding(BindingChange::CloneSession).await? {
            BindingResult::SessionCloned { session_id } => Ok(session_id),
            _ => unreachable!("clone worker returns its Session identity"),
        }
    }
    pub async fn fork_session(&mut self, point: ForkPoint) -> Result<String> {
        match self
            .change_binding(BindingChange::ForkSession(point))
            .await?
        {
            BindingResult::SessionForked { session_id } => Ok(session_id),
            _ => unreachable!("fork worker returns its Session identity"),
        }
    }
    pub async fn navigate_context(
        &mut self,
        expected: ContextPosition,
        head_entry: u64,
    ) -> Result<ContextPosition> {
        match self
            .change_binding(BindingChange::NavigateContext {
                expected,
                head_entry,
            })
            .await?
        {
            BindingResult::ContextNavigated(position) => Ok(position),
            _ => unreachable!("navigation worker returns the committed position"),
        }
    }
    pub async fn switch_session(&mut self, path: PathBuf) -> Result<()> {
        self.change_binding(BindingChange::SwitchSession(path))
            .await?;
        Ok(())
    }

    fn publish(&mut self, session: Arc<Session>, prepared: PreparedBinding) {
        if let Some(questions) = &self.setup.questions {
            questions.rebind_session(self.owner, session.id());
        }
        self.completions.rebind(session.id());
        self.setup.session = session;
        self.setup.runtime = prepared.runtime;
        self.setup.resources = prepared.resources;
        self.setup.commands = prepared.commands;
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "binding_io_tests.rs"]
mod io_tests;
