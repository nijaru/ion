//! Application-owned assembly. Shared control never adds capabilities here.
use std::{path::Path, sync::Arc};

use anyhow::{Context, Result};
use ion_ai::{BoxFuture, ModelRef};
use ion_core::{CodingAgent, CodingSession, CodingToolOutput, CodingToolSource, ToolSelection};
use tokio_util::sync::CancellationToken;

use crate::{
    CommandCatalog, Host, LocalTools, QuestionCustody, Resources, Selection, SessionSetup,
};

mod owner;
mod subagent;

pub use owner::ApplicationOwner;

/// Retained recipe used for initial assembly, reload and idle replacement.
/// Implementations select the complete model/tool environment, not additions to
/// coding defaults. A missing model requests the application's own default.
pub trait Application: Send + Sync {
    fn resources(&self, cwd: Option<&Path>) -> Result<Resources>;
    /// Explicit user-command metadata. Custom applications inherit no
    /// executable catalog; this is separate from model tools and instructions.
    fn commands(&self, _cwd: Option<&Path>) -> Result<CommandCatalog> {
        Ok(CommandCatalog::default())
    }
    fn runtime(
        &self,
        cwd: Option<&Path>,
        model: Option<&ModelRef>,
        questions: Option<Arc<QuestionCustody>>,
    ) -> Result<ApplicationRuntime>;
    /// Stable application-lifetime custody, independent of any Session's
    /// shutdown or binding. Return the same retained owner on every call;
    /// the outer application explicitly closes/joins it before its environment.
    fn owner(&self) -> Option<Arc<ApplicationOwner>> {
        None
    }
}

/// Explicit authority for direct user shell commands, separate from model tools.
pub trait UserShell: Send + Sync {
    /// Preflight before durable admission/recovery. The returned callback is
    /// cold, owns its execution route and receives Core's retained stop token.
    fn prepare(&self, command: &str) -> Result<PreparedShell>;
}

pub type PreparedShell =
    Box<dyn FnOnce(CancellationToken) -> BoxFuture<'static, CodingToolOutput> + Send>;

#[derive(Clone)]
pub struct ApplicationRuntime {
    pub agent: Arc<CodingAgent>,
    pub shell: Option<Arc<dyn UserShell>>,
    /// Allow trusted client input to read local image paths. This is not an OS
    /// sandbox or a path allowlist. Encoded/normalized images need no file access.
    pub local_images: bool,
    /// Process supervision captured by this runtime, not a tool grant. Managed
    /// delegation joins/releases this child's scope before returning its result.
    pub processes: Option<Arc<crate::ProcessOwner>>,
}

impl ApplicationRuntime {
    /// No native input or shell authority is inherited by a custom application.
    pub fn new(agent: Arc<CodingAgent>) -> Self {
        Self {
            agent,
            shell: None,
            local_images: false,
            processes: None,
        }
    }
}

#[derive(Clone)]
enum SelectionPolicy {
    Explicit(ToolSelection),
    ReadOnly,
}

#[derive(Clone)]
enum CodingRole {
    Main(Arc<ApplicationOwner>),
    Subagent,
}

/// The local coding application: native tools, managed delegation, discovered
/// resources and optional additional tools. Shared control owns none of these defaults.
#[derive(Clone)]
pub struct CodingApplication {
    host: Arc<Host>,
    external_tools: Option<Arc<dyn CodingToolSource>>,
    code_mode: bool,
    selection: SelectionPolicy,
    role: CodingRole,
    processes: Arc<crate::ProcessOwner>,
    command_environment: Option<Arc<crate::CommandEnvironment>>,
    search_settings: Option<Arc<crate::SearchSettings>>,
}

const READ_ONLY_TOOLS: &[&str] = &[
    "read",
    "ls",
    "find",
    "grep",
    "web_fetch",
    "web_search",
    "history",
    "list_mcp_resources",
    "list_mcp_resource_templates",
    "read_mcp_resource",
    "code_mode",
];

impl CodingApplication {
    pub fn new(host: Arc<Host>, external_tools: Option<Arc<dyn CodingToolSource>>) -> Self {
        let processes = Arc::new(crate::ProcessOwner::new(64).expect("supported process capacity"));
        let owner = Arc::new(ApplicationOwner::new(
            Arc::new(crate::BackgroundOwner::new(64).expect("supported custody capacity")),
            processes.clone(),
        ));
        Self {
            host,
            external_tools,
            code_mode: false,
            selection: SelectionPolicy::Explicit(ToolSelection::all()),
            role: CodingRole::Main(owner),
            processes,
            command_environment: None,
            search_settings: None,
        }
    }

    /// Supply the complete command environment for a service/library binding.
    /// This skips the user settings file and ambient command-environment
    /// discovery. Search uses built-in routing unless `with_search_settings`
    /// supplies explicit policy; native search still uses the selected profile.
    pub fn with_command_environment(mut self, environment: Arc<crate::CommandEnvironment>) -> Self {
        self.command_environment = Some(environment);
        self
    }

    /// Supply complete search routing policy rather than reading user settings.
    pub fn with_search_settings(mut self, settings: Arc<crate::SearchSettings>) -> Self {
        self.search_settings = Some(settings);
        self
    }

    /// Enable composition for this application, including later replacements.
    pub fn with_code_mode(mut self, enabled: bool) -> Self {
        self.code_mode = enabled;
        self
    }

