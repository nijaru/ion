//! Ion CLI host. This binary owns process lifetime and frontend selection.

use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
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
    /// Run against a real provider/model (e.g. desktop/qwen3.8:27b,
    /// openai-codex/gpt-5.6-luna, or openrouter/z-ai/glm-5.3-flash) instead
    /// of the scripted provider.
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

/// Best-effort git branch for the footer; None outside a repo.
fn git_branch() -> std::io::Result<Option<String>> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    Ok((!name.is_empty()).then_some(name))
}

/// Format the working directory compactly without collapsing distinct
/// paths that merely share a textual prefix with `$HOME`.
fn display_cwd(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    display_cwd_with_home(path, home.as_deref())
}

fn display_cwd_with_home(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(relative) = path.strip_prefix(home)
    {
        return if relative.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", relative.display())
        };
    }
    path.display().to_string()
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
    if let Some(notice) = store.startup_notice() {
        let _ = writeln!(io::stderr(), "store: {notice}");
    }
    let policy = policy_for(&cli.allow);
    let store_for_shutdown = Arc::clone(&store);
    let config = acp::AcpConfig {
        make_provider,
        store,
        policy,
        trust_project: cli.trust_project,
        agents_enabled: settings.agents_enabled(),
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

fn restore_tui_startup_terminal(mut terminal: ion_terminal::TerminalSession) {
    let restore_error = terminal.restore().err();
    drop(terminal);
    if let Some(err) = restore_error {
        let _ = writeln!(io::stderr(), "terminal restore failed: {err}");
    }
}

async fn run_tui(cli: &Cli, settings: &Settings) -> ExitCode {
    // Validate pure UI configuration before acquiring terminal, store, runtime,
    // or agent-host ownership. A malformed binding must not create durable
    // session state merely because interactive startup was attempted.
    let keymap = match tui::KeyMap::from_settings(&settings.keybindings) {
        Ok(keymap) => keymap,
        Err(err) => {
            let _ = writeln!(io::stderr(), "settings: {err}");
            return ExitCode::from(2);
        }
    };

    // The runtime owns model selection: /model <id> commits a durable
    // change through SessionHandle::switch_model and applies at the
    // next step boundary. The host only composes the resolver factory
    // and seeds the launch default.
    let root_provider: Arc<ion_core::SwitchingProvider<CliProvider>>;
    let make_provider: Arc<dyn Fn() -> CliProvider + Send + Sync>;
    let make_provider_for_model: Option<Arc<dyn Fn(String) -> CliProvider + Send + Sync>>;
    let model_name: Option<String>;
    let model_provider: Option<String>;
    // Composition facts hoisted for the session-switch factory: the
    // qualified resolver (provider-aware) and the provider-independent
    // base material let any resumed session rebuild its own provider
    // from its durable model reference (§14.8).
    let mut make_qualified_provider: Option<Arc<dyn Fn(String) -> CliProvider + Send + Sync>> =
        None;
    let mut provider_base_material: Option<ProviderMaterial> = None;
    let mut default_provider_label: Option<String> = None;
    let mut model_catalog = match settings.model_catalog() {
        Ok(catalog) => catalog,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            return ExitCode::from(2);
        }
    };
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
                    make_cli_provider_for_model(&model_ref, &default_provider, &make_material)
                });
            let qualified_model = format!("{}/{}", selection.provider, selection.model);
            if !model_catalog.iter().any(|model| model == &qualified_model) {
                model_catalog.push(qualified_model);
            }
            let initial = selection.model.clone();
            root_provider = Arc::new(ion_core::SwitchingProvider::switchable(
                initial.clone(),
                make(initial.clone()),
                Arc::clone(&make),
            ));
            let make_for_initial = Arc::clone(&make);
            make_provider = Arc::new(move || make_for_initial(initial.clone()));
            make_provider_for_model = Some(make.clone());
            model_name = Some(selection.model);
            model_provider = Some(selection.provider.clone());
            make_qualified_provider = Some(make);
            provider_base_material = Some(material);
            default_provider_label = Some(selection.provider.clone());
        }
        Ok(None) => {
            // Test hook (smoke checklist): hold each scripted step open
            // so a kill -9 can land mid-operation deterministically.
            let delay = std::env::var("ION_TEST_PROVIDER_DELAY_MS")
                .ok()
                .and_then(|ms| ms.parse::<u64>().ok())
                .map_or(std::time::Duration::ZERO, std::time::Duration::from_millis);
            let script = vec![ScriptedMessage::delayed(
                delay,
                "scripted provider: build with --model for real answers\n",
            )];
            root_provider = Arc::new(ion_core::SwitchingProvider::new(
                "scripted",
                CliProvider::Scripted(ScriptedProvider::new(script.clone())),
            ));
            let make: Arc<dyn Fn() -> CliProvider + Send + Sync> =
                Arc::new(move || CliProvider::Scripted(ScriptedProvider::new(script.clone())));
            make_provider = make;
            make_provider_for_model = None;
            model_name = None;
            model_provider = None;
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
            restore_tui_startup_terminal(guard);
            let _ = writeln!(io::stderr(), "store: {err}");
            return ExitCode::FAILURE;
        }
    };
    // The startup notice is rendered inside the transcript (HostConfig):
    // stderr here is already raw-mode and would corrupt the screen.
    let resume_session = if cli.resume {
        match store.latest_session().await {
            Ok(Some(id)) => Some(id),
            Ok(None) => {
                restore_tui_startup_terminal(guard);
                let _ = writeln!(io::stderr(), "no persisted session to resume");
                if let Err(close_err) = store.close().await {
                    tracing::error!(error = %close_err, "failed to close the session store");
                }
                return ExitCode::from(2);
            }
            Err(err) => {
                restore_tui_startup_terminal(guard);
                let _ = writeln!(io::stderr(), "store: {err}");
                if let Err(close_err) = store.close().await {
                    tracing::error!(error = %close_err, "failed to close the session store");
                }
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            restore_tui_startup_terminal(guard);
            let _ = writeln!(io::stderr(), "cwd: {err}");
            if let Err(close_err) = store.close().await {
                tracing::error!(error = %close_err, "failed to close the session store");
            }
            return ExitCode::FAILURE;
        }
    };
    let tools = match build_catalog(settings, cli).await {
        Ok(tools) => tools,
        Err(err) => {
            restore_tui_startup_terminal(guard);
            let _ = writeln!(io::stderr(), "cwd: {err}");
            if let Err(close_err) = store.close().await {
                tracing::error!(error = %close_err, "failed to close the session store");
            }
            return ExitCode::FAILURE;
        }
    };
    let trusted_resources = match ion_core::load_trusted_resources(&cwd, cli.trust_project) {
        Ok(resources) => resources,
        Err(err) => {
            restore_tui_startup_terminal(guard);
            let _ = writeln!(io::stderr(), "trusted resources: {err}");
            if let Err(close_err) = tools.close().await {
                tracing::error!(error = %close_err, "failed to close the tool catalog");
            }
            if let Err(close_err) = store.close().await {
                tracing::error!(error = %close_err, "failed to close the session store");
            }
            return ExitCode::FAILURE;
        }
    };
    let policy = policy_for(&cli.allow);
    // The TUI can grant approvals interactively (DESIGN.md §17.4);
    // print/ACP hosts stay fail-closed. The session manager owns the
    // runtime stack for the attached session; the factory below is the
    // single composition point for every runtime this process opens
    // (initial and every /new /resume /clone switch).
    let open_runtime: ion::session_manager::OpenRuntime = {
        let tools = tools.clone();
        let store = (*store).clone();
        let policy = Arc::clone(&policy);
        let trusted = trusted_resources.clone();
        let make_provider = Arc::clone(&make_provider);
        let make_provider_for_model = make_provider_for_model.clone();
        let root_provider = Arc::clone(&root_provider);
        let default_provider_label = default_provider_label
            .clone()
            .unwrap_or_else(|| "openrouter".to_owned());
        let make_material = provider_base_material.clone().unwrap_or_default();
        let make_provider_for_factory = Arc::clone(&make_provider);
        let make_qualified = make_qualified_provider
            .clone()
            .unwrap_or_else(|| Arc::new(move |_model_ref: String| make_provider_for_factory()));
        let agents_enabled = settings.agents_enabled();
        Arc::new(move |start| {
            let runtime_factory = |start: ion::session_manager::SessionStart| {
                let root_provider = Arc::clone(&root_provider);
                let tools = tools.clone();
                let store = store.clone();
                let policy = Arc::clone(&policy);
                let trusted = trusted.clone();
                let make_provider = Arc::clone(&make_provider);
                let make_provider_for_model = make_provider_for_model.clone();
                let default_provider_label = default_provider_label.clone();
                let make_material = make_material.clone();
                let make_qualified = Arc::clone(&make_qualified);
                Box::pin(async move {
                    let runtime = match start {
                        ion::session_manager::SessionStart::New => Runtime::start_interactive(
                            root_provider.clone(),
                            tools.clone(),
                            store.clone(),
                            Arc::clone(&policy),
                            trusted.clone(),
                        ),
                        ion::session_manager::SessionStart::Resume(session_id) => {
                            // The durable model is authoritative (§14.8): a fresh
                            // provider is composed for it, never the launch default.
                            let loaded = store.load(session_id).await.map_err(|err| {
                                ion_core::RuntimeError::OperationFailed(err.to_string())
                            })?;
                            let main_model = loaded.session.initial_model_ref.clone();
                            let provider = make_cli_provider_for_model(
                                &main_model,
                                &default_provider_label,
                                &make_material,
                            );
                            Runtime::open_interactive(
                                ion_core::SwitchingProvider::switchable(
                                    main_model.clone(),
                                    provider,
                                    Arc::clone(&make_qualified),
                                ),
                                tools.clone(),
                                store.clone(),
                                session_id,
                                Arc::clone(&policy),
                                trusted.clone(),
                            )
                            .await?
                        }
                    };
                    // Hosted-agent service for this session's family; the boxed
                    // teardown closes the stack in the manager's single order.
                    let agent_host = ion::enable_agent_host_with_model_resolver(
                        &tools,
                        &runtime,
                        &store,
                        make_provider.clone(),
                        make_provider_for_model.clone(),
                        trusted.clone(),
                        ion::AgentHostOptions {
                            max_active_agents: 4,
                            agents_enabled,
                        },
                    )
                    .await;
                    match agent_host {
                        Ok(host) => Ok(ion::session_manager::OpenedRuntime {
                            runtime,
                            hosted_teardown: Some(Box::new(move || {
                                Box::pin(async move { host.close().await })
                            })),
                        }),
                        Err(err) => {
                            let _ = runtime.session().close().await;
                            let _ = runtime.join().await;
                            Err(ion_core::RuntimeError::OperationFailed(format!(
                                "agents: {err}"
                            )))
                        }
                    }
                })
            };
            runtime_factory(start)
        })
    };
    let manager =
        ion::session_manager::SessionManager::new((*store).clone(), Arc::clone(&open_runtime));
    // The initial session opens through the same factory as every later
    // switch: one composition point, one teardown order. The CLI's
    // `--resume` already resolved the durable session id.
    let initial_start = match resume_session {
        Some(session_id) => ion::session_manager::SessionStart::Resume(session_id),
        None => ion::session_manager::SessionStart::New,
    };
    let attached = match (open_runtime)(initial_start).await {
        Ok(opened) => match manager.adopt(opened).await {
            Ok(attached) => attached,
            Err(err) => {
                restore_tui_startup_terminal(guard);
                let _ = writeln!(io::stderr(), "session: {err}");
                return run_tui_cleanup_failure(tools, store).await;
            }
        },
        Err(err) => {
            restore_tui_startup_terminal(guard);
            let _ = if resume_session.is_some() {
                writeln!(io::stderr(), "resume: {err}")
            } else {
                writeln!(io::stderr(), "session: {err}")
            };
            return run_tui_cleanup_failure(tools, store).await;
        }
    };
    let session = attached.handle();
    let result = tui::run(
        session.clone(),
        resume_session,
        settings.theme(),
        keymap,
        tui::HostConfig {
            model_name,
            model_provider,
            model_catalog,
            hide_thinking_block: settings.hide_thinking_block,
            startup_notice: store.startup_notice().map(str::to_owned),
            cwd_label: Some(display_cwd(&cwd)),
            branch: git_branch().ok().flatten(),
            workspace_files: tui::workspace_file_list(&cwd),
        },
        tui::SessionHost {
            manager: Some(manager),
            attached: Some(attached),
        },
        guard,
    )
    .await;
    if result.is_err() {
        // The TUI died before its own close path; shut the actor down
        // or join would await a task waiting on this very handle. The
        // attached session was consumed by tui::run's close path; only
        // the handle fallback remains for the pre-adopt failure window.
        if let Err(err) = session.close().await {
            tracing::error!(error = %err, "failed to close the session after TUI failure");
        }
    }

    // The attached runtime stack (runtime join + hosted teardown) was
    // closed inside tui::run via the session manager's single order;
    // the process-owned catalog/store close last.
    let catalog_close = tools.close().await;
    let store_close = store.close().await;
    let mut cleanup_failed = false;
    if let Err(err) = catalog_close {
        cleanup_failed = true;
        tracing::error!(error = %err, "failed to close the tool catalog");
    }
    if let Err(err) = store_close {
        cleanup_failed = true;
        tracing::error!(error = %err, "failed to close the session store");
    }

    match result {
        Ok(()) if !cleanup_failed => ExitCode::SUCCESS,
        Ok(()) => ExitCode::FAILURE,
        Err(err) => {
            let _ = writeln!(io::stderr(), "{err}");
            ExitCode::FAILURE
        }
    }
}

