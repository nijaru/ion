//! Application-owned assembly. Shared control never adds capabilities here.
use std::{path::Path, sync::Arc};

use anyhow::{Context, Result};
use ion_ai::{BoxFuture, ModelRef};
use ion_core::{CodingAgent, CodingSession, CodingToolOutput, CodingToolSource};
use tokio_util::sync::CancellationToken;

use crate::{Host, LocalTools, Resources, Selection, SessionSetup};

/// Retained recipe used for initial assembly, reload and idle replacement.
/// Implementations select the complete model/tool environment, not additions to
/// coding defaults. A missing model requests the application's own default.
pub trait Application: Send + Sync {
    fn resources(&self, cwd: Option<&Path>) -> Result<Resources>;
    fn runtime(&self, cwd: Option<&Path>, model: Option<&ModelRef>) -> Result<ApplicationRuntime>;
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

/// The local coding application: native tools, discovered resources and optional
/// additional tools. This recipe, not shared control, owns the coding defaults.
pub struct CodingApplication {
    host: Arc<Host>,
    external_tools: Option<Arc<dyn CodingToolSource>>,
    code_mode: bool,
}

impl CodingApplication {
    pub fn new(host: Arc<Host>, external_tools: Option<Arc<dyn CodingToolSource>>) -> Self {
        Self {
            host,
            external_tools,
            code_mode: false,
        }
    }

    /// Enable composition for this application, including later replacements.
    pub fn with_code_mode(mut self, enabled: bool) -> Self {
        self.code_mode = enabled;
        self
    }

    /// Preserve the executable's already-resolved initial route (including
    /// explicit overrides); subsequent selections use this application's store.
    pub fn setup(
        self,
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
            Arc::new(self),
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
        sources.extend(self.external_tools.iter().cloned());
        let tools = ion_core::ToolSet::new(sources);
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
