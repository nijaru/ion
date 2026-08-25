//! Ion CLI host. This binary owns process lifetime and frontend selection.

use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use ion::enable_children;
use ion::openai_codex::{CodexCredential, OpenAICodexProvider};
use ion::openrouter::OpenRouterProvider;
use ion::print::PrintFrontend;
use ion::settings::{ModelSelection, Settings};
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
    /// Run against a real provider/model (e.g. openai-codex/gpt-5.6-luna
    /// or openrouter/stealth/ox-alpha) instead of the scripted provider.
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
    /// Trust project-local configuration and model-facing resources (.ion/
    /// extensions.toml, AGENTS.md, and .ion/instructions.md) for this run
    /// (§17.2, §24.5). Never set automatically.
    #[arg(long = "trust-project")]
    trust_project: bool,
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

/// The host's approval policy: default (approval-gated) unless the
/// caller grants explicit actions (DESIGN.md §17).
fn policy_for(allow: &[String]) -> Arc<dyn ion_core::PolicyEngine> {
    if allow.is_empty() {
        Arc::new(ion_core::DefaultPolicy)
    } else {
        Arc::new(ion_core::AllowlistPolicy::new(allow.to_vec()))
    }
}

async fn run_acp(cli: &Cli, settings: &Settings) -> ExitCode {
    let make_provider = match provider_factory(cli, settings) {
        Ok(factory) => factory,
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
    let policy = policy_for(&cli.allow);
    let store_for_shutdown = Arc::clone(&store);
    let config = acp::AcpConfig {
        make_provider,
        store,
        policy,
        trust_project: cli.trust_project,
    };
    let result = acp::serve(tokio::io::stdin(), tokio::io::stdout(), config).await;
    if let Err(err) = store_for_shutdown.close().await {
        tracing::error!(error = %err, "failed to close the session store");
    }
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            let _ = writeln!(io::stderr(), "acp: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Compose the tool surface: core tools plus explicitly active MCP server
/// tools. A failing server logs and is skipped - one broken server never
/// blocks startup (DESIGN.md §19.1). The workspace directory is the tool
/// path boundary, so it must resolve or startup fails.
async fn build_catalog(
    settings: &Settings,
    cli: &Cli,
) -> Result<ion_core::ToolCatalog, std::io::Error> {
    let cwd = std::env::current_dir()?;
    let tools = ion_core::ToolCatalog::with_cwd_and_sandbox(cwd.clone(), settings.sandbox_mode());
    tools.set_active_mcp_servers(&settings.active_mcp_servers);
    if !settings.mcp_servers.is_empty() {
        let defs: Vec<ion_core::ServerDef> = settings
            .mcp_servers
            .iter()
            .cloned()
            .map(Into::into)
            .collect();
        ion_core::McpService::new().start_into(&defs, &tools).await;
    }
    // Project-local extension manifests load only under an explicit
    // trust grant (§24.5).
    let ext_defs =
        ion::settings::load_extension_defs(settings, Some(cwd.as_path()), cli.trust_project);
    if !ext_defs.is_empty() {
        ion_core::ExtensionService::new()
            .start_into(&ext_defs, &tools)
            .await;
    }
    Ok(tools)
}

async fn run_tui(cli: &Cli, settings: &Settings) -> ExitCode {
    // The runtime owns model selection: /model <id> commits a durable
    // change through SessionHandle::switch_model and applies at the
    // next step boundary. The host only composes the resolver factory
    // and seeds the launch default.
    let root_provider: Arc<ion_core::SwitchingProvider<CliProvider>>;
    let make_provider: Arc<dyn Fn() -> CliProvider + Send + Sync>;
    let model_name: Option<String>;
    match resolve_model(cli.model.clone(), settings) {
        Ok(Some(selection)) => {
            let material = match provider_material(&selection, settings) {
                Ok(material) => material,
                Err(err) => {
                    let _ = writeln!(io::stderr(), "{err}");
                    return ExitCode::from(2);
                }
            };
            let default_provider = selection.provider.clone();
            let make_material = material.clone();
            let make: Arc<dyn Fn(String) -> CliProvider + Send + Sync> =
                Arc::new(move |model_ref: String| {
                    make_cli_provider(&model_ref, &default_provider, &make_material)
                });
            let initial = selection.model.clone();
            root_provider = Arc::new(ion_core::SwitchingProvider::switchable(
                initial.clone(),
                make(initial.clone()),
                Arc::clone(&make),
            ));
            make_provider = Arc::new(move || make(initial.clone()));
            model_name = Some(selection.model);
        }
        Ok(None) => {
            let make: Arc<dyn Fn() -> CliProvider + Send + Sync> = Arc::new(|| {
                CliProvider::Scripted(ScriptedProvider::new(vec![ScriptedMessage::text(
                    "scripted provider: build with --model for real answers\n",
                )]))
            });
            root_provider = Arc::new(ion_core::SwitchingProvider::new(
                "scripted",
                CliProvider::Scripted(ScriptedProvider::new(vec![ScriptedMessage::text(
                    "scripted provider: build with --model for real answers\n",
                )])),
            ));
            make_provider = make;
            model_name = None;
        }
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            return ExitCode::from(2);
        }
    }
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
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            let _ = writeln!(io::stderr(), "cwd: {err}");
            return ExitCode::FAILURE;
        }
    };
    let tools = match build_catalog(settings, cli).await {
        Ok(tools) => tools,
        Err(err) => {
            let _ = writeln!(io::stderr(), "cwd: {err}");
            return ExitCode::FAILURE;
        }
    };
    let trusted_resources = match ion_core::load_trusted_resources(&cwd, cli.trust_project) {
        Ok(resources) => resources,
        Err(err) => {
            let _ = writeln!(io::stderr(), "trusted resources: {err}");
            if let Err(close_err) = tools.close().await {
                tracing::error!(error = %close_err, "failed to close the tool catalog");
            }
            return ExitCode::FAILURE;
        }
    };
    let policy = policy_for(&cli.allow);
    let runtime = if let Some(session_id) = resume_session {
        match Runtime::open_session_with_resources(
            root_provider,
            tools.clone(),
            (*store).clone(),
            session_id,
            trusted_resources.clone(),
        )
        .await
        {
            Ok(runtime) => runtime,
            Err(err) => {
                let _ = writeln!(io::stderr(), "resume: {err}");
                if let Err(close_err) = tools.close().await {
                    tracing::error!(error = %close_err, "failed to close the tool catalog");
                }
                return ExitCode::FAILURE;
            }
        }
    } else {
        Runtime::start_with_policy_and_resources(
            root_provider,
            tools.clone(),
            (*store).clone(),
            policy,
            trusted_resources.clone(),
        )
    };
    enable_children(
        &tools,
        &store,
        Arc::clone(&make_provider),
        runtime.session_id(),
        trusted_resources.clone(),
    );
    let keymap = match tui::KeyMap::from_settings(&settings.keybindings) {
        Ok(keymap) => keymap,
        Err(err) => {
            let _ = writeln!(io::stderr(), "settings: {err}");
            if let Err(close_err) = tools.close().await {
                tracing::error!(error = %close_err, "failed to close the tool catalog");
            }
            return ExitCode::from(2);
        }
    };
    let session = runtime.session();
    let result = tui::run(
        session.clone(),
        resume_session,
        settings.theme(),
        keymap,
        tui::HostConfig {
            model_name,
            hide_thinking_block: settings.hide_thinking_block,
        },
        guard,
    )
    .await;
    if result.is_err() {
        // The TUI died before its own close path; shut the actor down
        // or join would await a task waiting on this very handle.
        if let Err(err) = session.close().await {
            tracing::warn!(error = %err, "failed to close the session after TUI failure");
        }
    }
    match result {
        Ok(()) => {
            let join = runtime.join().await;
            let catalog_close = tools.close().await;
            let store_close = store.close().await;
            if let Err(err) = catalog_close {
                tracing::error!(error = %err, "failed to close the tool catalog");
                return ExitCode::FAILURE;
            }
            if let Err(err) = store_close {
                tracing::error!(error = %err, "failed to close the session store");
                return ExitCode::FAILURE;
            }
            if let Err(err) = join {
                tracing::error!(error = %err, "runtime join failed");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(err) => {
            let join = runtime.join().await;
            let catalog_close = tools.close().await;
            let store_close = store.close().await;
            if let Err(close_err) = catalog_close {
                tracing::error!(error = %close_err, "failed to close the tool catalog");
            }
            if let Err(close_err) = store_close {
                tracing::error!(error = %close_err, "failed to close the session store");
            }
            if let Err(join_err) = join {
                tracing::error!(error = %join_err, "runtime join failed");
            }
            let _ = writeln!(io::stderr(), "{err}");
            ExitCode::FAILURE
        }
    }
}

/// `--model` wins; otherwise the settings default (pi-style: the
/// compiled-in defaults mirror the maintainer's pi settings).
fn resolve_model(
    cli_model: Option<String>,
    settings: &Settings,
) -> Result<Option<ModelSelection>, String> {
    let configured = settings.model_selection()?;
    let Some(cli_model) = cli_model else {
        return Ok(configured);
    };
    let default_provider = configured
        .as_ref()
        .map_or("openrouter", |selection| selection.provider.as_str());
    parse_model_reference(&cli_model, default_provider).map(Some)
}

/// The provider factory shared by the root session and any children it
/// delegates to (§20): every child gets a fresh adapter instance.
fn provider_factory(
    cli: &Cli,
    settings: &Settings,
) -> Result<Arc<dyn Fn() -> CliProvider + Send + Sync>, String> {
    let selection = resolve_model(cli.model.clone(), settings)?;
    Ok(match selection {
        Some(selection) => {
            let material = provider_material(&selection, settings)?;
            let default_provider = selection.provider.clone();
            let model_ref = selection.model.clone();
            Arc::new(move || make_cli_provider(&model_ref, &default_provider, &material))
        }
        None => Arc::new(|| {
            CliProvider::Scripted(ScriptedProvider::new(vec![ScriptedMessage::text(
                "scripted provider: build with --model for real answers\n",
            )]))
        }),
    })
}

#[derive(Clone, Default)]
struct ProviderMaterial {
    openrouter_key: Option<String>,
    codex_credential: Option<CodexCredential>,
    reasoning_effort: Option<String>,
}

fn provider_material(
    selection: &ModelSelection,
    settings: &Settings,
) -> Result<ProviderMaterial, String> {
    match selection.provider.as_str() {
        "openai-codex" => Ok(ProviderMaterial {
            codex_credential: Some(CodexCredential::from_environment_or_pi()?),
            reasoning_effort: settings
                .thinking_level()
                .reasoning_effort()
                .map(str::to_owned),
            ..ProviderMaterial::default()
        }),
        "openrouter" => Ok(ProviderMaterial {
            openrouter_key: Some(
                std::env::var("OPENROUTER_API_KEY")
                    .map_err(|_| "model requires OPENROUTER_API_KEY to be set".to_owned())?,
            ),
            ..ProviderMaterial::default()
        }),
        provider => Err(format!("unsupported provider {provider:?}")),
    }
}

fn parse_model_reference(raw: &str, default_provider: &str) -> Result<ModelSelection, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("model reference cannot be empty".to_owned());
    }
    for provider in ["openai-codex", "openrouter"] {
        let prefix = format!("{provider}/");
        if let Some(model) = raw.strip_prefix(&prefix) {
            if model.is_empty() {
                return Err(format!("model reference {raw:?} has no model id"));
            }
            return Ok(ModelSelection {
                provider: provider.to_owned(),
                model: model.to_owned(),
            });
        }
    }
    Ok(ModelSelection {
        provider: default_provider.to_owned(),
        model: raw.to_owned(),
    })
}

