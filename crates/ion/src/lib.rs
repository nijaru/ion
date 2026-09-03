//! Ion frontends and host composition. The binary is a thin process
//! shell over this library; integration tests drive the same surface.

pub mod acp;
pub mod openai_codex;
pub mod openrouter;
pub mod print;
pub mod session_manager;
pub mod settings;
pub mod tui;

use std::future::Future;
use std::sync::Arc;

use ion_core::{EngineSignal, ModelCapabilities, Provider, ProviderRequest, ScriptedProvider};
use openai_codex::OpenAICodexProvider;
use openrouter::OpenRouterProvider;

pub use acp::AcpConfig;
pub use settings::Settings;

/// Process-owned agent service for one root runtime. Same-session lane agents
/// and separately hosted fresh/fork descendants share one host lifetime. The latter
/// retain only process-local provider/runtime residency outside `Family`.
pub struct AgentHost<P> {
    family: Arc<ion_core::AgentFamily>,
    hosted: Arc<ion_core::HostedAgentRuntimes<P>>,
}

/// Launch-time model-facing agent-control policy and family capacity.
#[derive(Debug, Clone, Copy)]
pub struct AgentHostOptions {
    /// Maximum number of active family agents.
    pub max_active_agents: usize,
    /// Publish and durably admit the model-facing `agents` scope.
    pub agents_enabled: bool,
}

impl<P> AgentHost<P> {
    #[must_use]
    pub fn family(&self) -> &Arc<ion_core::AgentFamily> {
        &self.family
    }

    pub async fn close(&self) -> Result<(), String> {
        self.hosted.close().await
    }
}

/// Attach the complete agent service using the launch-provider factory for
/// separately hosted descendants.
pub async fn enable_agent_host<P>(
    tools: &ion_core::ToolCatalog,
    runtime: &ion_core::Runtime,
    store: &ion_core::SessionStore,
    make_provider: Arc<dyn Fn() -> P + Send + Sync>,
    trusted_resources: Vec<ion_core::TrustedResource>,
    options: AgentHostOptions,
) -> Result<AgentHost<P>, ion_core::AgentError>
where
    P: ion_core::Provider + 'static,
{
    enable_agent_host_with_model_resolver(
        tools,
        runtime,
        store,
        make_provider,
        None,
        trusted_resources,
        options,
    )
    .await
}

/// Attach the complete agent service with an optional provider resolver for
/// explicit model overrides on separately hosted descendants.
pub async fn enable_agent_host_with_model_resolver<P>(
    tools: &ion_core::ToolCatalog,
    runtime: &ion_core::Runtime,
    store: &ion_core::SessionStore,
    make_provider: Arc<dyn Fn() -> P + Send + Sync>,
    make_provider_for_model: Option<Arc<dyn Fn(String) -> P + Send + Sync>>,
    trusted_resources: Vec<ion_core::TrustedResource>,
    options: AgentHostOptions,
) -> Result<AgentHost<P>, ion_core::AgentError>
where
    P: ion_core::Provider + 'static,
{
    let family = Arc::new(runtime.agent_family(options.max_active_agents).await?);
    let hosted = ion_core::hosted_agent_runtimes(
        ion_core::HostedAgentConfig {
            store: store.clone(),
            make_provider,
            make_provider_for_model,
            max_active: 4,
            budget: ion_core::hosted_agent_budget_default(),
            trusted_resources,
            cwd: tools.cwd().to_path_buf(),
        },
        runtime.session_id(),
    );
    ion_core::install_agent_host_tools(
        tools,
        runtime,
        Arc::clone(&family),
        Arc::clone(&hosted),
        options.agents_enabled,
    )
    .await?;
    Ok(AgentHost { family, hosted })
}

/// The host's provider choice for one invocation. `Provider` is not
/// dyn-compatible by design; the host composes concretely.
pub enum CliProvider {
    Scripted(ScriptedProvider),
    OpenAICodex(OpenAICodexProvider),
    OpenRouter(OpenRouterProvider),
    /// OpenAI-compatible local endpoint (currently the desktop route).
    Desktop(OpenRouterProvider),
    Unavailable(String),
}

