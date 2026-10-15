//! Immutable service preflight and prepare-before-publish binding replacement.
use std::{fs, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, ensure};
use ion_ai::ModelRef;
use ion_core::{CodingAgent, CodingSession, ForkPoint};
use serde::Serialize;

use crate::{Application, ApplicationRuntime, Resources, SessionCatalog, SessionControl};

/// Configuration, not execution. Consume this to create the sole control owner.
pub struct SessionSetup {
    application: Arc<dyn Application>,
    pub(crate) catalog: SessionCatalog,
    pub(crate) session: Arc<CodingSession>,
    pub(crate) runtime: ApplicationRuntime,
    pub(crate) resources: Resources,
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
    ) -> Result<Self> {
        let model = model.cloned().or(session.model_preference()?);
        let resources = application.resources(session.cwd())?;
        let runtime = application.runtime(session.cwd(), model.as_ref())?;
        validate_model(&runtime, model.as_ref())?;
        Self::from_prepared(session, catalog, application, resources, runtime, reasoning)
    }

    pub(crate) fn from_prepared(
        session: Arc<CodingSession>,
        catalog: SessionCatalog,
        application: Arc<dyn Application>,
        resources: Resources,
        runtime: ApplicationRuntime,
        reasoning: Option<ion_ai::Reasoning>,
    ) -> Result<Self> {
        ensure!(
            catalog.cwd() == session.cwd(),
            "catalog and Session working directories differ"
        );
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
        })
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
    pub fn instructions(&self) -> &str {
        self.resources.instructions()
    }
    pub fn session_id(&self) -> String {
        self.session
            .path()
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
    pub fn into_control<M: Serialize + Send + 'static>(self) -> SessionControl<M> {
        SessionControl::from_setup(self)
    }

    fn prepare(
        &self,
        session: &CodingSession,
        model: Option<&ModelRef>,
    ) -> Result<(Resources, ApplicationRuntime)> {
        let resources = self.application.resources(session.cwd())?;
        let runtime = self.application.runtime(session.cwd(), model)?;
        validate_model(&runtime, model)?;
        runtime.agent.validate_reasoning(session.reasoning()?)?;
        Ok((resources, runtime))
    }
}

fn validate_model(runtime: &ApplicationRuntime, requested: Option<&ModelRef>) -> Result<()> {
    ensure!(
        requested.is_none_or(|model| model == runtime.agent.model()),
        "application returned a different model than requested"
    );
    Ok(())
}

impl<M: Serialize + Send + 'static> SessionControl<M> {
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
    pub fn instructions(&self) -> &str {
        self.setup.instructions()
    }
    pub fn session_id(&self) -> String {
        self.setup.session_id()
    }

    /// Reload affects future admission; active/queued inputs retain their snapshot.
    pub fn reload_resources(&mut self) -> Result<()> {
        self.setup.resources = self.setup.application.resources(self.setup.session.cwd())?;
        Ok(())
    }

    pub fn select_model(&mut self, model: ModelRef) -> Result<()> {
        self.ensure_replaceable()?;
        let runtime = self
            .setup
            .application
            .runtime(self.setup.session.cwd(), Some(&model))?;
        validate_model(&runtime, Some(&model))?;
        runtime.agent.select_model(&self.setup.session)?;
        self.setup.runtime = runtime;
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
        let runtime = self.setup.application.runtime(previous.cwd(), None)?;
        runtime
            .agent
            .validate_reasoning(ion_ai::Reasoning::ProviderDefault)?;
        let path = self.setup.catalog.new_path()?;
        let session = Arc::new(CodingSession::create(&path, previous.cwd())?);
        runtime.agent.select_model(&session)?;
        self.publish(session, runtime, resources);
        Ok(())
    }

    pub fn clone_session(&mut self) -> Result<String> {
        self.ensure_replaceable()?;
        let previous = self.setup.session.clone();
        let _idle = previous.reserve_idle()?;
        let resources = self.setup.application.resources(previous.cwd())?;
        // Retain the resolved configuration, not its lookup key. An unsaved
        // route must remain usable; a changed store must not redirect a clone.
        let runtime = self.setup.runtime.clone();
        runtime.agent.validate_reasoning(previous.reasoning()?)?;
        let path = self.setup.catalog.new_path()?;
        let session = Arc::new(previous.clone_to(&path)?);
        let id = path
            .file_stem()
            .context("cloned session has no ID")?
            .to_string_lossy()
            .into_owned();
        self.publish(session, runtime, resources);
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
        let runtime = self
            .setup
            .application
            .runtime(previous.cwd(), Some(&selected_turn.model))?;
        validate_model(&runtime, Some(&selected_turn.model))?;
        runtime.agent.validate_reasoning(selected_turn.reasoning)?;
        let path = self.setup.catalog.new_path()?;
        let session = Arc::new(previous.fork_to(&path, point)?);
        let id = path
            .file_stem()
            .context("forked session has no ID")?
            .to_string_lossy()
            .into_owned();
        self.publish(session, runtime, resources);
        Ok(id)
    }

    pub fn switch_session(&mut self, path: PathBuf) -> Result<()> {
        self.ensure_replaceable()?;
        let previous = self.setup.session.clone();
        let _idle = previous.reserve_idle()?;
        let path = self.setup.catalog.resolve_explicit(path)?;
        if fs::canonicalize(previous.path())? == fs::canonicalize(&path)? {
            return self.reload_resources();
        }
        let session = Arc::new(CodingSession::open(path)?);
        ensure!(
            session.cwd() == previous.cwd(),
            "session belongs to another working directory"
        );
        let model = session.model_preference()?;
        let (resources, runtime) = self.setup.prepare(&session, model.as_ref())?;
        self.publish(session, runtime, resources);
        Ok(())
    }

    fn publish(
        &mut self,
        session: Arc<CodingSession>,
        runtime: ApplicationRuntime,
        resources: Resources,
    ) {
        self.setup.session = session;
        self.setup.runtime = runtime;
        self.setup.resources = resources;
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
