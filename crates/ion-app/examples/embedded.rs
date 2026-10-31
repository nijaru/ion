//! A single-prompt SDK client using Ion's shared session-control owner.

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use ion_core::Session;
use ion_host::{
    ControlUpdate, Delivery, Host, OperationOutcome, ReturnReason, SessionControl, UserInput,
};

#[derive(Parser)]
#[command(about = "Submit one prompt through Ion's embedded SDK; Ctrl-C stops and joins")]
struct Args {
    #[arg(long, default_value = ".")]
    cwd: PathBuf,
    #[arg(long)]
    session: PathBuf,
    #[arg(long)]
    provider: String,
    #[arg(long)]
    model: String,
    prompt: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let cwd = args
        .cwd
        .canonicalize()
        .context("resolve working directory")?;
    let session = Arc::new(if args.session.try_exists()? {
        Session::open(&args.session)?
    } else {
        Session::create(&args.session, Some(&cwd))?
    });

    // Keep every fallible step after opening the Session inside this outcome.
    // `?` must not destroy the runtime before shutdown joins retained work.
    let outcome = submit(args, &cwd, session.clone()).await;
    let shutdown = session.shutdown().await;
    match (outcome, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(error), Err(shutdown)) => {
            Err(error.context(format!("Session shutdown also failed: {shutdown}")))
        }
    }
}

async fn submit(args: Args, cwd: &Path, session: Arc<Session>) -> Result<()> {
    ensure!(
        session.cwd() == Some(cwd),
        "Session belongs to a different working directory"
    );
    let host = Arc::new(Host::from_environment()?);
    let selected = host.models().choose(
        Some(ion_ai::ModelRef {
            provider: args.provider,
            model: args.model,
        }),
        None,
        host.credentials(),
    )?;
    // This example connects no MCP servers and restores the saved reasoning
    // preference instead of overriding it. There is no client-owned agent loop.
    let resources = host.resources(
        session
            .cwd()
            .context("coding requires a working directory")?,
    )?;
    let setup = ion_host::CodingApplication::new(host, None)
        .setup(session, selected, None, None, resources)?;
    let mut control = setup.into_control::<()>();
    let application_owner = control.owner();
    let mut outcome = prompt(&mut control, args.prompt).await;
    if let Some(owner) = &application_owner {
        owner.request_shutdown();
    }
    let report = control.shutdown().await;
    if let Some(owner) = &application_owner
        && let Err(error) = owner.join_shutdown().await
    {
        outcome = Err(match outcome {
            Ok(_) => error,
            Err(primary) => primary.context(format!("application shutdown also failed: {error}")),
        });
    }
    for error in report.cleanup_errors {
        outcome = match outcome {
            Ok(_) => Err(error.into()),
            Err(primary) => Err(primary.context(format!("control shutdown also failed: {error}"))),
        };
    }
    // No blocking publisher can delay joining application-owned work.
    let answer = outcome?;
    let mut output = std::io::stdout().lock();
    writeln!(output, "{answer}").context("publish committed answer")?;
    output.flush().context("flush committed answer")
}

async fn prompt(control: &mut SessionControl, text: String) -> Result<String> {
    // Registration is fallible before admission, not a detached listener task.
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                text,
                images: Vec::new(),
                files: Vec::new(),
            },
            (),
        )
        .map_err(|rejected| rejected.reason)?;
    let mut listening = true;
    let mut rejection = None;
    let answer = loop {
        tokio::select! {
            _ = signal.recv(), if listening => {
                listening = false;
                control.request_stop();
            }
            update = control.next() => match update {
                ControlUpdate::Settled(settlement) => break match settlement.outcome {
                    OperationOutcome::Turn(result) => result.map_err(Into::into),
                    _ => unreachable!("this example only submits coding input"),
                },
                ControlUpdate::Returned(input) => rejection = Some(match input.reason {
                    ReturnReason::Preparation(error) => error,
                    ReturnReason::Stopped => ion_core::AgentError::Cancelled.into(),
                    reason => anyhow::anyhow!("input not accepted: {reason:?}"),
                }),
                ControlUpdate::Idle => break Err(rejection.unwrap_or_else(|| anyhow::anyhow!("operation ended without settlement"))),
                _ => {} // Passive progress never performs blocking publication.
            }
        }
    };
    if answer.is_ok()
        && let Some(owner) = control.owner()
    {
        tokio::select! {
            _ = signal.recv(), if listening => {
                owner.request_shutdown();
                owner.join_shutdown().await?;
                return Err(ion_core::AgentError::Cancelled.into());
            }
            result = owner.background().drain() => result?,
        }
    }
    answer
}