/// Shared close path for a run_tui startup failure after the terminal
/// was restored: the manager/attachment never existed, so only the
/// process-owned catalog and store close here.
async fn run_tui_cleanup_failure(
    tools: ion_core::ToolCatalog,
    store: std::sync::Arc<SessionStore>,
) -> ExitCode {
    let mut failed = false;
    if let Err(err) = tools.close().await {
        failed = true;
        tracing::error!(error = %err, "failed to close the tool catalog");
    }
    if let Err(err) = store.close().await {
        failed = true;
        tracing::error!(error = %err, "failed to close the session store");
    }
    let _ = failed;
    ExitCode::FAILURE
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
            Arc::new(move || make_cli_provider_for_model(&model_ref, &default_provider, &material))
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
    desktop_api_key: String,
    desktop_base_url: String,
    codex_credential: Option<CodexCredential>,
    reasoning_effort: Option<String>,
}

fn provider_material(
    selection: &ModelSelection,
    settings: &Settings,
) -> Result<ProviderMaterial, String> {
    // Keep provider-independent settings in the base material so a qualified
    // catalog entry can switch providers without silently losing the local
    // endpoint or reasoning configuration.
    let base = ProviderMaterial {
        desktop_api_key: settings.desktop_api_key(),
        desktop_base_url: settings.desktop_base_url(),
        reasoning_effort: settings
            .thinking_level()
            .reasoning_effort()
            .map(str::to_owned),
        ..ProviderMaterial::default()
    };
    match selection.provider.as_str() {
        "openai-codex" => Ok(ProviderMaterial {
            codex_credential: Some(CodexCredential::from_environment_or_pi()?),
            ..base
        }),
        "openrouter" => Ok(ProviderMaterial {
            openrouter_key: Some(
                std::env::var("OPENROUTER_API_KEY")
                    .map_err(|_| "model requires OPENROUTER_API_KEY to be set".to_owned())?,
            ),
            ..base
        }),
        "desktop" => Ok(base),
        provider => Err(format!("unsupported provider {provider:?}")),
    }
}

