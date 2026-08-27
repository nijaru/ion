//! Ion frontends and host composition. The binary is a thin process
//! shell over this library; integration tests drive the same surface.

pub mod acp;
pub mod openai_codex;
pub mod openrouter;
pub mod print;
pub mod settings;
pub mod tui;

use std::future::Future;
use std::sync::Arc;

use ion_core::{EngineSignal, ModelCapabilities, Provider, ProviderRequest, ScriptedProvider};
use openai_codex::OpenAICodexProvider;
use openrouter::OpenRouterProvider;

pub use acp::AcpConfig;
pub use settings::Settings;

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
            },
        }
    }
}

/// Register the bounded-child delegation surface (§20) on a started
/// runtime: the delegate tool needs the parent session id, which only
/// exists once the runtime is composed. Call before the first submit.
pub fn enable_children<P>(
    tools: &ion_core::ToolCatalog,
    store: &ion_core::SessionStore,
    make_provider: Arc<dyn Fn() -> P + Send + Sync>,
    parent_id: ion_core::SessionId,
    trusted_resources: Vec<ion_core::TrustedResource>,
) where
    P: ion_core::Provider,
{
    tools.register_scope(
        "delegate",
        vec![Arc::new(ion_core::DelegateTool::new(
            ion_core::DelegateConfig {
                store: store.clone(),
                make_provider,
                make_provider_for_model: None,
                max_active_children: 4,
                child_budget: ion_core::child_budget_default(),
                trusted_resources,
                cwd: tools.cwd().to_path_buf(),
            },
            parent_id,
        ))],
    );
}

/// Build the scripted-provider factory used when no real model is
/// configured; the frontends build their scripted providers inline and
/// integration tests share this factory.
pub fn scripted_provider_factory(
    script: Vec<ion_core::ScriptedMessage>,
) -> Arc<dyn Fn() -> CliProvider + Send + Sync> {
    Arc::new(move || CliProvider::Scripted(ScriptedProvider::new(script.clone())))
}
