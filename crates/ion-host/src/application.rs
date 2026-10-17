//! Application-owned assembly. Shared control never adds capabilities here.
use std::{path::Path, sync::Arc};

use anyhow::{Context, Result};
use ion_ai::{BoxFuture, ModelRef};
use ion_core::{CodingAgent, CodingSession, CodingToolOutput, CodingToolSource, ToolSelection};
use tokio_util::sync::CancellationToken;

use crate::{Host, LocalTools, Resources, Selection, SessionSetup};

mod subagent;

/// Retained recipe used for initial assembly, reload and idle replacement.
/// Implementations select the complete model/tool environment, not additions to
/// coding defaults. A missing model requests the application's own default.
pub trait Application: Send + Sync {
    fn resources(&self, cwd: Option<&Path>) -> Result<Resources>;
    fn runtime(&self, cwd: Option<&Path>, model: Option<&ModelRef>) -> Result<ApplicationRuntime>;
    /// Optional application-lifetime custody, independent of any Session's
    /// shutdown or binding. The outer application explicitly closes/joins it.
    fn background(&self) -> Option<Arc<crate::BackgroundOwner>> {
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
}

impl ApplicationRuntime {
    /// No native input or shell authority is inherited by a custom application.
    pub fn new(agent: Arc<CodingAgent>) -> Self {
        Self {
            agent,
            shell: None,
            local_images: false,
        }
    }
}

#[derive(Clone)]
enum CodingRole {
    Main(Arc<crate::BackgroundOwner>),
    Subagent,
}

/// The local coding application: native tools, managed delegation, discovered
/// resources and optional additional tools. Shared control owns none of these defaults.
#[derive(Clone)]
pub struct CodingApplication {
    host: Arc<Host>,
    external_tools: Option<Arc<dyn CodingToolSource>>,
    code_mode: bool,
    selection: ToolSelection,
    role: CodingRole,
}

const READ_ONLY_TOOLS: &[&str] = &[
    "read",
    "list_mcp_resources",
    "list_mcp_resource_templates",
    "read_mcp_resource",
    "code_mode",
];

impl CodingApplication {
    pub fn new(host: Arc<Host>, external_tools: Option<Arc<dyn CodingToolSource>>) -> Self {
        Self {
            host,
            external_tools,
            code_mode: false,
            selection: ToolSelection::all(),
            role: CodingRole::Main(Arc::new(
                crate::BackgroundOwner::new(64).expect("supported custody capacity"),
            )),
        }
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
        self.selection = selection;
        self
    }

    /// Select the read-only coding tools. This excludes native mutations, direct
    /// shell execution and managed children; it is a model-loadout policy, not
    /// an OS or credential boundary.
    pub fn with_read_only_tools(mut self) -> Self {
        self.selection = ToolSelection::include(READ_ONLY_TOOLS.iter().map(|name| (*name).into()));
        self
    }

    /// Preserve the executable's already-resolved initial route (including
    /// explicit overrides); subsequent selections use this application's store.
    pub fn setup(
        &self,
        session: Arc<CodingSession>,
        selected: Selection,
        reasoning: Option<ion_ai::Reasoning>,
    ) -> Result<SessionSetup> {
        let cwd = session
            .cwd()
            .context("coding requires a working directory")?;
        let catalog = self.host.sessions(cwd.to_path_buf());
        let resources = self.resources(Some(cwd))?;
        let runtime = self.runtime_with_selection(cwd, &selected)?;
        SessionSetup::from_prepared(
            session,
            catalog,
            Arc::new(self.clone()),
            resources,
            runtime,
            reasoning,
        )
    }

    fn runtime_with_selection(
        &self,
        cwd: &Path,
        selected: &Selection,
    ) -> Result<ApplicationRuntime> {
        let native = Arc::new(LocalTools::new(cwd)?);
        let mut sources: Vec<Arc<dyn CodingToolSource>> = vec![native.clone()];
        if let CodingRole::Main(owner) = &self.role {
            sources.push(Arc::new(subagent::Subagent::new(
                self,
                cwd,
                selected,
                owner.clone(),
            )));
        }
        sources.extend(self.external_tools.iter().cloned());
        let tools = ion_core::ToolSet::new(sources).with_selection(self.selection.clone());
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
        })
    }
}

impl Application for CodingApplication {
    fn background(&self) -> Option<Arc<crate::BackgroundOwner>> {
        match &self.role {
            CodingRole::Main(owner) => Some(owner.clone()),
            CodingRole::Subagent => None,
        }
    }

    fn resources(&self, cwd: Option<&Path>) -> Result<Resources> {
        self.host
            .resources(cwd.context("coding requires a working directory")?)
    }

    fn runtime(&self, cwd: Option<&Path>, model: Option<&ModelRef>) -> Result<ApplicationRuntime> {
        let selected =
            self.host
                .models()
                .choose(None, None, model.cloned(), self.host.credentials())?;
        self.runtime_with_selection(
            cwd.context("coding requires a working directory")?,
            &selected,
        )
    }
}