impl Provider for CliProvider {
    fn initial_model_ref(&self) -> String {
        match self {
            CliProvider::OpenAICodex(provider) => provider.initial_model_ref(),
            CliProvider::OpenRouter(provider) | CliProvider::Desktop(provider) => {
                provider.initial_model_ref()
            }
            // Preserve the pre-existing identity for host/test placeholders that
            // do not own a configured external model.
            CliProvider::Scripted(_) | CliProvider::Unavailable(_) => {
                std::any::type_name::<Self>().to_owned()
            }
        }
    }

    fn run(
        &self,
        request: ProviderRequest,
        cancel: tokio_util::sync::CancellationToken,
        out: tokio::sync::mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        Box::pin(async move {
            match self {
                CliProvider::Scripted(provider) => provider.run(request, cancel, out).await,
                CliProvider::OpenAICodex(provider) => {
                    provider.run(request, cancel, out).await;
                }
                CliProvider::OpenRouter(provider) | CliProvider::Desktop(provider) => {
                    provider.run(request, cancel, out).await;
                }
                CliProvider::Unavailable(message) => {
                    let _ = out
                        .send(EngineSignal::Failed {
                            operation_id: request.operation_id,
                            step: request.step,
                            message: message.clone(),
                        })
                        .await;
                }
            }
        })
    }

    async fn context_window(&self) -> Option<u64> {
        match self {
            CliProvider::Scripted(_) => None,
            CliProvider::OpenAICodex(provider) => provider.context_window().await,
            CliProvider::OpenRouter(provider) | CliProvider::Desktop(provider) => {
                provider.context_window().await
            }
            CliProvider::Unavailable(_) => None,
        }
    }

    async fn capabilities(&self) -> ModelCapabilities {
        match self {
            CliProvider::Scripted(provider) => provider.capabilities().await,
            CliProvider::OpenAICodex(provider) => provider.capabilities().await,
            CliProvider::OpenRouter(provider) | CliProvider::Desktop(provider) => {
                provider.capabilities().await
            }
            CliProvider::Unavailable(_) => ModelCapabilities {
                reasoning: false,
                tool_calls: false,
                prompt_cache: false,
                streaming: false,
                images: false,
            },
        }
    }
}

/// Build the scripted-provider factory used when no real model is
/// configured; the frontends build their scripted providers inline and
/// integration tests share this factory.
pub fn scripted_provider_factory(
    script: Vec<ion_core::ScriptedMessage>,
) -> Arc<dyn Fn() -> CliProvider + Send + Sync> {
    Arc::new(move || CliProvider::Scripted(ScriptedProvider::new(script.clone())))
}

#[cfg(test)]
mod provider_identity_tests {
    use super::*;

    #[tokio::test]
    async fn disabled_agent_host_does_not_publish_model_facing_controls() {
        let store = ion_core::SessionStore::open_in_memory().expect("store");
        let tools = ion_core::ToolCatalog::with_cwd("/tmp");
        let runtime = ion_core::Runtime::start_with_policy(
            ScriptedProvider::echo(),
            tools.clone(),
            store.clone(),
            Arc::new(ion_core::DefaultPolicy),
        );
        let provider = Arc::new(ScriptedProvider::echo);
        let agent_host = enable_agent_host(
            &tools,
            &runtime,
            &store,
            provider,
            Vec::new(),
            AgentHostOptions {
                max_active_agents: 1,
                agents_enabled: false,
            },
        )
        .await
        .expect("agent host");

        assert!(!tools.specs().iter().any(|spec| spec.name == "spawn_agent"));

        agent_host.close().await.expect("close agent host");
        runtime.session().close().await.expect("close runtime");
        runtime.join().await.expect("join runtime");
        store.close().await.expect("close store");
    }

    #[tokio::test]
    async fn raw_cli_provider_seeds_runtime_with_configured_model_identity() {
        let store = ion_core::SessionStore::open_in_memory().expect("store");
        let runtime = ion_core::Runtime::start_with_store(
            CliProvider::OpenRouter(OpenRouterProvider::new("test/model", "key")),
            ion_core::ToolRegistry::default(),
            store,
        );
        let session = runtime.session();
        let snapshot = session.snapshot().await.expect("snapshot");
        assert_eq!(snapshot.model_ref, "test/model");
        assert!(
            CliProvider::OpenAICodex(OpenAICodexProvider::new("gpt-test", "token", "account",))
                .supports_model("gpt-test"),
            "fixed Codex adapters must expose their configured model identity",
        );
        session.close().await.expect("close");
        runtime.join().await.expect("join");
    }
}
