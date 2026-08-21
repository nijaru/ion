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
}

/// Build the scripted-provider factory used when no real model is
/// configured. Shared by the CLI frontends and tests.
pub fn scripted_provider_factory(
    script: Vec<ion_core::ScriptedMessage>,
) -> Arc<dyn Fn() -> CliProvider + Send + Sync> {
    Arc::new(move || CliProvider::Scripted(ScriptedProvider::new(script.clone())))
}
