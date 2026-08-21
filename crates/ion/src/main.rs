//! Ion CLI host. This binary owns process lifetime and frontend selection.

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;
use futures_util::future::Either;
use ion_core::{
    Runtime, RuntimeError, ScriptedMessage, ScriptedProvider, SessionStore, ToolRegistry,
    default_db_path,
};
use std::future::Future;

mod openrouter;
mod print;

use openrouter::OpenRouterProvider;
use print::PrintFrontend;

#[derive(Parser, Debug)]
#[command(
    name = "ion",
    version,
    about = "Ion terminal coding agent",
    disable_help_subcommand = true
)]
struct Cli {
    /// Run one prompt through print mode and exit.
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    print: Option<String>,
    /// Run against a real OpenRouter model (e.g. stealth/ox-alpha)
    /// instead of the scripted provider. Requires OPENROUTER_API_KEY.
    #[arg(long = "model", value_name = "MODEL")]
    model: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();
    let Some(prompt) = cli.print else {
        let _ = writeln!(
            io::stderr(),
            "interactive TUI is not implemented yet; use ion -p \"prompt\""
        );
        return ExitCode::from(2);
    };

    match run_print(prompt, cli.model).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            ExitCode::FAILURE
        }
    }
}

/// The host's provider choice for one invocation. `Provider` is not
/// dyn-compatible by design; the host composes concretely.
enum CliProvider {
    Scripted(ScriptedProvider),
    OpenRouter(OpenRouterProvider),
}

impl ion_core::Provider for CliProvider {
    fn run(
        &self,
        request: ion_core::ProviderRequest,
        cancel: tokio_util::sync::CancellationToken,
        out: tokio::sync::mpsc::Sender<ion_core::EngineSignal>,
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

async fn run_print(prompt: String, model: Option<String>) -> Result<(), RuntimeError> {
    let provider = match model {
        Some(model) => {
            let api_key = std::env::var("OPENROUTER_API_KEY").map_err(|_| {
                RuntimeError::OperationFailed(
                    "--model requires OPENROUTER_API_KEY to be set".to_owned(),
                )
            })?;
            CliProvider::OpenRouter(OpenRouterProvider::new(model, api_key))
        }
        None => CliProvider::Scripted(ScriptedProvider::new(vec![ScriptedMessage::text(format!(
            "scripted: {prompt}\n"
        ))])),
    };
    let tools = ToolRegistry::default();
    let runtime = Runtime::start_with_store(
        provider,
        tools,
        SessionStore::open(default_db_path())
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?,
    );
    let session = runtime.session();
    let result = PrintFrontend::new(io::stdout()).run(&session, prompt).await;
    let shutdown = session.close().await;
    let join = runtime.join().await;
    result?;
    shutdown?;
    join
}
