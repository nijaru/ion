//! Ion CLI host. This binary owns process lifetime and frontend selection.

use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use futures_util::future::Either;
use ion_core::{
    Runtime, RuntimeError, ScriptedMessage, ScriptedProvider, SessionStore, ToolRegistry,
    default_db_path,
};
use std::future::Future;

mod openrouter;
mod print;
mod tui;

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
    /// Reopen the most recent persisted session in the interactive
    /// TUI instead of starting a new one.
    #[arg(long = "resume")]
    resume: bool,
    /// Tools this non-interactive run may execute without approval,
    /// comma-separated (e.g. --allow bash,write). Everything else
    /// terminates the operation with ApprovalRequired (DESIGN.md §17).
    #[arg(long = "allow", value_name = "TOOLS", value_delimiter = ',')]
    allow: Vec<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();
    if cli.print.is_none() {
        return run_tui(&cli).await;
    }
    let prompt = cli.print.expect("checked above");

    match run_print(prompt, cli.model, cli.allow).await {
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

async fn run_tui(cli: &Cli) -> ExitCode {
    let api_key = match cli.model.as_deref() {
        Some(_) => match std::env::var("OPENROUTER_API_KEY") {
            Ok(key) => Some(key),
            Err(_) => {
                let _ = writeln!(io::stderr(), "--model requires OPENROUTER_API_KEY");
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let provider = match cli.model.clone() {
        Some(model) => {
            CliProvider::OpenRouter(OpenRouterProvider::new(model, api_key.expect("checked")))
        }
        None => CliProvider::Scripted(ScriptedProvider::new(vec![ScriptedMessage::text(
            "scripted provider: build with --model for real answers\n",
        )])),
    };
    let store = match SessionStore::open(default_db_path()) {
        Ok(store) => Arc::new(store),
        Err(err) => {
            let _ = writeln!(io::stderr(), "store: {err}");
            return ExitCode::FAILURE;
        }
    };
    let resume_session = if cli.resume {
        match store.latest_session().await {
            Ok(Some(id)) => Some(id),
            Ok(None) => {
                let _ = writeln!(io::stderr(), "no persisted session to resume");
                return ExitCode::from(2);
            }
            Err(err) => {
                let _ = writeln!(io::stderr(), "store: {err}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let tools = ToolRegistry::default();
    let policy: Arc<dyn ion_core::PolicyEngine> = if cli.allow.is_empty() {
        Arc::new(ion_core::DefaultPolicy)
    } else {
        Arc::new(ion_core::AllowlistPolicy::new(cli.allow.clone()))
    };
    let runtime = if let Some(session_id) = resume_session {
        match Runtime::open_session(provider, tools, (*store).clone(), session_id).await {
            Ok(runtime) => runtime,
            Err(err) => {
                let _ = writeln!(io::stderr(), "resume: {err}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        Runtime::start_with_policy(provider, tools, (*store).clone(), policy)
    };
    match tui::run(runtime.session(), store, resume_session).await {
        Ok(()) => {
            let _ = runtime.join().await;
            ExitCode::SUCCESS
        }
        Err(err) => {
            let _ = runtime.join().await;
            let _ = writeln!(io::stderr(), "{err}");
            ExitCode::FAILURE
        }
    }
}

async fn run_print(
    prompt: String,
    model: Option<String>,
    allow: Vec<String>,
) -> Result<(), RuntimeError> {
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
    let store = SessionStore::open(default_db_path())
        .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
    let policy: Arc<dyn ion_core::PolicyEngine> = if allow.is_empty() {
        Arc::new(ion_core::DefaultPolicy)
    } else {
        Arc::new(ion_core::AllowlistPolicy::new(allow))
    };
    let runtime = Runtime::start_with_policy(provider, tools, store, policy);
    let session = runtime.session();
    let result = PrintFrontend::new(io::stdout()).run(&session, prompt).await;
    let shutdown = session.close().await;
    let join = runtime.join().await;
    result?;
    shutdown?;
    join
}
