//! Local CLI and terminal host for the same coding Agent and Session.
use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context, Result, bail, ensure};
use clap::{Parser, Subcommand};
use ion_ai::ModelRef;
use ion_core::{CodingAgent, CodingAgentError, CodingAgentEvent, CodingSession};
use ion_host::{CredentialStatus, Host, SavedSelection, Wire};
use serde_json::json;
use tokio_util::sync::CancellationToken;

mod terminal_client;

#[derive(Parser)]
#[command(about = "Ion: a local coding agent")]
struct Cli {
    /// Run one prompt headlessly, then exit.
    #[arg(short = 'p', long = "print", global = true)]
    print: Option<String>,
    /// Emit JSONL progress records for a headless prompt.
    #[arg(long, global = true)]
    json: bool,
    /// Working directory for a new session (defaults to the current directory).
    #[arg(long, global = true)]
    cwd: Option<PathBuf>,
    /// Session database path or ID; new work starts a fresh session by default.
    #[arg(long, global = true, conflicts_with = "continue_session")]
    session: Option<PathBuf>,
    /// Continue the most recently active session in this working directory.
    #[arg(short = 'c', long = "continue", global = true)]
    continue_session: bool,
    /// Provider for this invocation; use with --model.
    #[arg(long, global = true)]
    provider: Option<String>,
    /// Exact model ID for this invocation; use with --provider.
    #[arg(long, global = true)]
    model: Option<String>,
    #[command(subcommand)]
    action: Option<Action>,
}

#[derive(Subcommand)]
enum Action {
    /// List models Ion can route to; account access may differ.
    Models { query: Option<String> },
    /// Select a catalog model, or configure a custom compatible endpoint.
    Use {
        provider: String,
        model: String,
        #[arg(long)]
        endpoint: Option<String>,
        #[arg(long, value_enum)]
        wire: Option<Wire>,
        #[arg(long)]
        api_key_env: Option<String>,
    },
    /// Save a provider API key entered at a masked terminal prompt.
    Login { provider: String },
    /// Remove a saved credential; an environment key stays active.
    Logout { provider: String },
    /// Show credential sources without displaying secrets.
    Auth,
    /// List saved sessions for the working directory.
    Sessions,
    /// Submit one prompt and print the committed final answer.
    Run { prompt: String },
    /// Open the terminal chat client.
    Chat,
    /// Inspect committed Session history without running a model or tool.
    Inspect,
    /// Copy a saved conversation into an independent Session in this directory.
    Clone,
    /// Summarize settled history for continued work, retaining the raw log.
    Compact,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run_cli(Cli::parse()).await {
        eprintln!("ion: {error:#}");
        std::process::exit(1);
    }
}

