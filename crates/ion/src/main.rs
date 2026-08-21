//! Ion CLI host. This binary owns process lifetime and frontend selection.

use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use ion::openrouter::OpenRouterProvider;
use ion::print::PrintFrontend;
use ion::scripted_provider_factory;
use ion::settings::Settings;
use ion::tui;
use ion::{CliProvider, acp};
use ion_core::{
    Runtime, RuntimeError, ScriptedMessage, ScriptedProvider, SessionStore, default_db_path,
};

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
    /// Serve the Agent Client Protocol (v1) on stdio instead of
    /// running the TUI or print mode.
    #[arg(long = "acp")]
    acp: bool,
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
    let settings = match Settings::load() {
        Ok(settings) => settings,
        Err(err) => {
            let _ = writeln!(io::stderr(), "settings: {err}");
            return ExitCode::from(2);
        }
    };
    if cli.acp {
        return run_acp(&cli, &settings).await;
    }
    if cli.print.is_none() {
        return run_tui(&cli, &settings).await;
    }
    let prompt = cli.print.clone().expect("checked above");

    match run_print(prompt, &cli, &settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            ExitCode::FAILURE
        }
    }
}

async fn run_acp(cli: &Cli, settings: &Settings) -> ExitCode {
    let model = cli
        .model
        .clone()
        .or_else(|| settings.openrouter_model().ok().flatten());
    let api_key = match model.as_deref() {
        Some(_) => match std::env::var("OPENROUTER_API_KEY") {
            Ok(key) => Some(key),
            Err(_) => {
                let _ = writeln!(io::stderr(), "model requires OPENROUTER_API_KEY");
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let store = match SessionStore::open(default_db_path()) {
        Ok(store) => Arc::new(store),
        Err(err) => {
            let _ = writeln!(io::stderr(), "store: {err}");
            return ExitCode::FAILURE;
        }
    };
    let policy: Arc<dyn ion_core::PolicyEngine> = if cli.allow.is_empty() {
        Arc::new(ion_core::DefaultPolicy)
    } else {
        Arc::new(ion_core::AllowlistPolicy::new(cli.allow.clone()))
    };
    // ACP sessions may arrive concurrently; each gets its own provider
    // instance via the factory.
    let make_provider: Arc<dyn Fn() -> CliProvider + Send + Sync> = match model {
        Some(model) => {
            let key = api_key.expect("checked");
            Arc::new(move || {
                CliProvider::OpenRouter(OpenRouterProvider::new(model.clone(), key.clone()))
            })
        }
        None => scripted_provider_factory(vec![ScriptedMessage::text("scripted provider\n")]),
    };
    let config = acp::AcpConfig {
        make_provider,
        store,
        policy,
    };
    let _ = settings;
    match acp::serve(tokio::io::stdin(), tokio::io::stdout(), config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(io::stderr(), "acp: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Compose the tool surface: core tools plus every configured MCP
/// server's published tools. A failing server logs and is skipped -
/// one broken server never blocks startup (DESIGN.md §19.1).
async fn build_catalog(settings: &Settings) -> ion_core::ToolCatalog {
    let tools = ion_core::ToolCatalog::default();
    if !settings.mcp_servers.is_empty() {
        let defs: Vec<ion_core::ServerDef> = settings
            .mcp_servers
            .iter()
            .cloned()
            .map(Into::into)
            .collect();
        ion_core::McpService::new().start_into(&defs, &tools).await;
    }
    tools
}

async fn run_tui(cli: &Cli, settings: &Settings) -> ExitCode {
    let model = match resolve_model(cli.model.clone(), settings) {
        Ok(model) => model,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            return ExitCode::from(2);
        }
    };
    let api_key = match model.as_deref() {
        Some(_) => match std::env::var("OPENROUTER_API_KEY") {
            Ok(key) => Some(key),
            Err(_) => {
                let _ = writeln!(
                    io::stderr(),
                    "model requires OPENROUTER_API_KEY (set it, or clear \
                     defaultModel in {})",
                    Settings::path()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "~/.config/ion/settings.toml".to_owned())
                );
                return ExitCode::from(2);
            }
        },
        None => None,
    };
    let provider = match model {
        Some(model) => {
            CliProvider::OpenRouter(OpenRouterProvider::new(model, api_key.expect("checked")))
        }
        None => CliProvider::Scripted(ScriptedProvider::new(vec![ScriptedMessage::text(
            "scripted provider: build with --model for real answers\n",
        )])),
    };
    // Terminal first: the close-on-error path below suspends open
    // operations, so a terminal-less launch must fail before any
    // session state exists.
    let guard = match tui::setup_terminal() {
        Ok(guard) => guard,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            return ExitCode::from(2);
        }
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
    let tools = build_catalog(settings).await;
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
    let keymap = match tui::KeyMap::from_settings(&settings.keybindings) {
        Ok(keymap) => keymap,
        Err(err) => {
            let _ = writeln!(io::stderr(), "settings: {err}");
            return ExitCode::from(2);
        }
    };
    let session = runtime.session();
    let result = tui::run(
        session.clone(),
        store,
        resume_session,
        settings.theme(),
        keymap,
        guard,
    )
    .await;
    if result.is_err() {
        // The TUI died before its own close path; shut the actor down
        // or join would await a task waiting on this very handle.
        let _ = session.close().await;
    }
    match result {
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

/// `--model` wins; otherwise the settings default (pi-style: the
/// compiled-in defaults mirror the maintainer's pi settings).
fn resolve_model(cli_model: Option<String>, settings: &Settings) -> Result<Option<String>, String> {
    if cli_model.is_some() {
        return Ok(cli_model);
    }
    settings.openrouter_model()
}

async fn run_print(prompt: String, cli: &Cli, settings: &Settings) -> Result<(), RuntimeError> {
    let model =
        resolve_model(cli.model.clone(), settings).map_err(RuntimeError::OperationFailed)?;
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
    let tools = build_catalog(settings).await;
    let store = SessionStore::open(default_db_path())
        .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
    let policy: Arc<dyn ion_core::PolicyEngine> = if cli.allow.is_empty() {
        Arc::new(ion_core::DefaultPolicy)
    } else {
        Arc::new(ion_core::AllowlistPolicy::new(cli.allow.clone()))
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