fn make_cli_provider(
    model_ref: &str,
    default_provider: &str,
    material: &ProviderMaterial,
) -> CliProvider {
    let selection = match parse_model_reference(model_ref, default_provider) {
        Ok(selection) => selection,
        Err(err) => return CliProvider::Unavailable(err),
    };
    match selection.provider.as_str() {
        "openai-codex" => match &material.codex_credential {
            Some(credential) => CliProvider::OpenAICodex(
                OpenAICodexProvider::from_credential(selection.model, credential.clone())
                    .with_reasoning_effort(material.reasoning_effort.clone()),
            ),
            None => CliProvider::Unavailable(
                "OpenAI Codex credential is unavailable for this model".to_owned(),
            ),
        },
        "openrouter" => match &material.openrouter_key {
            Some(key) => CliProvider::OpenRouter(OpenRouterProvider::new(selection.model, key)),
            None => CliProvider::Unavailable(
                "OPENROUTER_API_KEY is unavailable for this model".to_owned(),
            ),
        },
        provider => CliProvider::Unavailable(format!("unsupported provider {provider:?}")),
    }
}

async fn run_print(prompt: String, cli: &Cli, settings: &Settings) -> Result<(), RuntimeError> {
    let make_provider = provider_factory(cli, settings).map_err(RuntimeError::OperationFailed)?;
    let cwd =
        std::env::current_dir().map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
    let tools = build_catalog(settings, cli)
        .await
        .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
    let trusted_resources = match ion_core::load_trusted_resources(&cwd, cli.trust_project) {
        Ok(resources) => resources,
        Err(err) => {
            if let Err(close_err) = tools.close().await {
                return Err(RuntimeError::OperationFailed(format!(
                    "{err}; tool catalog close failed: {close_err}"
                )));
            }
            return Err(RuntimeError::OperationFailed(err));
        }
    };
    let store = match SessionStore::open(default_db_path()) {
        Ok(store) => store,
        Err(err) => {
            if let Err(close_err) = tools.close().await {
                return Err(RuntimeError::OperationFailed(format!(
                    "{err}; tool catalog close failed: {close_err}"
                )));
            }
            return Err(RuntimeError::OperationFailed(err.to_string()));
        }
    };
    let policy = policy_for(&cli.allow);
    let runtime = Runtime::start_with_policy_and_resources(
        (make_provider)(),
        tools.clone(),
        store.clone(),
        policy,
        trusted_resources.clone(),
    );
    enable_children(
        &tools,
        &store,
        Arc::clone(&make_provider),
        runtime.session_id(),
        trusted_resources.clone(),
    );
    let session = runtime.session();
    let result = PrintFrontend::new(io::stdout()).run(&session, prompt).await;
    let shutdown = session.close().await;
    let join = runtime.join().await;
    let catalog_close = tools.close().await;
    let store_close = store.close().await;
    result?;
    shutdown?;
    join?;
    catalog_close.map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
    store_close.map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
    Ok(())
}
