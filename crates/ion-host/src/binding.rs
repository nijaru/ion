//! Immutable service preflight and prepare-before-publish binding replacement.
use std::{fs, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, ensure};
use ion_ai::ModelRef;
use ion_core::{CodingAgent, CodingSession, ForkPoint};
use serde::Serialize;

use crate::{
    Application, ApplicationRuntime, CommandCatalog, QuestionCustody, Resources, SessionCatalog,
    SessionControl,
};

/// Configuration, not execution. Consume this to create the sole control owner.
pub struct SessionSetup {
    application: Arc<dyn Application>,
    pub(crate) catalog: SessionCatalog,
    pub(crate) session: Arc<CodingSession>,
    pub(crate) runtime: ApplicationRuntime,
    pub(crate) resources: Resources,
    pub(crate) commands: CommandCatalog,
    pub(crate) questions: Option<Arc<QuestionCustody>>,
}

impl SessionSetup {
    /// An omitted model restores the Session preference, then the application's
    /// default for a fresh Session. Omitted reasoning restores its saved value.
    pub fn new(
        session: Arc<CodingSession>,
        catalog: SessionCatalog,
        application: Arc<dyn Application>,
        model: Option<&ModelRef>,
        reasoning: Option<ion_ai::Reasoning>,
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

    pub(crate) fn from_prepared(
        session: Arc<CodingSession>,
        catalog: SessionCatalog,
        application: Arc<dyn Application>,
        resources: Resources,
        runtime: ApplicationRuntime,
        reasoning: Option<ion_ai::Reasoning>,
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
        Ok(Self {
            application,
            catalog,
            session,
            runtime,
            resources,
            commands,
            questions,
        })
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
    pub fn session(&self) -> &Arc<CodingSession> {
        &self.session
    }
    pub fn agent(&self) -> &Arc<CodingAgent> {
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

    fn prepare(
        &self,
        session: &CodingSession,
        model: Option<&ModelRef>,
    ) -> Result<(Resources, CommandCatalog, ApplicationRuntime)> {
        let resources = self.application.resources(session.cwd())?;
        let commands = self.application.commands(session.cwd())?;
        let runtime = self
            .application
            .runtime(session.cwd(), model, self.questions.clone())?;
        validate_model(&runtime, model)?;
        runtime.agent.validate_reasoning(session.reasoning()?)?;
        Ok((resources, commands, runtime))
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

impl<M: Serialize + Send + 'static> SessionControl<M> {
    pub fn owner(&self) -> Option<Arc<crate::ApplicationOwner>> {
        self.setup.owner()
    }

    pub fn background(&self) -> Option<Arc<crate::BackgroundOwner>> {
        self.setup.background()
    }

    pub fn catalog(&self) -> &SessionCatalog {
        self.setup.catalog()
    }
    pub fn session(&self) -> &Arc<CodingSession> {
        self.setup.session()
    }
    pub fn agent(&self) -> &Arc<CodingAgent> {
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

    /// Reload affects future admission; active/queued inputs retain their snapshot.
    pub fn reload_resources(&mut self) -> Result<()> {
        let resources = self.setup.application.resources(self.setup.session.cwd())?;
        let commands = self.setup.application.commands(self.setup.session.cwd())?;
        self.setup.resources = resources;
        self.setup.commands = commands;
        Ok(())
    }

    pub fn select_model(&mut self, model: ModelRef) -> Result<()> {
        self.ensure_replaceable()?;
        let commands = self.setup.application.commands(self.setup.session.cwd())?;
        let runtime = self.setup.application.runtime(
            self.setup.session.cwd(),
            Some(&model),
            self.setup.questions.clone(),
        )?;
        validate_model(&runtime, Some(&model))?;
        runtime.agent.select_model(&self.setup.session)?;
        self.setup.runtime = runtime;
        self.setup.commands = commands;
        Ok(())
    }

    pub fn select_reasoning(&self, reasoning: ion_ai::Reasoning) -> Result<()> {
        self.ensure_replaceable()?;
        self.setup
            .runtime
            .agent
            .select_reasoning(&self.setup.session, reasoning)?;
        Ok(())
    }

    pub fn new_session(&mut self) -> Result<()> {
        self.ensure_replaceable()?;
        let previous = self.setup.session.clone();
        let _idle = previous.reserve_idle()?;
        // Preflight before allocating/publishing a new database. New Sessions
        // intentionally reset reasoning, unlike conversation copies.
        let resources = self.setup.application.resources(previous.cwd())?;
        let commands = self.setup.application.commands(previous.cwd())?;
        let runtime =
            self.setup
                .application
                .runtime(previous.cwd(), None, self.setup.questions.clone())?;
        runtime
            .agent
            .validate_reasoning(ion_ai::Reasoning::ProviderDefault)?;
        let path = self.setup.catalog.new_path()?;
        let session = Arc::new(CodingSession::create(&path, previous.cwd())?);
        runtime.agent.select_model(&session)?;
        self.publish(session, runtime, resources, commands);
        Ok(())
    }

    pub fn clone_session(&mut self) -> Result<String> {
        self.ensure_replaceable()?;
        let previous = self.setup.session.clone();
        let _idle = previous.reserve_idle()?;
        let resources = self.setup.application.resources(previous.cwd())?;
        let commands = self.setup.application.commands(previous.cwd())?;
        // Retain the resolved configuration, not its lookup key. An unsaved
        // route must remain usable; a changed store must not redirect a clone.
        let runtime = self.setup.runtime.clone();
        runtime.agent.validate_reasoning(previous.reasoning()?)?;
        let path = self.setup.catalog.new_path()?;
        let session = Arc::new(previous.clone_to(&path)?);
        let id = session.id().to_string();
        self.publish(session, runtime, resources, commands);
        Ok(id)
    }

    pub fn fork_session(&mut self, point: ForkPoint) -> Result<String> {
        self.ensure_replaceable()?;
        let previous = self.setup.session.clone();
        let _idle = previous.reserve_idle()?;
        let turn = match point {
            ForkPoint::BeforeTurn(turn) | ForkPoint::AfterTurn(turn) => turn,
        };
        let selected_turn = previous
            .view()?
            .turns()
            .into_iter()
            .find(|item| item.turn == turn)
            .context("selected Turn does not exist")?;
        let resources = self.setup.application.resources(previous.cwd())?;
        let commands = self.setup.application.commands(previous.cwd())?;
        let runtime = self.setup.application.runtime(
            previous.cwd(),
            Some(&selected_turn.model),
            self.setup.questions.clone(),
        )?;
        validate_model(&runtime, Some(&selected_turn.model))?;
        runtime.agent.validate_reasoning(selected_turn.reasoning)?;
        let path = self.setup.catalog.new_path()?;
        let session = Arc::new(previous.fork_to(&path, point)?);
        let id = session.id().to_string();
        self.publish(session, runtime, resources, commands);
        Ok(id)
    }

    pub fn switch_session(&mut self, path: PathBuf) -> Result<()> {
        self.ensure_replaceable()?;
        let previous = self.setup.session.clone();
        let _idle = previous.reserve_idle()?;
        let path = fs::canonicalize(self.setup.catalog.resolve_explicit(path)?)?;
        if previous.path() == path {
            return self.reload_resources();
        }
        let session = match self.background().and_then(|owner| owner.parent(&path)) {
            Some(parent) => parent,
            None => Arc::new(CodingSession::open(path)?),
        };
        ensure!(
            session.cwd() == previous.cwd(),
            "session belongs to another working directory"
        );
        let model = session.model_preference()?;
        let (resources, commands, runtime) = self.setup.prepare(&session, model.as_ref())?;
        self.publish(session, runtime, resources, commands);
        Ok(())
    }

    fn publish(
        &mut self,
        session: Arc<CodingSession>,
        runtime: ApplicationRuntime,
        resources: Resources,
        commands: CommandCatalog,
    ) {
        if let Some(questions) = &self.setup.questions {
            questions.rebind_session(self.owner, session.id());
        }
        self.setup.session = session;
        self.setup.runtime = runtime;
        self.setup.resources = resources;
        self.setup.commands = commands;
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
