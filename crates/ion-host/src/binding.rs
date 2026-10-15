//! Immutable service preflight and prepare-before-publish binding replacement.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use ion_ai::ModelRef;
use ion_core::{CodingAgent, CodingSession, CodingToolSource, ForkPoint};
use serde::Serialize;

use crate::{Host, Resources, Selection, SessionCatalog, SessionControl};

/// Preflight once before choosing the client's metadata type. This value owns
/// configuration, not execution; consume it to create the sole control owner.
pub struct SessionSetup {
    pub(crate) host: Arc<Host>,
    pub(crate) catalog: SessionCatalog,
    pub(crate) session: Arc<CodingSession>,
    pub(crate) selected: Selection,
    pub(crate) agent: Arc<CodingAgent>,
    pub(crate) resources: Resources,
    external_tools: Option<Arc<dyn CodingToolSource>>,
}

impl SessionSetup {
    pub fn new(
        host: Arc<Host>,
        session: Arc<CodingSession>,
        selected: Selection,
        external_tools: Option<Arc<dyn CodingToolSource>>,
        reasoning: Option<ion_ai::Reasoning>,
    ) -> Result<Self> {
        let catalog = host.sessions(session.cwd().to_path_buf());
        let resources = host.resources(session.cwd())?;
        let agent =
            host.agent_with_optional_tools(session.cwd(), &selected, external_tools.clone())?;
        if let Some(reasoning) = reasoning {
            agent.select_reasoning(&session, reasoning)?;
        } else {
            agent.validate_reasoning(session.reasoning()?)?;
        }
        Ok(Self {
            host,
            catalog,
            session,
            selected,
            agent,
            resources,
            external_tools,
        })
    }

    pub fn host(&self) -> &Host {
        &self.host
    }
    pub fn catalog(&self) -> &SessionCatalog {
        &self.catalog
    }
    pub fn session(&self) -> &Arc<CodingSession> {
        &self.session
    }
    pub fn selected(&self) -> &Selection {
        &self.selected
    }
    pub fn agent(&self) -> &Arc<CodingAgent> {
        &self.agent
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

    fn prepare(&self, cwd: &Path, selected: &Selection) -> Result<(Resources, Arc<CodingAgent>)> {
        let resources = self.host.resources(cwd)?;
        let agent =
            self.host
                .agent_with_optional_tools(cwd, selected, self.external_tools.clone())?;
        Ok((resources, agent))
    }
}

impl<M: Serialize + Send + 'static> SessionControl<M> {
    pub fn host(&self) -> &Host {
        self.setup.host()
    }
    pub fn catalog(&self) -> &SessionCatalog {
        self.setup.catalog()
    }
    pub fn session(&self) -> &Arc<CodingSession> {
        self.setup.session()
    }
    pub fn selected(&self) -> &Selection {
        self.setup.selected()
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

    /// Reload affects future admission. Active requests and queued expansions
    /// retain their captured resources. Resource inspection remains available busy.
    pub fn reload_resources(&mut self) -> Result<()> {
        self.setup.resources = self.setup.host.resources(self.setup.session.cwd())?;
        Ok(())
    }

    pub fn select_model(&mut self, model: ModelRef) -> Result<()> {
        self.ensure_replaceable()?;
        let selected = self.setup.host.models().resolve_identity(&model)?;
        let agent = self.setup.host.agent_with_optional_tools(
            self.setup.session.cwd(),
            &selected,
            self.setup.external_tools.clone(),
        )?;
        agent.select_model(&self.setup.session)?;
        self.setup.selected = selected;
        self.setup.agent = agent;
        Ok(())
    }

    pub fn select_reasoning(&self, reasoning: ion_ai::Reasoning) -> Result<()> {
        self.ensure_replaceable()?;
        self.setup
            .agent
            .select_reasoning(&self.setup.session, reasoning)?;
        Ok(())
    }

    pub fn new_session(&mut self) -> Result<()> {
        self.ensure_replaceable()?;
        let previous = self.setup.session.clone();
        let _idle = previous.reserve_idle()?;
        let selected =
            self.setup
                .host
                .models()
                .choose(None, None, None, self.setup.host.credentials())?;
        let (resources, agent) = self.setup.prepare(self.setup.session.cwd(), &selected)?;
        let path = self.setup.catalog.new_path()?;
        let session = Arc::new(CodingSession::create(&path, self.setup.session.cwd())?);
        agent.select_model(&session)?;
        self.publish(session, selected, agent, resources);
        Ok(())
    }

    pub fn clone_session(&mut self) -> Result<String> {
        self.ensure_replaceable()?;
        let previous = self.setup.session.clone();
        let _idle = previous.reserve_idle()?;
        let (resources, agent) = self
            .setup
            .prepare(self.setup.session.cwd(), &self.setup.selected)?;
        agent.validate_reasoning(self.setup.session.reasoning()?)?;
        let path = self.setup.catalog.new_path()?;
        let session = Arc::new(self.setup.session.clone_to(&path)?);
        let id = path
            .file_stem()
            .context("cloned session has no ID")?
            .to_string_lossy()
            .into_owned();
        self.publish(session, self.setup.selected.clone(), agent, resources);
        Ok(id)
    }

    pub fn fork_session(&mut self, point: ForkPoint) -> Result<String> {
        self.ensure_replaceable()?;
        let previous = self.setup.session.clone();
        let _idle = previous.reserve_idle()?;
        let turn = match point {
            ForkPoint::BeforeTurn(turn) | ForkPoint::AfterTurn(turn) => turn,
        };
        let selected_turn = self
            .setup
            .session
            .view()?
            .turns()
            .into_iter()
            .find(|item| item.turn == turn)
            .context("selected Turn does not exist")?;
        let selected = self.setup.host.models().choose(
            None,
            None,
            Some(selected_turn.model),
            self.setup.host.credentials(),
        )?;
        let (resources, agent) = self.setup.prepare(self.setup.session.cwd(), &selected)?;
        agent.validate_reasoning(selected_turn.reasoning)?;
        let path = self.setup.catalog.new_path()?;
        let session = Arc::new(self.setup.session.fork_to(&path, point)?);
        let id = path
            .file_stem()
            .context("forked session has no ID")?
            .to_string_lossy()
            .into_owned();
        self.publish(session, selected, agent, resources);
        Ok(id)
    }

    pub fn switch_session(&mut self, path: PathBuf) -> Result<()> {
        self.ensure_replaceable()?;
        let previous = self.setup.session.clone();
        let _idle = previous.reserve_idle()?;
        let path = self.setup.catalog.resolve_explicit(path)?;
        if fs::canonicalize(self.setup.session.path())? == fs::canonicalize(&path)? {
            return self.reload_resources();
        }
        let session = Arc::new(CodingSession::open(path)?);
        ensure!(
            session.cwd() == self.setup.session.cwd(),
            "session belongs to another working directory"
        );
        let selected = self.setup.host.models().choose(
            None,
            None,
            session.model_preference()?,
            self.setup.host.credentials(),
        )?;
        let (resources, agent) = self.setup.prepare(session.cwd(), &selected)?;
        agent.validate_reasoning(session.reasoning()?)?;
        self.publish(session, selected, agent, resources);
        Ok(())
    }

    fn publish(
        &mut self,
        session: Arc<CodingSession>,
        selected: Selection,
        agent: Arc<CodingAgent>,
        resources: Resources,
    ) {
        self.setup.session = session;
        self.setup.selected = selected;
        self.setup.agent = agent;
        self.setup.resources = resources;
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
mod tests;