    /// Narrow model-visible tools without changing the application's authority.
    /// This is not an OS sandbox; excluded tools are not reachable by model calls
    /// or Code Mode, but user shell commands and host-side application actions
    /// remain separate.
    pub fn with_tool_selection(mut self, selection: ToolSelection) -> Self {
        self.selection = SelectionPolicy::Explicit(selection);
        self
    }

    /// Select the read-only coding tools. This narrows model-visible tools and
    /// excludes managed children; explicit user shell and user commands remain
    /// separate authority. It is a model-loadout policy, not an OS or credential
    /// boundary.
    pub fn with_read_only_tools(mut self) -> Self {
        self.selection = SelectionPolicy::ReadOnly;
        self
    }

    /// Preserve the executable's already-resolved initial route (including
    /// explicit overrides); subsequent selections use this application's store.
    /// Supply the resource snapshot from `Application::resources` so launch
    /// preflight and binding use the same discovered bytes.
    pub fn setup(
        &self,
        session: Arc<CodingSession>,
        selected: Selection,
        reasoning: Option<ion_ai::Reasoning>,
        questions: Option<QuestionCustody>,
        resources: Resources,
    ) -> Result<SessionSetup> {
        let questions = questions.map(Arc::new);
        let cwd = session
            .cwd()
            .context("coding requires a working directory")?;
        let catalog = self.host.sessions(Some(cwd.to_path_buf()));
        let runtime = self.runtime_with_selection(cwd, &selected, questions.clone())?;
        SessionSetup::from_prepared(
            session,
            catalog,
            Arc::new(self.clone()),
            resources,
            runtime,
            reasoning,
            questions,
        )
    }

    fn runtime_with_selection(
        &self,
        cwd: &Path,
        selected: &Selection,
        questions: Option<Arc<QuestionCustody>>,
    ) -> Result<ApplicationRuntime> {
        let settings = if self.command_environment.is_none() {
            crate::user_settings::UserSettings::load(self.host.config_root())?
        } else {
            crate::user_settings::UserSettings::default()
        };
        let environment = match &self.command_environment {
            Some(environment) => environment.clone(),
            None => Arc::new(crate::CommandEnvironment::coding_with_settings(
                settings.shell()?,
            )?),
        };
        let search_settings = match &self.search_settings {
            Some(settings) => settings.clone(),
            None => Arc::new(settings.search()?),
        };
        let native = Arc::new(
            LocalTools::with_environment(cwd, environment.clone())?.with_project_instructions(),
        );
        let mut sources: Vec<Arc<dyn CodingToolSource>> = vec![
            native.clone(),
            Arc::new(crate::WebTools::new()?),
            Arc::new(crate::SearchTools::coding(
                selected,
                self.host.credentials(),
                &search_settings,
            )?),
            Arc::new(crate::ProcessTools::with_environment(
                cwd,
                self.processes.clone(),
                environment.clone(),
            )?),
        ];
        if let CodingRole::Main(owner) = &self.role {
            sources.push(Arc::new(subagent::Subagent::new(
                self,
                cwd,
                selected,
                owner.background().clone(),
                owner.completions().clone(),
                environment,
                search_settings,
            )));
        }
        let ask_available = questions.is_some();
        if let Some(questions) = questions {
            sources.push(Arc::new(crate::QuestionTools::new(questions)));
        }
        sources.extend(self.external_tools.iter().cloned());
        let selection = match &self.selection {
            SelectionPolicy::Explicit(selection) => selection.clone(),
            SelectionPolicy::ReadOnly => {
                let mut names = READ_ONLY_TOOLS
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<Vec<_>>();
                if ask_available {
                    names.push("ask_user_question".into());
                }
                ToolSelection::include(names)
            }
        };
        let tools = ion_core::ToolSet::new(sources)
            .with_selection(selection)
            .with_history();
        let tools = if self.code_mode {
            tools.with_code_mode(
                Arc::new(crate::code_mode::QuickJs),
                ion_core::CodeLimits::default(),
            )
        } else {
            tools
        };
        let agent = self.host.agent(selected, Arc::new(tools))?;
        Ok(ApplicationRuntime {
            agent,
            shell: Some(native),
            local_images: true,
            processes: Some(self.processes.clone()),
        })
    }
}

impl Application for CodingApplication {
    fn owner(&self) -> Option<Arc<ApplicationOwner>> {
        match &self.role {
            CodingRole::Main(owner) => Some(owner.clone()),
            CodingRole::Subagent => None,
        }
    }

    fn resources(&self, cwd: Option<&Path>) -> Result<Resources> {
        self.host
            .resources(cwd.context("coding requires a working directory")?)
    }

    fn commands(&self, cwd: Option<&Path>) -> Result<CommandCatalog> {
        // Managed children and custom subagent recipes inherit no implicit
        // executable authority. A main coding Session, including one with a
        // read-only model loadout, may use the user's explicit commands.
        if !matches!(self.role, CodingRole::Main(_)) {
            return Ok(CommandCatalog::default());
        }
        CommandCatalog::load(
            cwd.context("coding requires a working directory")?,
            self.host.config_root(),
        )
    }

    fn runtime(
        &self,
        cwd: Option<&Path>,
        model: Option<&ModelRef>,
        questions: Option<Arc<QuestionCustody>>,
    ) -> Result<ApplicationRuntime> {
        let selected = self
            .host
            .models()
            .choose(None, model.cloned(), self.host.credentials())?;
        self.runtime_with_selection(
            cwd.context("coding requires a working directory")?,
            &selected,
            questions,
        )
    }
}
