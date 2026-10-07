//! Shared local host setup for Ion clients and Rust embedders.
//! Session/Turn semantics live in `ion-core`; concrete provider/tool implementations live here.

pub mod auth;
mod binding;
pub mod catalog;
pub mod code_mode;
mod credentials;
mod edit_diff;
pub mod image_input;
mod local_tools;
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
use ion_core::{AgentLimits, CodingAgent, CodingToolSource, PromptCacheWarmingPolicy, ToolSet};

pub use auth::{CredentialStatus, CredentialStore};
pub use binding::SessionBinding;
pub use credentials::{CredentialResolutionError, CredentialResolver};
pub use local_tools::LocalTools;
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
    code_mode: bool,
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
            code_mode: false,
        }
    }

    pub fn from_environment() -> Result<Self> {
        Ok(Self::new(
            app_root("XDG_CONFIG_HOME", ".config")?,
            app_root("XDG_STATE_HOME", ".local/state")?,
        ))
    }

    /// Enable bounded JavaScript composition beside the direct tools for all
    /// agents assembled by this host, including later model/session switches.
    pub fn with_code_mode(mut self, enabled: bool) -> Self {
        self.code_mode = enabled;
        self
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
        self.agent_with_tool_source(selected, tools)
    }

    pub(crate) fn agent_with_optional_tools(
        &self,
        cwd: &Path,
        selected: &Selection,
        custom: Option<Arc<dyn CodingToolSource>>,
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
        custom: Arc<dyn CodingToolSource>,
    ) -> Result<Arc<CodingAgent>> {
        let builtins: Arc<dyn CodingToolSource> = Arc::new(LocalTools::new(cwd)?);
        self.agent_with_tool_set(selected, Arc::new(self.tool_set([builtins, custom])))
    }

    /// Compose a selected route with a complete caller-owned capability source.
    pub fn agent_with_tool_source(
        &self,
        selected: &Selection,
        tools: Arc<dyn CodingToolSource>,
    ) -> Result<Arc<CodingAgent>> {
        self.agent_with_tool_set(selected, Arc::new(self.tool_set([tools])))
    }

    fn tool_set(&self, sources: impl IntoIterator<Item = Arc<dyn CodingToolSource>>) -> ToolSet {
        let tools = ToolSet::new(sources);
        if self.code_mode {
            tools.with_code_mode(
                Arc::new(code_mode::QuickJs),
                ion_core::CodeLimits::default(),
            )
        } else {
            tools
        }
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
        let service = Arc::new(HttpModelService::new_with_capabilities(
            &selected.endpoint,
            selected.wire,
            resolver,
            selected.capabilities,
            selected.context_window_tokens,
        )?);
        Ok(Arc::new(
            CodingAgent::with_tool_set(service, tools, selected.identity()).with_limits(
                AgentLimits {
                    max_output_tokens: selected.max_output_tokens,
                    context_window_tokens: selected.context_window_tokens,
                    image_input: selected.image_input,
                    prompt_cache_warming: prompt_cache_warming(selected),
                    ..AgentLimits::default()
                },
            ),
        ))
    }
}

fn prompt_cache_warming(selected: &Selection) -> Option<PromptCacheWarmingPolicy> {
    let cache = selected.capabilities.prompt_cache;
    let catalog::PromptCacheLifetime::Fixed {
        default_seconds, ..
    } = cache.lifetime
    else {
        return None;
    };
    if cache.refresh != catalog::PromptCacheRefresh::ReplayOneToken {
        return None;
    }
    let pricing = cache.pricing?;
    Some(PromptCacheWarmingPolicy {
        lifetime_seconds: u64::from(default_seconds),
        cache_write_microusd_per_million: pricing.write_5m_microusd_per_million,
        cache_read_microusd_per_million: pricing.read_microusd_per_million,
        output_microusd_per_million: pricing.output_microusd_per_million,
        minimum_savings_microusd: 50_000,
    })
}

fn app_root(variable: &str, fallback: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(variable) {
        ensure!(!path.is_empty(), "{variable} must not be empty");
        return Ok(PathBuf::from(path).join("ion"));
    }
    let home = std::env::var_os("HOME").context("HOME is required for Ion paths")?;
    Ok(PathBuf::from(home).join(fallback).join("ion"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::ModelRef;
    use ion_core::{CodingSession, ToolRegistration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use tokio_util::sync::CancellationToken;

    struct NoTools;
    impl CodingToolSource for NoTools {
        fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn selected_agent_keeps_identity_transport_and_limits_together() {
        let root = std::env::temp_dir().join(format!("ion-bound-model-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let body_start = loop {
                assert!(socket.read_buf(&mut bytes).await.unwrap() > 0);
                assert!(bytes.len() < 64 * 1024);
                if let Some(at) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    break at + 4;
                }
            };
            let headers = std::str::from_utf8(&bytes[..body_start]).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            while bytes.len() < body_start + length {
                assert!(socket.read_buf(&mut bytes).await.unwrap() > 0);
            }
            let body: serde_json::Value =
                serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap();
            let reply = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
            body
        });
        let host = Host::new(root.join("config"), root.join("state"));
        let route = SavedSelection {
            provider: format!("binding-probe-{}", uuid::Uuid::now_v7()),
            model: "selected-model".into(),
            endpoint: Some(endpoint),
            wire: Some(Wire::ChatCompletions),
            api_key_env: None,
            image_input: false,
        };
        let mut selected = host.models().save_default(&route).unwrap();
        selected.max_output_tokens = 1234;
        selected.context_window_tokens = Some(50_000);
        let agent = host
            .agent_with_tool_source(&selected, Arc::new(NoTools))
            .unwrap();
        host.models()
            .save_default(&SavedSelection {
                model: "new-default".into(),
                ..route
            })
            .unwrap();
        let session = CodingSession::create(root.join("session.sqlite"), &root).unwrap();
        session
            .select_model(ModelRef {
                provider: "previous".into(),
                model: "previous-model".into(),
            })
            .unwrap();
        assert_eq!(agent.model(), &selected.identity());
        assert_eq!(agent.limits().max_output_tokens, 1234);
        assert_eq!(agent.limits().context_window_tokens, Some(50_000));
        assert_eq!(
            agent
                .submit(
                    &session,
                    "hello".into(),
                    String::new(),
                    CancellationToken::new(),
                    |_| {}
                )
                .await
                .unwrap(),
            "answer"
        );
        let body = server.await.unwrap();
        assert_eq!(body["model"], "selected-model");
        assert_eq!(body["max_completion_tokens"], 1234);
        let view = session.view().unwrap();
        assert_eq!(view.last_model, Some(selected.identity()));
        assert_eq!(view.last_effective_model, Some(selected.identity()));
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}