/// Refresh only provider credentials when a qualified catalog entry crosses
/// providers. Missing credentials become a model-visible unavailable
/// provider through `make_cli_provider`; they do not get silently replaced
/// with the launch provider.
fn material_for_provider(base: &ProviderMaterial, provider: &str) -> ProviderMaterial {
    let mut material = base.clone();
    match provider {
        "openai-codex" => {
            material.codex_credential = CodexCredential::from_environment_or_pi().ok();
        }
        "openrouter" => {
            material.openrouter_key = std::env::var("OPENROUTER_API_KEY").ok();
        }
        _ => {}
    }
    material
}

fn make_cli_provider_for_model(
    model_ref: &str,
    default_provider: &str,
    base: &ProviderMaterial,
) -> CliProvider {
    let provider = match parse_model_reference(model_ref, default_provider) {
        Ok(selection) => selection.provider,
        Err(err) => return CliProvider::Unavailable(err),
    };
    let material = material_for_provider(base, &provider);
    make_cli_provider(model_ref, default_provider, &material)
}

fn parse_model_reference(raw: &str, default_provider: &str) -> Result<ModelSelection, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("model reference cannot be empty".to_owned());
    }
    for provider in ["openai-codex", "openrouter", "desktop"] {
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
        "desktop" => CliProvider::Desktop(
            OpenRouterProvider::new_with_base_url(
                selection.model,
                &material.desktop_api_key,
                &material.desktop_base_url,
            )
            .with_context_window_hint(262_144),
        ),
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
    if let Some(notice) = store.startup_notice() {
        eprintln!("store: {notice}");
    };
    let policy = policy_for(&cli.allow);
    let runtime = Runtime::start_with_policy_and_resources(
        (make_provider)(),
        tools.clone(),
        store.clone(),
        policy,
        trusted_resources.clone(),
    );
    let agent_host = match ion::enable_agent_host(
        &tools,
        &runtime,
        &store,
        Arc::clone(&make_provider),
        trusted_resources.clone(),
        ion::AgentHostOptions {
            max_active_agents: 4,
            agents_enabled: settings.agents_enabled(),
        },
    )
    .await
    {
        Ok(host) => host,
        Err(err) => {
            let session = runtime.session();
            let _ = session.close().await;
            let _ = runtime.join().await;
            let _ = tools.close().await;
            let _ = store.close().await;
            return Err(RuntimeError::OperationFailed(format!(
                "attach agent host: {err}"
            )));
        }
    };
    let session = runtime.session();
    let result = PrintFrontend::new(io::stdout()).run(&session, prompt).await;
    let shutdown = session.close().await;
    let join = runtime.join().await;
    let agent_close = agent_host.close().await;
    let catalog_close = tools.close().await;
    let store_close = store.close().await;
    result?;
    shutdown?;
    join?;
    agent_close.map_err(RuntimeError::OperationFailed)?;
    catalog_close.map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
    store_close.map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_core::Provider;

    #[test]
    fn parses_desktop_model_reference() {
        assert_eq!(
            parse_model_reference("desktop/qwen3.8:27b", "openrouter").unwrap(),
            ModelSelection {
                provider: "desktop".to_owned(),
                model: "qwen3.8:27b".to_owned(),
            }
        );
    }

    #[tokio::test]
    async fn qualified_switch_keeps_desktop_material_when_crossing_providers() {
        let settings: Settings = toml::from_str(
            r#"
            defaultProvider = "desktop"
            defaultModel = "qwen3.8:27b"
            desktopBaseUrl = "http://127.0.0.1:9000/v1"
            "#,
        )
        .unwrap();
        let selection = settings.model_selection().unwrap().unwrap();
        let material = provider_material(&selection, &settings).unwrap();
        match make_cli_provider_for_model("desktop/next", "openrouter", &material) {
            CliProvider::Desktop(provider) => {
                assert_eq!(provider.context_window().await, Some(262_144));
            }
            CliProvider::Scripted(_)
            | CliProvider::OpenAICodex(_)
            | CliProvider::OpenRouter(_)
            | CliProvider::Unavailable(_) => panic!("expected desktop provider"),
        }
    }

    #[tokio::test]
    async fn desktop_material_builds_a_local_provider_without_a_key() {
        let settings: Settings = toml::from_str(
            r#"
            defaultProvider = "desktop"
            defaultModel = "qwen3.8:27b"
            desktopBaseUrl = "http://127.0.0.1:9000/v1"
            "#,
        )
        .unwrap();
        let selection = settings.model_selection().unwrap().unwrap();
        let material = provider_material(&selection, &settings).unwrap();
        match make_cli_provider(&selection.model, &selection.provider, &material) {
            CliProvider::Desktop(provider) => {
                assert_eq!(provider.context_window().await, Some(262_144));
            }
            CliProvider::Scripted(_)
            | CliProvider::OpenAICodex(_)
            | CliProvider::OpenRouter(_)
            | CliProvider::Unavailable(_) => panic!("expected desktop provider"),
        }
    }

    #[test]
    fn cwd_display_is_home_relative_without_prefix_collisions() {
        let home = Path::new("/Users/test");
        assert_eq!(
            display_cwd_with_home(Path::new("/Users/test/project"), Some(home)),
            "~/project"
        );
        assert_eq!(
            display_cwd_with_home(Path::new("/Users/tester/project"), Some(home)),
            "/Users/tester/project"
        );
        assert_eq!(
            display_cwd_with_home(Path::new("/Users/test"), Some(home)),
            "~"
        );
    }
}
