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
    /// Continue the most recent persisted session (alias for --resume).
    #[arg(short = 'c', long = "continue")]
    continue_session: bool,
    /// Open a specific persisted session by id (exact or unique prefix).
    /// Rejected with --resume/--continue/--fork/--no-session.
    #[arg(long = "session", value_name = "ID")]
    session: Option<String>,
    /// Display title for a newly created session (fresh, --fork, or
    /// ephemeral). Rejected when opening an existing session.
    #[arg(short = 'n', long = "name", value_name = "NAME")]
    name: Option<String>,
    /// Fork a persisted session (id or unique prefix) into a new session
    /// and open the fork. Rejected with --session/--resume/--continue/
    /// --no-session.
    #[arg(long = "fork", value_name = "ID")]
    fork: Option<String>,
    /// Ephemeral run: in-memory store, nothing persisted. Rejected with
    /// every session-selection flag.
    #[arg(long = "no-session")]
    no_session: bool,
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
    /// Interactive TUI mode: `regular` (default, inline scrollback) or
    /// `fullscreen` (alt-screen transcript with search; pi parity).
    #[arg(long = "tui-mode", value_enum)]
    tui_mode: Option<TuiModeArg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum TuiModeArg {
    Regular,
    Fullscreen,
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

/// How the interactive TUI opens its first session (CLI flags
/// resolved against the store; forks are cloned before open).
enum InitialSession {
    /// Fresh session, optionally titled with --name.
    New { title: Option<String> },
    /// Existing session (resume/continue/session id/fork clone).
    Resume(ion_core::SessionId),
}

/// Reject contradictory session flags before the terminal, store, or
/// runtime exists. Pure: unit-tested below.
fn validate_session_flags(cli: &Cli) -> Result<(), String> {
    if cli.no_session {
        let mut conflicts = Vec::new();
        if cli.session.is_some() {
            conflicts.push("--session");
        }
        if cli.continue_session || cli.resume {
            conflicts.push("--continue/--resume");
        }
        if cli.fork.is_some() {
            conflicts.push("--fork");
        }
        if !conflicts.is_empty() {
            return Err(format!(
                "--no-session cannot be combined with {}",
                conflicts.join(", ")
            ));
        }
        return Ok(());
    }
    if cli.fork.is_some() {
        let mut conflicts = Vec::new();
        if cli.session.is_some() {
            conflicts.push("--session");
        }
        if cli.continue_session || cli.resume {
            conflicts.push("--continue/--resume");
        }
        if !conflicts.is_empty() {
            return Err(format!(
                "--fork cannot be combined with {}",
                conflicts.join(", ")
            ));
        }
    }
    if cli.session.is_some() && (cli.continue_session || cli.resume) {
        return Err("--session cannot be combined with --continue/--resume".to_owned());
    }
    if cli.name.is_some() && (cli.continue_session || cli.resume || cli.session.is_some()) {
        return Err("--name only applies to newly created sessions".to_owned());
    }
    Ok(())
}

/// Match a session argument against durable sessions (pi `--session`
/// semantics): exact display id (`session-<uuid>`) or bare UUID first,
/// then unique prefix over the display form or the UUID part, so both
/// `session-1111…` and `1111…` work. Pure over a snapshot.
fn match_session_id(
    sessions: &[ion_core::SessionSummary],
    arg: &str,
) -> Result<ion_core::SessionId, String> {
    if let Some(found) = sessions.iter().find(|s| s.id.to_string() == arg) {
        return Ok(found.id);
    }
    if let Some(id) = ion_core::SessionId::parse(arg) {
        if sessions.iter().any(|s| s.id == id) {
            return Ok(id);
        }
        return Err(format!("no session matching {arg:?}"));
    }
    let mut prefixed = sessions.iter().filter(|s| {
        let display = s.id.to_string();
        display.starts_with(arg) || display["session-".len()..].starts_with(arg)
    });
    match (prefixed.next(), prefixed.next()) {
        (Some(only), None) => Ok(only.id),
        (None, _) => Err(format!("no session matching {arg:?}")),
        (Some(_), Some(_)) => Err(format!("ambiguous session prefix {arg:?}; use the full id")),
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
) -> Result<(ion_core::ToolCatalog, Option<ion_core::ExtensionService>), std::io::Error> {
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
    // trust grant (§24.5). The service handle stays alive for the TUI
    // (Phase G): its hub drives extension UI, its registry resolves
    // extension commands.
    let ext_defs =
        ion::settings::load_extension_defs(settings, Some(cwd.as_path()), cli.trust_project);
    let extension_service = (!ext_defs.is_empty()).then(ion_core::ExtensionService::new);
    if let Some(service) = &extension_service {
        service.start_into(&ext_defs, &tools).await;
    }
    Ok((tools, extension_service))
}

/// TUI startup failure after the terminal guard exists: restore the
/// terminal, report, close the store, exit. Usage errors (bad flags,
/// unknown sessions) exit 2; infrastructure failures exit 1.
async fn abort_tui_startup(
    guard: ion_terminal::TerminalSession,
    store: &SessionStore,
    message: String,
    code: u8,
) -> ExitCode {
    restore_tui_startup_terminal(guard);
    let _ = writeln!(io::stderr(), "{message}");
    if let Err(close_err) = store.close().await {
        tracing::error!(error = %close_err, "failed to close the session store");
    }
    ExitCode::from(code)
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
    // Session flags are validated just as early: contradictions must
    // fail before any durable state exists.
    if let Err(err) = validate_session_flags(cli) {
        let _ = writeln!(io::stderr(), "{err}");
        return ExitCode::from(2);
    }

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
    let store = match if cli.no_session {
        // Ephemeral run: nothing reaches the durable database.
        SessionStore::open_in_memory()
    } else {
        SessionStore::open(default_db_path())
    } {
        Ok(store) => Arc::new(store),
        Err(err) => {
            restore_tui_startup_terminal(guard);
            let _ = writeln!(io::stderr(), "store: {err}");
            return ExitCode::FAILURE;
        }
    };
    // The startup notice is rendered inside the transcript (HostConfig):
    // stderr here is already raw-mode and would corrupt the screen.
    //
    // CLI session selection resolves before any runtime exists: latest
    // for --continue/--resume, id-or-unique-prefix for --session, clone
    // first for --fork. Flag validation already ran; resolution failures
    // below are usage errors (exit 2), store failures exit 1.
    let initial = if cli.no_session {
        InitialSession::New {
            title: cli.name.clone(),
        }
    } else if let Some(arg) = cli.fork.as_deref() {
        let sessions = match store.list_sessions().await {
            Ok(sessions) => sessions,
            Err(err) => return abort_tui_startup(guard, &store, format!("store: {err}"), 1).await,
        };
        let source = match match_session_id(&sessions, arg) {
            Ok(id) => id,
            Err(err) => return abort_tui_startup(guard, &store, err, 2).await,
        };
        let source_title = sessions
            .iter()
            .find(|s| s.id == source)
            .map(|s| s.title.clone())
            .unwrap_or_default();
        let title = cli.name.clone().unwrap_or_else(|| {
            if source_title.is_empty() {
                format!("Fork of {}", &source.to_string()[..8])
            } else {
                format!("Fork of {source_title}")
            }
        });
        match store.clone_session(source, &title).await {
            Ok(id) => InitialSession::Resume(id),
            Err(err) => return abort_tui_startup(guard, &store, format!("store: {err}"), 1).await,
        }
    } else if let Some(arg) = cli.session.as_deref() {
        let sessions = match store.list_sessions().await {
            Ok(sessions) => sessions,
            Err(err) => return abort_tui_startup(guard, &store, format!("store: {err}"), 1).await,
        };
        match match_session_id(&sessions, arg) {
            Ok(id) => InitialSession::Resume(id),
            Err(err) => return abort_tui_startup(guard, &store, err, 2).await,
        }
    } else if cli.resume || cli.continue_session {
        match store.latest_session().await {
            Ok(Some(id)) => InitialSession::Resume(id),
            Ok(None) => {
                return abort_tui_startup(
                    guard,
                    &store,
                    "no persisted session to resume".to_owned(),
                    2,
                )
                .await;
            }
            Err(err) => {
                return abort_tui_startup(guard, &store, format!("store: {err}"), 1).await;
            }
        }
    } else {
        InitialSession::New {
            title: cli.name.clone(),
        }
    };
    let resume_session = match initial {
        InitialSession::New { .. } => None,
        InitialSession::Resume(id) => Some(id),
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
    let (tools, extension_service) = match build_catalog(settings, cli).await {
        Ok(built) => built,
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
    // session flags already resolved the durable session id; --name
    // titles a fresh session before adopt reads the title back.
    let new_title = match &initial {
        InitialSession::New { title } => title.clone(),
        InitialSession::Resume(_) => None,
    };
    let initial_start = match resume_session {
        Some(session_id) => ion::session_manager::SessionStart::Resume(session_id),
        None => ion::session_manager::SessionStart::New,
    };
    let attached = match (open_runtime)(initial_start).await {
        Ok(opened) => {
            if let Some(title) = new_title {
                let session_id = opened.runtime.session_id();
                if let Err(err) = store.rename_session(session_id, &title).await {
                    restore_tui_startup_terminal(guard);
                    let _ = writeln!(io::stderr(), "session: {err}");
                    return run_tui_cleanup_failure(tools, store).await;
                }
            }
            match manager.adopt(opened).await {
                Ok(attached) => attached,
                Err(err) => {
                    restore_tui_startup_terminal(guard);
                    let _ = writeln!(io::stderr(), "session: {err}");
                    return run_tui_cleanup_failure(tools, store).await;
                }
            }
        }
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
            launch_mode: tui_launch_mode(cli.tui_mode, settings),
            model_name,
            model_provider,
            model_catalog,
            default_model: settings
                .model_selection()
                .ok()
                .flatten()
                .map(|selection| format!("{}/{}", selection.provider, selection.model)),
            hide_thinking_block: settings.hide_thinking_block,
            startup_notice: store.startup_notice().map(str::to_owned),
            cwd_label: Some(display_cwd(&cwd)),
            branch: git_branch().ok().flatten(),
            workspace_files: tui::workspace_file_list(&cwd),
            extension_service,
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

/// Resolve the launch TUI mode: the `--tui-mode` flag overrides the
/// settings value (pi parity).
fn tui_launch_mode(cli_mode: Option<TuiModeArg>, settings: &Settings) -> ion::settings::TuiMode {
    match cli_mode {
        Some(TuiModeArg::Regular) => ion::settings::TuiMode::Regular,
        Some(TuiModeArg::Fullscreen) => ion::settings::TuiMode::Fullscreen,
        None => settings.tui_mode(),
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
    // Session selection is an interactive-TUI concept; failing here
    // beats silently ignoring the flags. Only --no-session carries
    // over (an ephemeral one-shot run).
    if cli.session.is_some()
        || cli.continue_session
        || cli.resume
        || cli.fork.is_some()
        || cli.name.is_some()
    {
        return Err(RuntimeError::OperationFailed(
            "session flags (--session/--continue/--resume/--fork/--name) apply to the interactive TUI, not --print".to_owned(),
        ));
    }
    let make_provider = provider_factory(cli, settings).map_err(RuntimeError::OperationFailed)?;
    let cwd =
        std::env::current_dir().map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
    let (tools, _extension_service) = build_catalog(settings, cli)
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
    let store = match if cli.no_session {
        SessionStore::open_in_memory()
    } else {
        SessionStore::open(default_db_path())
    } {
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

#[cfg(test)]
mod session_flag_tests {
    use super::*;

    fn cli() -> Cli {
        Cli {
            print: None,
            model: None,
            resume: false,
            continue_session: false,
            session: None,
            name: None,
            fork: None,
            no_session: false,
            acp: false,
            trust_project: false,
            allow: Vec::new(),
            tui_mode: None,
        }
    }

    fn summaries() -> Vec<ion_core::SessionSummary> {
        let id = |s: &str| ion_core::SessionId::parse(s).expect("fixed test uuid");
        vec![
            ion_core::SessionSummary {
                id: id("11111111-1111-1111-1111-111111111111"),
                title: "first".to_owned(),
                updated_at: 1,
                entry_count: 1,
            },
            ion_core::SessionSummary {
                id: id("11111111-2222-2222-2222-222222222222"),
                title: "second".to_owned(),
                updated_at: 2,
                entry_count: 1,
            },
        ]
    }

    #[test]
    fn empty_flags_validate() {
        assert!(validate_session_flags(&cli()).is_ok());
    }

    #[test]
    fn fork_rejects_session_selection_flags() {
        let mut c = cli();
        c.fork = Some("abc".to_owned());
        assert!(validate_session_flags(&c).is_ok());
        c.session = Some("abc".to_owned());
        assert!(validate_session_flags(&c).is_err());
        c.session = None;
        c.resume = true;
        assert!(validate_session_flags(&c).is_err());
    }

    #[test]
    fn no_session_rejects_everything_session_shaped() {
        let mut c = cli();
        c.no_session = true;
        assert!(validate_session_flags(&c).is_ok());
        c.name = Some("ephemeral".to_owned());
        assert!(
            validate_session_flags(&c).is_ok(),
            "naming an ephemeral run is fine"
        );
        c.fork = Some("abc".to_owned());
        assert!(validate_session_flags(&c).is_err());
    }

    #[test]
    fn name_rejected_when_opening_existing_sessions() {
        let mut c = cli();
        c.name = Some("x".to_owned());
        assert!(validate_session_flags(&c).is_ok());
        c.resume = true;
        assert!(validate_session_flags(&c).is_err());
        c.resume = false;
        c.session = Some("abc".to_owned());
        assert!(validate_session_flags(&c).is_err());
    }

    #[test]
    fn session_match_prefers_exact_then_unique_prefix() {
        let all = summaries();
        let full = "11111111-1111-1111-1111-111111111111";
        let expect = ion_core::SessionId::parse(full).unwrap();
        assert_eq!(match_session_id(&all, full).unwrap(), expect);
        assert_eq!(
            match_session_id(&all, &format!("session-{full}")).unwrap(),
            expect
        );
        assert_eq!(match_session_id(&all, "11111111-1111").unwrap(), expect);
    }

    #[test]
    fn session_match_reports_unknown_and_ambiguous() {
        let all = summaries();
        assert!(match_session_id(&all, "9999").is_err());
        let err = match_session_id(&all, "11111111").unwrap_err();
        assert!(err.contains("ambiguous"), "unexpected: {err}");
    }
}
