//! Shared local host setup for Ion clients and Rust embedders.
//! Session history and model/tool execution remain owned by `ion-core`.

pub mod auth;
pub mod catalog;
pub mod model_setup;
pub mod project_instructions;
pub mod session_catalog;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use ion_core::{
    AgentLimits, CodingAgent, CodingSession, CodingToolHost, HttpModelService, LocalTools,
};

pub use auth::{CredentialStatus, CredentialStore};
pub use model_setup::{ModelChoice, ModelStore, SavedSelection, Selection, Wire};
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
        project_instructions::load(cwd)
    }

    pub fn agent(&self, session: &CodingSession, selected: &Selection) -> Result<Arc<CodingAgent>> {
        let tools = Arc::new(LocalTools::new(session.cwd())?);
        self.agent_with_tools(selected, tools)
    }

    /// Compose a selected route with host-supplied tools. This is the same
    /// provider setup used by the executable, without prescribing its tools.
    pub fn agent_with_tools(
        &self,
        selected: &Selection,
        tools: Arc<dyn CodingToolHost>,
    ) -> Result<Arc<CodingAgent>> {
        selected.require_access(&self.credentials)?;
        let resolver = self
            .credentials
            .resolver(&selected.provider, &selected.api_key_env)?;
        let service = Arc::new(HttpModelService::new(
            &selected.endpoint,
            selected.wire,
            resolver,
        )?);
        Ok(Arc::new(CodingAgent::new(service, tools).with_limits(
            AgentLimits {
                max_output_tokens: selected.max_output_tokens,
                context_window_tokens: selected.context_window_tokens,
                ..AgentLimits::default()
            },
        )))
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
