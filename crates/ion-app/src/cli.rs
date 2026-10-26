//! Command-line grammar and normalization, before any Host services are opened.
use std::{
    io::{self, IsTerminal, Read},
    path::PathBuf,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{
    Arg, ArgAction, ArgMatches, Command, FromArgMatches, Subcommand, builder::TypedValueParser,
    parser::ValueSource,
};
use ion_ai::{ModelRef, Reasoning};
use ion_core::ToolSelection;
use ion_host::{McpExposure, Wire};

use crate::terminal_client::TuiMode;

pub(crate) struct Invocation {
    pub action: Action,
    pub cwd: Option<PathBuf>,
    pub session: Option<PathBuf>,
    pub continue_session: bool,
    pub resume_session: bool,
    pub model: Option<ModelRef>,
    pub reasoning: Option<Reasoning>,
    pub image: Vec<PathBuf>,
    pub tui_mode: TuiMode,
    pub json: bool,
    pub completion_delivery: ion_core::CompletionDelivery,
    pub code_mode: bool,
    pub read_only: bool,
    pub tool_selection: ToolSelection,
    /// Literal pipe input, captured before Session or tool admission.
    pub piped_input: String,
}

#[derive(Subcommand)]
pub(crate) enum Action {
    /// Private clipboard transfer helper; never initializes model or Session services.
    #[command(name = "__clipboard-read", hide = true)]
    ClipboardRead,
    /// Print license notices for bundled syntax definitions (no model or Session setup).
    SyntaxLicenses,
    /// List models Ion can route to; account access may differ.
    Models { query: Option<String> },
    /// Select a catalog model, or configure a custom compatible endpoint.
    Use {
        /// Catalog provider ID, or a label for a custom provider.
        #[arg(id = "selected_provider", value_name = "PROVIDER")]
        provider: String,
        /// Exact model ID, including any namespace.
        #[arg(id = "selected_model", value_name = "MODEL")]
        model: String,
        /// Full HTTP endpoint URL; requires --wire for a custom route.
        #[arg(long)]
        endpoint: Option<String>,
        /// Request/response protocol for the custom endpoint.
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(
            Wire::VALUES.iter().map(|(name, _)| *name)
        ).map(|value| value.parse::<Wire>().expect("listed wire format is valid")))]
        wire: Option<Wire>,
        /// Environment variable supplying the custom endpoint API key.
        #[arg(long)]
        api_key_env: Option<String>,
        /// Context capacity in tokens for a custom endpoint's model.
        #[arg(long)]
        context_window: Option<u32>,
        /// Maximum output tokens for a custom endpoint's model.
        #[arg(long)]
        max_output_tokens: Option<u32>,
        /// Declare that a custom endpoint's model accepts image input.
        #[arg(long)]
        images: bool,
    },
    /// Save a provider API key entered at a masked terminal prompt.
    Login {
        #[arg(id = "selected_provider", value_name = "PROVIDER")]
        provider: String,
    },
    /// Remove a saved credential; an environment key stays active.
    Logout {
        #[arg(id = "selected_provider", value_name = "PROVIDER")]
        provider: String,
    },
    /// Show credential sources without displaying secrets.
    Auth,
    /// Inspect explicit executable user commands.
    Commands {
        #[command(subcommand)]
        action: Option<CommandsAction>,
    },
    /// Trust the current project's MCP configuration for its current contents.
    Trust,
    /// Remove trust for the current project's MCP configuration.
    Untrust,
    /// List saved sessions for the working directory.
    Sessions,
    /// List skills and prompt templates available in the working directory.
    Resources,
    /// Submit one prompt and print the committed final answer.
    Run { prompt: Option<String> },
    /// Open terminal chat, optionally starting with a prompt.
    Chat { prompt: Option<String> },
    /// Configure explicitly connected MCP tool servers.
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Control a persistent coding session over JSONL stdin/stdout.
    Rpc,
    /// Inspect committed Session history without running a model or tool.
    Inspect,
    /// Show recorded Session token usage, not account billing or quota.
    Usage,
    /// Print a readable committed transcript, or save it to a new file.
    Export { path: Option<PathBuf> },
    /// List numbered Turns in a saved Session.
    Turns,
    /// Copy a saved conversation into an independent Session in this directory.
    Clone,
    /// Fork a saved Session before a Turn, or after it with --after.
    Fork {
        turn: u64,
        #[arg(long)]
        after: bool,
    },
    /// Summarize settled history for continued work, retaining the raw log.
    Compact,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum ExposureArg {
    Direct,
    Deferred,
    Hidden,
}