async fn run_cli(cli: Cli) -> Result<()> {
    if cli.json
        && (!matches!(&cli.action, Some(Action::Run { .. }) | None)
            || cli.action.is_none() && cli.print.is_none())
    {
        bail!("--json requires `run PROMPT` or `--print PROMPT`");
    }
    let host = Arc::new(Host::from_environment()?);
    let credentials = host.credentials();
    let models = host.models();
    match cli.action {
        Some(_) if cli.print.is_some() => bail!("--print cannot be combined with a subcommand"),
        Some(Action::Models { query }) => {
            let query = query.unwrap_or_default().to_ascii_lowercase();
            for choice in models.choices(credentials)? {
                let model = choice.selected;
                if !model.provider.contains(&query)
                    && !model.model.to_ascii_lowercase().contains(&query)
                    && !choice.label.to_ascii_lowercase().contains(&query)
                {
                    continue;
                }
                println!(
                    "{}/{}\t{}\t{}",
                    model.provider,
                    model.model,
                    choice.label,
                    status_label(choice.credential)
                );
            }
            Ok(())
        }
        Some(Action::Use {
            provider,
            model,
            endpoint,
            wire,
            api_key_env,
        }) => {
            let saved = SavedSelection {
                provider,
                model,
                endpoint,
                wire,
                api_key_env,
            };
            let resolved = models.save_default(&saved)?;
            println!("Selected {}/{}", resolved.provider, resolved.model);
            Ok(())
        }
        Some(Action::Login { provider }) => {
            ensure!(
                io::stdin().is_terminal(),
                "API-key login requires a terminal; use an environment variable in headless mode"
            );
            let key = rpassword::prompt_password(format!("{provider} API key: "))?;
            credentials.save_api_key(&provider, &key)?;
            println!("Saved {provider} credential.");
            Ok(())
        }
        Some(Action::Logout { provider }) => {
            credentials.remove(&provider)?;
            println!("Removed saved {provider} credential. Any environment key remains active.");
            Ok(())
        }
        Some(Action::Auth) => {
            let mut seen = BTreeSet::new();
            for choice in models.choices(credentials)? {
                let model = choice.selected;
                if seen.insert(model.provider.clone()) {
                    println!("{}: {}", model.provider, status_label(choice.credential));
                }
            }
            Ok(())
        }
        Some(Action::Sessions) => {
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
            let catalog = host.sessions(cwd);
            for session in catalog.list()? {
                println!(
                    "{}\t{}\t{} turn(s)\t{}\t{}",
                    session.id,
                    session.name.as_deref().unwrap_or(""),
                    session.turns,
                    session
                        .model
                        .as_ref()
                        .map_or_else(String::new, |model| format!(
                            "{}/{}",
                            model.provider, model.model
                        )),
                    session.preview.as_deref().unwrap_or("")
                );
            }
            Ok(())
        }
        action => {
            let explicit_cwd = cli.cwd.is_some();
            let cwd = cli.cwd.unwrap_or(std::env::current_dir()?).canonicalize()?;
            ensure!(cwd.is_dir(), "working directory is not a directory");
            let catalog = host.sessions(cwd.clone());
            let path = if let Some(explicit) = cli.session {
                catalog.resolve_explicit(explicit)?
            } else if cli.continue_session {
                catalog.latest()?
            } else if matches!(
                action,
                Some(Action::Inspect | Action::Compact | Action::Clone)
            ) {
                bail!("use --continue or --session ID; run `ion sessions` to find one")
            } else {
                catalog.new_path()?
            };
            let existing = if path.is_file() {
                Some(CodingSession::inspect(&path)?)
            } else {
                None
            };
            if let Some(view) = &existing {
                ensure!(
                    !explicit_cwd || view.cwd == cwd,
                    "--cwd does not match the session's working directory ({})",
                    view.cwd.display()
                );
            }
            if matches!(action, Some(Action::Inspect)) {
                let view = existing.context("session does not exist")?;
                println!("{}", serde_json::to_string_pretty(&view)?);
                return Ok(());
            }
            if matches!(action, Some(Action::Clone)) {
                ensure!(existing.is_some(), "session does not exist");
                let source = CodingSession::open(&path)?;
                let target = catalog.new_path()?;
                let cloned = source.clone_to(&target)?;
                let id = cloned
                    .path()
                    .file_stem()
                    .context("cloned session has no ID")?;
                println!("Cloned session as {}", id.to_string_lossy());
                return Ok(());
            }
            if matches!(action, Some(Action::Compact)) {
                ensure!(existing.is_some(), "session does not exist");
            }
            let previous = existing.and_then(|view| view.last_model);
            let selected = models.choose(cli.provider, cli.model, previous, credentials)?;
            selected.require_access(credentials)?;
            let model = selected.identity();
            let session = Arc::new(if path.is_file() {
                CodingSession::open(&path)?
            } else {
                CodingSession::create(&path, &cwd)?
            });
            let agent = host.agent(&session, &selected)?;
            let instructions = host.instructions(session.cwd())?;
            match action {
                Some(Action::Run { prompt }) => {
                    headless(
                        session,
                        agent,
                        model,
                        instructions,
                        with_piped_input(prompt)?,
                        cli.json,
                    )
                    .await
                }
                Some(Action::Compact) => {
                    let stop = CancellationToken::new();
                    let signal_stop = stop.clone();
                    let signal = tokio::spawn(async move {
                        if tokio::signal::ctrl_c().await.is_ok() {
                            signal_stop.cancel();
                        }
                    });
                    let result = agent.compact(&session, model, stop, |_| {}).await;
                    signal.abort();
                    if result? {
                        println!("Context summarized; raw Session history retained");
                    } else {
                        println!("No settled history to summarize");
                    }
                    Ok(())
                }
                Some(Action::Clone) => unreachable!("clone handled before model selection"),
                Some(Action::Chat) | None if cli.print.is_none() => {
                    terminal_client::chat(session, agent, selected, instructions, catalog, host)
                        .await
                }
                None => {
                    headless(
                        session,
                        agent,
                        model,
                        instructions,
                        with_piped_input(cli.print.expect("matched Some"))?,
                        cli.json,
                    )
                    .await
                }
                _ => unreachable!("non-agent action handled above"),
            }
        }
    }
}

fn status_label(status: CredentialStatus) -> &'static str {
    match status {
        CredentialStatus::Environment => "environment key",
        CredentialStatus::Saved => "saved login",
        CredentialStatus::Missing => "no credential",
    }
}

