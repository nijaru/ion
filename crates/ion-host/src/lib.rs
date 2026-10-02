//! Shared local host setup for Ion clients and Rust embedders.
//! Session/Turn semantics live in `ion-core`; concrete provider/tool implementations live here.

pub mod auth;
mod binding;
pub mod catalog;
mod credentials;
pub mod image_input;
pub mod mcp;
mod model_http;
pub mod model_setup;
pub mod project_instructions;
pub mod resources;
pub mod session_catalog;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use ion_core::{AgentLimits, CodingAgent, CodingToolHost, LocalTools, ToolSet};

pub use auth::{CredentialStatus, CredentialStore};
pub use binding::SessionBinding;
pub use credentials::{CredentialResolutionError, CredentialResolver};
pub use mcp::{McpConfig, McpHttpServer, McpServer, McpStartup, McpStdioServer, McpTools};
pub use model_http::{HttpModelService, HttpWire};
pub use model_setup::{ModelChoice, ModelStore, SavedSelection, Selection, Wire};
pub use resources::{PromptTemplate, ResourceDiagnostic, Resources, Skill};
pub use session_catalog::{SessionCatalog, SessionSummary};

/// Host configuration can be supplied explicitly by an embedding program or
/// resolved from standard local directories for the executable.
pub struct Host {
    config_root: PathBuf,
    state_root: PathBuf,
    credentials: CredentialStore,
    models: ModelStore,
}

impl Host {
    pub fn new(config_root: PathBuf, state_root: PathBuf) -> Self {
        let credentials = CredentialStore::new(config_root.join("credentials"));
        let models = ModelStore::new(config_root.clone());
        Self {
            config_root,
            state_root,
            credentials,
            models,
        }
    }

    pub fn from_environment() -> Result<Self> {
        Ok(Self::new(
            app_root("XDG_CONFIG_HOME", ".config")?,
            app_root("XDG_STATE_HOME", ".local/state")?,
        ))
    }

    pub fn config_root(&self) -> &Path {
        &self.config_root
    }
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }
    pub fn credentials(&self) -> &CredentialStore {
        &self.credentials
    }
    pub fn models(&self) -> &ModelStore {
        &self.models
    }

    pub fn sessions(&self, cwd: PathBuf) -> SessionCatalog {
        SessionCatalog::new(self.state_root.join("sessions"), cwd)
    }

    pub fn instructions(&self, cwd: &Path) -> Result<String> {
        Ok(self.resources(cwd)?.instructions().to_owned())
    }

    pub fn resources(&self, cwd: &Path) -> Result<Resources> {
        Resources::load(cwd, &self.config_root)
    }

    pub fn mcp_config(&self) -> McpConfig {
        McpConfig::new(&self.config_root)
    }

    pub async fn external_tools(&self, cwd: &Path) -> McpStartup {
        McpTools::connect(&self.mcp_config(), cwd).await
    }

    pub fn agent(&self, cwd: &Path, selected: &Selection) -> Result<Arc<CodingAgent>> {
        let tools = Arc::new(LocalTools::new(cwd)?);
        self.agent_with_tool_host(selected, tools)
    }

    pub(crate) fn agent_with_optional_tools(
        &self,
        cwd: &Path,
        selected: &Selection,
        custom: Option<Arc<dyn CodingToolHost>>,
    ) -> Result<Arc<CodingAgent>> {
        match custom {
            Some(custom) => self.agent_with_tools(cwd, selected, custom),
            None => self.agent(cwd, selected),
        }
    }

    /// Add custom tools to Ion's built-ins. A custom tool with a built-in name
    /// deliberately replaces that tool while all other built-ins remain.
    pub fn agent_with_tools(
        &self,
        cwd: &Path,
        selected: &Selection,
        custom: Arc<dyn CodingToolHost>,
    ) -> Result<Arc<CodingAgent>> {
        let builtins: Arc<dyn CodingToolHost> = Arc::new(LocalTools::new(cwd)?);
        self.agent_with_tool_set(selected, Arc::new(ToolSet::new([builtins, custom])))
    }

    /// Compose a selected route with a complete caller-owned tool host.
    pub fn agent_with_tool_host(
        &self,
        selected: &Selection,
        tools: Arc<dyn CodingToolHost>,
    ) -> Result<Arc<CodingAgent>> {
        self.agent_with_tool_set(selected, Arc::new(ToolSet::new([tools])))
    }

    fn agent_with_tool_set(
        &self,
        selected: &Selection,
        tools: Arc<ToolSet>,
    ) -> Result<Arc<CodingAgent>> {
        selected.require_access(&self.credentials)?;
        let resolver = self
            .credentials
            .resolver(&selected.provider, selected.api_key_env.as_deref())?;
        let service = Arc::new(HttpModelService::new(
            &selected.endpoint,
            selected.wire,
            resolver,
        )?);
        Ok(Arc::new(
            CodingAgent::with_tool_set(service, tools).with_limits(AgentLimits {
                max_output_tokens: selected.max_output_tokens,
                context_window_tokens: selected.context_window_tokens,
                image_input: selected.image_input,
                ..AgentLimits::default()
            }),
        ))
    }
}

fn app_root(variable: &str, fallback: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(variable) {
        ensure!(!path.is_empty(), "{variable} must not be empty");
        return Ok(PathBuf::from(path).join("ion"));
    }
    let home = std::env::var_os("HOME").context("HOME is required for Ion paths")?;
    Ok(PathBuf::from(home).join(fallback).join("ion"))
}