impl From<ExposureArg> for McpExposure {
    fn from(value: ExposureArg) -> Self {
        match value {
            ExposureArg::Direct => Self::Direct,
            ExposureArg::Deferred => Self::Deferred,
            ExposureArg::Hidden => Self::Hidden,
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum CommandsAction {
    /// List enabled commands and origin; untrusted project entries are inactive.
    List,
    /// Trust the current project commands manifest for its exact contents.
    Trust,
    /// Remove trust for the current project commands manifest.
    Untrust,
}

#[derive(Subcommand)]
pub(crate) enum McpAction {
    List,
    Add {
        name: String,
        command: String,
        /// Write to the project `.ion/mcp.json` instead of user configuration.
        #[arg(long)]
        project: bool,
        #[arg(long, value_enum, default_value = "direct")]
        exposure: ExposureArg,
        /// Arguments passed to the server; use -- to separate overlapping Ion flags.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    AddHttp {
        name: String,
        url: String,
        #[arg(long)]
        bearer_token_env: Option<String>,
        #[arg(long)]
        project: bool,
        #[arg(long, value_enum, default_value = "direct")]
        exposure: ExposureArg,
    },
    Remove {
        name: String,
        #[arg(long)]
        project: bool,
    },
}

/// Each option has one definition. Applicability owns both help and validation of
/// options placed before a command; no option is globally inherited.
fn options() -> Vec<Arg> {
    vec![
        Arg::new("print").short('p').long("print").action(ArgAction::SetTrue)
            .help("Submit a prompt or piped stdin headlessly, then exit"),
        Arg::new("json").long("json").action(ArgAction::SetTrue)
            .help("Emit JSONL progress records for a headless prompt"),
        Arg::new("completion_delivery").long("completion-delivery")
            .value_parser(clap::builder::PossibleValuesParser::new(ion_core::CompletionDelivery::ALL.map(ion_core::CompletionDelivery::as_str)).map(|value| value.parse::<ion_core::CompletionDelivery>().expect("listed delivery is valid")))
            .value_name("POLICY").help("Managed-work readiness: inbox (default), active continue, or automatic idle wake"),
        Arg::new("code_mode").long("code-mode").action(ArgAction::SetTrue)
            .help("Enable bounded JavaScript composition alongside direct tools"),
        Arg::new("tools").long("tools").action(ArgAction::Append).value_delimiter(',')
            .value_name("PATTERNS").help("Limit visible tools to comma-separated names or * patterns"),
        Arg::new("exclude_tools").long("exclude-tools").action(ArgAction::Append).value_delimiter(',')
            .value_name("PATTERNS").help("Remove visible tools by comma-separated names or * patterns"),
        Arg::new("read_only").long("read-only").action(ArgAction::SetTrue)
            .help("Use read-only tools; exclude mutations, direct shell and managed children"),
        Arg::new("cwd").long("cwd").value_parser(clap::value_parser!(PathBuf))
            .value_name("DIR").help("Working directory (defaults to the current directory)"),
        Arg::new("session").long("session").value_parser(clap::value_parser!(PathBuf))
            .value_name("PATH_OR_ID").help("Open a saved Session; new work starts fresh by default"),
        Arg::new("continue_session").short('c').long("continue").action(ArgAction::SetTrue)
            .help("Continue the most recently active Session in this directory"),
        Arg::new("resume_session").short('r').long("resume").action(ArgAction::SetTrue)
            .help("Choose a saved Session in a terminal; supports current/all folder scope"),
        Arg::new("provider").long("provider").value_name("PROVIDER")
            .help("Provider for an exact --model ID, including namespaced IDs"),
        Arg::new("model").long("model").value_name("PROVIDER/MODEL")
            .help("Exact qualified model identity, or an exact ID with --provider; does not change the saved default"),
        Arg::new("reasoning").long("reasoning").value_parser(clap::value_parser!(Reasoning))
            .value_name("EFFORT").help("Generation effort: default, off, low, medium, high or budget:TOKENS; route support varies"),
        Arg::new("image").long("image").action(ArgAction::Append)
            .value_parser(clap::value_parser!(PathBuf)).value_name("PATH")
            .help("Attach an image to the first prompt (repeatable)"),
        Arg::new("tui_mode").long("tui-mode").value_parser(clap::value_parser!(TuiMode))
            .value_name("MODE").help("Interactive surface: inline native scrollback (default) or fullscreen"),
    ]
}

fn applicable(command: &str, option: &str) -> bool {
    let coding = matches!(command, "run" | "chat" | "rpc");
    let saved = matches!(
        command,
        "inspect" | "usage" | "export" | "turns" | "clone" | "fork" | "compact"
    );
    match option {
        "cwd" => {
            coding
                || saved
                || matches!(
                    command,
                    "sessions" | "resources" | "commands" | "trust" | "untrust" | "mcp"
                )
        }
        "session" | "continue_session" => coding || saved,
        "resume_session" => matches!(command, "chat" | "run") || saved,
        "provider" | "model" | "reasoning" => coding || command == "compact",
        "completion_delivery" | "code_mode" | "tools" | "exclude_tools" | "read_only" => coding,
        "image" => matches!(command, "run" | "chat"),
        "json" => command == "run",
        "tui_mode" => command == "chat",
        _ => false,
    }
}

fn scoped(command: Command, owner: &str, options: &[Arg]) -> Command {
    command
        .args(
            options
                .iter()
                .filter(|arg| applicable(owner, arg.get_id().as_str()))
                .cloned(),
        )
        .mut_subcommands(|child| scoped(child, owner, options))
}

fn command() -> Command {
    let options = options();
    Action::augment_subcommands(
        Command::new("ion")
            .about("Ion: a local coding agent")
            .after_help("Without a command, open chat in a terminal or submit piped stdin headlessly.\nOptions may precede a command, but must apply to that command. See ion COMMAND --help.")
            .args(options.iter().cloned())
            .arg(Arg::new("prompt").value_name("PROMPT")
                .help("Initial prompt; interactive in a terminal, headless with --print or redirected I/O")),
    )
    .mut_subcommands(|child| {
        let owner = child.get_name().to_owned();
        scoped(child, &owner, &options)
    })
}

pub(crate) fn parse() -> Result<Invocation> {
    let mut command = command();
    let matches = command.get_matches_mut();
    let mut invocation = normalize(
        matches,
        io::stdin().is_terminal() && io::stdout().is_terminal(),
    )
    .unwrap_or_else(|error| {
        command
            .error(clap::error::ErrorKind::ValueValidation, error)
            .exit()
    });
    if let Action::Run { prompt } = &invocation.action {
        if !io::stdin().is_terminal() {
            invocation.piped_input = read_pipe(io::stdin().lock())?;
        }
        ensure!(
            !prompt.as_deref().unwrap_or_default().trim().is_empty()
                || !invocation.piped_input.trim().is_empty()
                || !invocation.image.is_empty(),
            "a headless prompt needs text, piped stdin or --image"
        );
    }
    Ok(invocation)
}

fn normalize(mut matches: ArgMatches, interactive: bool) -> Result<Invocation> {
    let explicit = matches.subcommand().map(|(name, _)| name.to_owned());
    let print = matches.remove_one::<bool>("print").unwrap_or(false);
    let prompt = matches.remove_one::<String>("prompt");
    ensure!(
        !print || explicit.is_none(),
        "--print cannot be combined with a subcommand"
    );
    ensure!(
        explicit.is_none() || prompt.is_none(),
        "put the prompt after chat or run, not before a subcommand"
    );
    let action = if explicit.is_some() {
        Action::from_arg_matches(&matches)?
    } else if print || !interactive {
        Action::Run { prompt }
    } else {
        Action::Chat { prompt }
    };
    if let Action::Chat {
        prompt: Some(prompt),
    } = &action
    {
        ensure!(
            prompt.len() <= crate::terminal_client::MAX_DRAFT,
            "interactive initial prompt exceeds 64 KiB"
        );
    }
    let owner = explicit.as_deref().unwrap_or(match action {
        Action::Run { .. } => "run",
        _ => "chat",
    });
    for arg in options() {
        let id = arg.get_id().as_str();
        if id != "print" && supplied(&matches, id) {
            ensure!(
                applicable(owner, id),
                "--{} does not apply to {owner}",
                arg.get_long().expect("shared option has a long name")
            );
        }
    }
    let mut levels = vec![matches];
    while let Some((_, child)) = levels
        .last_mut()
        .expect("root match exists")
        .remove_subcommand()
    {
        levels.push(child);
    }
    let session = one(&mut levels, "session")?;
    let continue_session = one(&mut levels, "continue_session")?.unwrap_or(false);
    ensure!(
        session.is_none() || !continue_session,
        "--session conflicts with --continue"
    );
    let resume_session = one(&mut levels, "resume_session")?.unwrap_or(false);
    ensure!(
        !resume_session || session.is_none() && !continue_session,
        "--resume conflicts with --session and --continue"
    );
    let json = one(&mut levels, "json")?.unwrap_or(false);
    ensure!(
        !resume_session || interactive && !json,
        "--resume requires terminal input and output without --json; use --session ID instead"
    );
    let tools = patterns(many(&mut levels, "tools"))?;
    let exclude = patterns(many(&mut levels, "exclude_tools"))?;
    let read_only = one(&mut levels, "read_only")?.unwrap_or(false);
    ensure!(
        !read_only || tools.is_empty() && exclude.is_empty(),
        "--read-only conflicts with --tools and --exclude-tools"
    );
    let tool_selection = if tools.is_empty() {
        ToolSelection::all()
    } else {
        ToolSelection::include(tools)
    }
    .with_exclude(exclude);
    let provider: Option<String> = one(&mut levels, "provider")?;
    let model: Option<String> = one(&mut levels, "model")?;
    let model = match (provider, model) {
        (None, None) => None,
        (Some(provider), Some(model)) => {
            ensure!(
                !provider.is_empty() && !model.is_empty(),
                "provider and model must not be empty"
            );
            Some(ModelRef { provider, model })
        }
        (None, Some(qualified)) => Some(
            qualified
                .parse()
                .map_err(anyhow::Error::msg)
                .context("use --model PROVIDER/MODEL, or --provider PROVIDER --model MODEL")?,
        ),
        (Some(_), None) => bail!("--provider requires --model"),
    };
    Ok(Invocation {
        action,
        cwd: one(&mut levels, "cwd")?,
        session,
        continue_session,
        resume_session,
        model,
        reasoning: one(&mut levels, "reasoning")?,
        image: many(&mut levels, "image"),
        tui_mode: one(&mut levels, "tui_mode")?.unwrap_or(TuiMode::Inline),
        json,
        completion_delivery: one(&mut levels, "completion_delivery")?.unwrap_or_default(),
        code_mode: one(&mut levels, "code_mode")?.unwrap_or(false),
        read_only,
        tool_selection,
        piped_input: String::new(),
    })
}

fn supplied(matches: &ArgMatches, id: &str) -> bool {
    matches!(matches.try_contains_id(id), Ok(true))
        && matches.value_source(id) == Some(ValueSource::CommandLine)
}

fn one<T: Clone + Send + Sync + 'static>(levels: &mut [ArgMatches], id: &str) -> Result<Option<T>> {
    let mut value = None;
    for level in levels {
        if supplied(level, id) {
            ensure!(
                value.is_none(),
                "option --{} was supplied more than once",
                id.replace('_', "-")
            );
            value = level.remove_one::<T>(id);
        }
    }
    Ok(value)
}

fn many<T: Clone + Send + Sync + 'static>(levels: &mut [ArgMatches], id: &str) -> Vec<T> {
    levels
        .iter_mut()
        .filter(|level| supplied(level, id))
        .flat_map(|level| {
            level
                .remove_many::<T>(id)
                .expect("supplied repeatable option has values")
        })
        .collect()
}

fn patterns(values: Vec<String>) -> Result<Vec<String>> {
    values
        .into_iter()
        .map(|value| {
            let value = value.trim();
            ensure!(!value.is_empty(), "tool pattern is empty");
            Ok(value.to_owned())
        })
        .collect()
}

fn read_pipe(input: impl Read) -> Result<String> {
    const LIMIT: u64 = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    input
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .context("read piped stdin")?;
    ensure!(bytes.len() as u64 <= LIMIT, "piped stdin exceeds 8 MiB");
    String::from_utf8(bytes).context("piped stdin is not UTF-8")
}

pub(crate) fn with_piped_input(prompt: String, piped: String) -> String {
    if piped.is_empty() {
        prompt
    } else if prompt.is_empty() {
        piped
    } else {
        format!("{piped}\n{prompt}")
    }
}

#[cfg(test)]
mod tests;
