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

    /// Capture the Session without borrowing the mutable client binding. The
    /// returned operation grants no external authority until durable admission.
    pub fn run_user_shell(
        &self,
        command: &str,
        stop: CancellationToken,
        exclude_from_context: bool,
    ) -> impl std::future::Future<Output = Result<CodingToolOutput>> + Send + use<> {
        let session = self.session.clone();
        let command = command.to_owned();
        async move {
            let tools = LocalTools::new(session.cwd())?;
            let permit = session
                .begin_user_shell(command, exclude_from_context, stop.clone())
                .await?;
            let output = tools.run_user_shell(permit.command(), stop).await;
            permit.record(output.value.clone(), output.is_error)?;
            Ok(output)
        }
    }

    /// Capture image sources against this binding, without borrowing it while
    /// normalization runs. Clients retain the future until settlement.
    pub fn prepare_images(
        &self,
        mut sources: Vec<crate::image_input::ImageSource>,
        stop: CancellationToken,
    ) -> impl std::future::Future<Output = Result<Vec<crate::image_input::LoadedImage>>> + Send + use<>
    {
        for source in &mut sources {
            if let crate::image_input::ImageSource::Path(path) = source
                && !path.is_absolute()
            {
                *path = self.session.cwd().join(&*path);
            }
        }
        crate::image_input::prepare_images(
            &self.selected,
            sources,
            self.agent.limits().max_request_bytes,
            stop,
        )
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
        agent.select_model(&self.session)?;
        self.selected = selected;
        self.agent = agent;
        Ok(())
    }

    pub fn select_reasoning(&self, reasoning: ion_ai::Reasoning) -> Result<()> {
        self.agent.select_reasoning(&self.session, reasoning)?;
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
        agent.select_model(&session)?;
        self.publish(session, selected, agent, resources);
        Ok(())
    }

    pub fn clone_session(&mut self) -> Result<String> {
        let (resources, agent) = self.prepare(self.session.cwd(), &self.selected)?;
        agent.validate_reasoning(self.session.reasoning()?)?;
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
        let selected_turn = self
            .session
            .view()?
            .turns()
            .into_iter()
            .find(|item| item.turn == turn)
            .context("selected Turn does not exist")?;
        let selected = self.host.models().choose(
            None,
            None,
            Some(selected_turn.model),
            self.host.credentials(),
        )?;
        let (resources, agent) = self.prepare(self.session.cwd(), &selected)?;
        agent.validate_reasoning(selected_turn.reasoning)?;
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
        let mut binding = SessionBinding::new(host, session.clone(), selected, None, None).unwrap();
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

        fs::rename(root.join("moved-work"), &cwd).unwrap();
        let running = binding.run_user_shell("printf captured", CancellationToken::new(), false);
        binding.new_session().unwrap();
        // Preparation is inert, and a later binding change cannot redirect it.
        assert_eq!(session.view().unwrap().entries, before.entries);
        assert!(!running.await.unwrap().is_error);
        let original = session.view().unwrap();
        assert!(
            matches!(original.entries.last(), Some(ion_core::SessionEntry::UserShellSettled { outcome: ion_core::UserShellOutcome::Observed { output, .. }, .. }) if output["stdout"] == "captured")
        );
        assert!(
            !binding
                .session()
                .view()
                .unwrap()
                .entries
                .iter()
                .any(|entry| matches!(entry, ion_core::SessionEntry::UserShellAdmitted { .. }))
        );
        drop(binding);
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn idle_override_and_effort_restore_as_one_selection() {
        let root = std::env::temp_dir().join(format!("ion-binding-{}", uuid::Uuid::now_v7()));
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        let host = Arc::new(Host::new(root.join("config"), root.join("state")));
        let mut limited = route("limited");
        limited.wire = Some(Wire::LlamaCppNoThinking);
        let limited = host.models().save_default(&limited).unwrap();
        let selected = host.models().save_default(&route("override")).unwrap();
        let path = host.sessions(cwd.clone()).new_path().unwrap();
        let session = Arc::new(CodingSession::create(&path, &cwd).unwrap());
        session.select_model(limited.identity()).unwrap();
        let binding =
            SessionBinding::new(host.clone(), session.clone(), selected, None, None).unwrap();
        binding.select_reasoning(ion_ai::Reasoning::High).unwrap();
        drop(binding);
        drop(session);
        let session = Arc::new(CodingSession::open(path).unwrap());
        let selected = host
            .models()
            .choose(
                None,
                None,
                session.view().unwrap().last_model,
                host.credentials(),
            )
            .unwrap();
        let binding = SessionBinding::new(host.clone(), session, selected, None, None).unwrap();
        assert_eq!(binding.selected().model, "override");
        assert_eq!(
            binding.session().reasoning().unwrap(),
            ion_ai::Reasoning::High
        );
        drop(binding);
        drop(host);
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
        let mut binding =
            SessionBinding::new(host.clone(), first, first_model, None, None).unwrap();

        binding.select_reasoning(ion_ai::Reasoning::High).unwrap();
        let entries = binding.session().view().unwrap().entries;
        assert!(
            binding
                .select_reasoning(ion_ai::Reasoning::BudgetTokens(128))
                .is_err()
        );
        assert_eq!(binding.session().view().unwrap().entries, entries);
        let mut limited = route("limited");
        limited.wire = Some(Wire::LlamaCppNoThinking);
        host.models().save_default(&limited).unwrap();
        assert!(
            binding
                .select_model(ion_ai::ModelRef {
                    provider: "desktop".into(),
                    model: "limited".into()
                })
                .is_err()
        );
        assert_eq!(binding.selected().model, "one");
        assert_eq!(binding.session().view().unwrap().entries, entries);
        host.models().save_default(&route("two")).unwrap();
        binding.new_session().unwrap();
        let second_id = binding.session_id();
        assert_eq!(binding.selected().model, "two");
        assert_eq!(
            binding.session().reasoning().unwrap(),
            ion_ai::Reasoning::ProviderDefault
        );

        let moved = root.join("moved-work");
        fs::rename(&cwd, &moved).unwrap();
        assert!(binding.switch_session(first_path.clone()).is_err());
        assert_eq!(binding.session_id(), second_id);
        assert_eq!(binding.selected().model, "two");
        fs::rename(&moved, &cwd).unwrap();

        binding.switch_session(first_path).unwrap();
        assert_eq!(binding.selected().model, "one");
        assert_eq!(
            binding.session().reasoning().unwrap(),
            ion_ai::Reasoning::High
        );
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