async fn headless(
    session: Arc<CodingSession>,
    agent: Arc<CodingAgent>,
    model: ModelRef,
    instructions: String,
    prompt: String,
    json_output: bool,
) -> Result<()> {
    let id = session
        .path()
        .file_stem()
        .context("session has no ID")?
        .to_string_lossy();
    if json_output {
        write_json_record(&json!({"type":"session","id":id,"cwd":session.cwd()}))?;
    } else {
        eprintln!("[session: {id}]");
    }
    let stop = CancellationToken::new();
    let output_stop = stop.clone();
    let signal_stop = stop.clone();
    let signal = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal_stop.cancel();
        }
    });
    let mut output_error = None;
    let result = agent
        .submit(
            &session,
            model,
            prompt,
            instructions,
            stop,
            |event| {
                if json_output {
                    let record = match event {
                        CodingAgentEvent::TextDelta(text) => {
                            json!({"type":"text_delta","text":text})
                        }
                        CodingAgentEvent::ProviderRetry { attempt, max_retries, delay_ms } => {
                            json!({"type":"provider_retry","attempt":attempt,"max_retries":max_retries,"delay_ms":delay_ms})
                        }
                        CodingAgentEvent::ToolStarted {
                            call_id,
                            name,
                            arguments,
                        } => json!({"type":"tool_started","call_id":call_id,"name":name,"arguments":arguments}),
                        CodingAgentEvent::ToolFinished {
                            call_id,
                            name,
                            output,
                        } => json!({"type":"tool_finished","call_id":call_id,"name":name,"output":output.value,"is_error":output.is_error}),
                        CodingAgentEvent::ToolRejected {
                            call_id,
                            name,
                            output,
                        } => json!({"type":"tool_rejected","call_id":call_id,"name":name,"output":output.value,"is_error":output.is_error}),
                        CodingAgentEvent::InterruptedCalls(count) => {
                            json!({"type":"interrupted_calls","count":count})
                        }
                        CodingAgentEvent::ContextCompacted { through_entry } => {
                            json!({"type":"context_compacted","through_entry":through_entry})
                        }
                        CodingAgentEvent::ResponseRestarted => json!({"type":"response_restarted"}),
                        CodingAgentEvent::Final(text) => json!({"type":"final","text":text}),
                    };
                    if output_error.is_none()
                        && let Err(error) = write_json_record(&record)
                    {
                        output_error = Some(error);
                        output_stop.cancel();
                    }
                    return;
                }
                match event {
                CodingAgentEvent::TextDelta(_) => {}
                CodingAgentEvent::ProviderRetry { attempt, max_retries, delay_ms } => {
                    eprintln!("[provider retry {attempt}/{max_retries} in {delay_ms}ms]")
                }
                CodingAgentEvent::ToolStarted { name, .. } => eprintln!("[tool: {name}]"),
                CodingAgentEvent::ToolFinished { name, output, .. } => {
                    eprintln!("[tool: {name}] {}", output.value)
                }
                CodingAgentEvent::ToolRejected { name, output, .. } => {
                    eprintln!("[tool skipped: {name}] {}", output.value)
                }
                CodingAgentEvent::InterruptedCalls(count) => {
                    eprintln!("[recovered {count} incomplete tool call(s); effects unknown]")
                }
                CodingAgentEvent::ContextCompacted { through_entry } => {
                    eprintln!("[context summarized through entry {through_entry}]")
                }
                CodingAgentEvent::ResponseRestarted => {
                    eprintln!("[incomplete response discarded; retrying]");
                }
                CodingAgentEvent::Final(_) => {}
                }
            },
        )
        .await;
    signal.abort();
    if json_output {
        if let Some(error) = output_error {
            return Err(error.into());
        }
        match &result {
            Ok(_) => write_json_record(&json!({"type":"run_end","status":"completed"}))?,
            Err(CodingAgentError::Cancelled) => write_json_record(
                &json!({"type":"run_end","status":"cancelled","error":"turn was cancelled"}),
            )?,
            Err(error) => write_json_record(
                &json!({"type":"run_end","status":"failed","error":error.to_string()}),
            )?,
        }
        result?;
        return Ok(());
    }
    let answer = result?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{answer}")?;
    stdout.flush()?;
    Ok(())
}

fn write_json_record(value: &serde_json::Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    let mut stdout = io::stdout().lock();
    stdout.write_all(&bytes)?;
    stdout.flush()
}

fn with_piped_input(prompt: String) -> Result<String> {
    if io::stdin().is_terminal() {
        return Ok(prompt);
    }
    const MAX_STDIN: u64 = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    io::stdin().take(MAX_STDIN + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_STDIN, "piped stdin exceeds 8 MiB");
    if bytes.is_empty() {
        return Ok(prompt);
    }
    let input = String::from_utf8(bytes).context("piped stdin is not UTF-8")?;
    Ok(format!("{input}\n{prompt}"))
}
