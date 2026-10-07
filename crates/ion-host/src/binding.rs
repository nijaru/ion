//! One active Session/model/resource binding shared by interactive clients.
//! Conversation facts and Turn execution remain in `ion-core`.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use ion_ai::ModelRef;
use ion_core::{CodingAgent, CodingSession, CodingToolOutput, CodingToolSource, ForkPoint};

use crate::{Host, LocalTools, Resources, Selection, SessionCatalog};
use tokio_util::sync::CancellationToken;

pub struct SessionBinding {
    host: Arc<Host>,
    catalog: SessionCatalog,
    session: Arc<CodingSession>,
    selected: Selection,
    agent: Arc<CodingAgent>,
    resources: Resources,
    external_tools: Option<Arc<dyn CodingToolSource>>,
}

impl SessionBinding {
    pub fn new(
        host: Arc<Host>,
        session: Arc<CodingSession>,
        selected: Selection,
        external_tools: Option<Arc<dyn CodingToolSource>>,
    ) -> Result<Self> {
        let catalog = host.sessions(session.cwd().to_path_buf());
        let resources = host.resources(session.cwd())?;
        let agent =
            host.agent_with_optional_tools(session.cwd(), &selected, external_tools.clone())?;
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

    pub async fn run_user_shell(
        &self,
        command: &str,
        stop: CancellationToken,
        exclude_from_context: bool,
    ) -> Result<CodingToolOutput> {
        let tools = LocalTools::new(self.session.cwd())?;
        let permit = self
            .session
            .begin_user_shell(command.to_owned(), exclude_from_context, stop.clone())
            .await?;
        let output = tools.run_user_shell(permit.command(), stop).await;
        permit.record(output.value.clone(), output.is_error)?;
        Ok(output)
    }

    pub fn session_id(&self) -> String {
        self.session
            .path()
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    fn prepare(&self, cwd: &Path, selected: &Selection) -> Result<(Resources, Arc<CodingAgent>)> {
        let resources = self.host.resources(cwd)?;
        let agent =
            self.host
                .agent_with_optional_tools(cwd, selected, self.external_tools.clone())?;
        Ok((resources, agent))
    }

    pub fn reload_resources(&mut self) -> Result<()> {
        self.resources = self.host.resources(self.session.cwd())?;
        Ok(())
    }

    pub fn select_model(&mut self, model: ModelRef) -> Result<()> {
        let selected = self.host.models().resolve_identity(&model)?;
        let agent = self.host.agent_with_optional_tools(
            self.session.cwd(),
            &selected,
            self.external_tools.clone(),
        )?;
        self.session.select_model(model)?;
        self.selected = selected;
        self.agent = agent;
        Ok(())
    }

    pub fn new_session(&mut self) -> Result<()> {
        let selected = self
            .host
            .models()
            .choose(None, None, None, self.host.credentials())?;
        let (resources, agent) = self.prepare(self.session.cwd(), &selected)?;
        let path = self.catalog.new_path()?;
        let session = Arc::new(CodingSession::create(&path, self.session.cwd())?);
        session.select_model(selected.identity())?;
        self.publish(session, selected, agent, resources);
        Ok(())
    }

    pub fn clone_session(&mut self) -> Result<String> {
        let (resources, agent) = self.prepare(self.session.cwd(), &self.selected)?;
        let path = self.catalog.new_path()?;
        let session = Arc::new(self.session.clone_to(&path)?);
        let id = path
            .file_stem()
            .context("cloned session has no ID")?
            .to_string_lossy()
            .into_owned();
        self.publish(session, self.selected.clone(), agent, resources);
        Ok(id)
    }

    pub fn fork_session(&mut self, point: ForkPoint) -> Result<String> {
        let turn = match point {
            ForkPoint::BeforeTurn(turn) | ForkPoint::AfterTurn(turn) => turn,
        };
        let model = self
            .session
            .view()?
            .turns()
            .into_iter()
            .find(|item| item.turn == turn)
            .context("selected Turn does not exist")?
            .model;
        let selected =
            self.host
                .models()
                .choose(None, None, Some(model), self.host.credentials())?;
        let (resources, agent) = self.prepare(self.session.cwd(), &selected)?;
        let path = self.catalog.new_path()?;
        let session = Arc::new(self.session.fork_to(&path, point)?);
        let id = path
            .file_stem()
            .context("forked session has no ID")?
            .to_string_lossy()
            .into_owned();
        self.publish(session, selected, agent, resources);
        Ok(id)
    }

    pub fn switch_session(&mut self, path: PathBuf) -> Result<()> {
        let path = self.catalog.resolve_explicit(path)?;
        if fs::canonicalize(self.session.path())? == fs::canonicalize(&path)? {
            return self.reload_resources();
        }
        let session = Arc::new(CodingSession::open(path)?);
        let view = session.view()?;
        ensure!(
            view.cwd == self.session.cwd(),
            "session belongs to another working directory"
        );
        let selected =
            self.host
                .models()
                .choose(None, None, view.last_model, self.host.credentials())?;
        let (resources, agent) = self.prepare(session.cwd(), &selected)?;
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
        self.session = session;
        self.selected = selected;
        self.agent = agent;
        self.resources = resources;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{SavedSelection, Wire};

    fn route(model: &str) -> SavedSelection {
        SavedSelection {
            provider: "desktop".into(),
            model: model.into(),
            endpoint: Some("http://127.0.0.1:43129/v1/chat/completions".into()),
            wire: Some(Wire::ChatCompletions),
            api_key_env: None,
            image_input: false,
        }
    }

    #[tokio::test]
    async fn direct_shell_preflights_before_recovery_or_admission() {
        let root = std::env::temp_dir().join(format!("ion-binding-{}", uuid::Uuid::now_v7()));
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        let host = Arc::new(Host::new(root.join("config"), root.join("state")));
        let selected = host.models().save_default(&route("one")).unwrap();
        let path = host.sessions(cwd.clone()).new_path().unwrap();
        let session = Arc::new(CodingSession::create(&path, &cwd).unwrap());
        let binding = SessionBinding::new(host, session.clone(), selected, None).unwrap();
        drop(
            session
                .begin_user_shell("old command".into(), false, CancellationToken::new())
                .await
                .unwrap(),
        );
        let before = session.view().unwrap();
        fs::rename(&cwd, root.join("moved-work")).unwrap();
        assert!(
            binding
                .run_user_shell("touch must-not-dispatch", CancellationToken::new(), false)
                .await
                .is_err()
        );
        assert_eq!(session.view().unwrap().entries, before.entries);
        assert!(!root.join("moved-work/must-not-dispatch").exists());
        drop(binding);
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn replacement_restores_model_and_keeps_old_binding_on_preflight_failure() {
        let root = std::env::temp_dir().join(format!("ion-binding-{}", uuid::Uuid::now_v7()));
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        let host = Arc::new(Host::new(root.join("config"), root.join("state")));
        let first_model = host.models().save_default(&route("one")).unwrap();
        let first_path = host.sessions(cwd.clone()).new_path().unwrap();
        let first = Arc::new(CodingSession::create(&first_path, &cwd).unwrap());
        first.select_model(first_model.identity()).unwrap();
        let mut binding = SessionBinding::new(host.clone(), first, first_model, None).unwrap();

        host.models().save_default(&route("two")).unwrap();
        binding.new_session().unwrap();
        let second_id = binding.session_id();
        assert_eq!(binding.selected().model, "two");

        let moved = root.join("moved-work");
        fs::rename(&cwd, &moved).unwrap();
        assert!(binding.switch_session(first_path.clone()).is_err());
        assert_eq!(binding.session_id(), second_id);
        assert_eq!(binding.selected().model, "two");
        fs::rename(&moved, &cwd).unwrap();

        binding.switch_session(first_path).unwrap();
        assert_eq!(binding.selected().model, "one");
        assert_ne!(binding.session_id(), second_id);
        fs::create_dir_all(cwd.join(".ion/prompts")).unwrap();
        fs::write(cwd.join(".ion/prompts/hello.md"), "Hello $1").unwrap();
        binding.reload_resources().unwrap();
        assert!(
            binding
                .resources()
                .templates()
                .any(|item| item.name == "hello")
        );

        drop(binding);
        drop(host);
        fs::remove_dir_all(root).unwrap();
    }
}
