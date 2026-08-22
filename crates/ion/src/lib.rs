//! Ion frontends and host composition. The binary is a thin process
//! shell over this library; integration tests drive the same surface.

pub mod acp;
pub mod openrouter;
pub mod print;
pub mod settings;
pub mod tui;

use std::future::Future;
use std::sync::Arc;

use futures_util::future::Either;

use ion_core::{EngineSignal, Provider, ProviderRequest, ScriptedProvider};
use openrouter::OpenRouterProvider;

pub use acp::{AcpConfig, serve as acp_serve};
pub use settings::Settings;

/// The host's provider choice for one invocation. `Provider` is not
/// dyn-compatible by design; the host composes concretely.
pub enum CliProvider {
    Scripted(ScriptedProvider),
    OpenRouter(OpenRouterProvider),
}

impl Provider for CliProvider {
    fn run(
        &self,
        request: ProviderRequest,
        cancel: tokio_util::sync::CancellationToken,
        out: tokio::sync::mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        // The arms return distinct concrete futures; erase them for the
        // host-level dispatch.
        Box::pin(match self {
            CliProvider::Scripted(provider) => {
                let fut = provider.run(request, cancel, out);
                Either::Left(fut)
            }
            CliProvider::OpenRouter(provider) => {
                let fut = provider.run(request, cancel, out);
                Either::Right(fut)
            }
        })
    }

    async fn context_window(&self) -> Option<u64> {
        match self {
            CliProvider::Scripted(_) => None,
            CliProvider::OpenRouter(provider) => provider.context_window().await,
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
) where
    P: ion_core::Provider,
{
    tools.register_scope(
        "delegate",
        vec![Arc::new(ion_core::DelegateTool::new(
            ion_core::DelegateConfig {
                store: store.clone(),
                make_provider,
                max_active_children: 4,
                child_budget: ion_core::child_budget_default(),
            },
            parent_id,
        ))],
    );
}

/// Build the scripted-provider factory used when no real model is
/// configured. Shared by the CLI frontends and tests.
pub fn scripted_provider_factory(
    script: Vec<ion_core::ScriptedMessage>,
) -> Arc<dyn Fn() -> CliProvider + Send + Sync> {
    Arc::new(move || CliProvider::Scripted(ScriptedProvider::new(script.clone())))
}
