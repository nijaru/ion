//! A single-prompt SDK client using Ion's shared session-control owner.

use std::{io::Write, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use clap::Parser;
use ion_app::{
    CodingApplication, ControlUpdate, Delivery, Host, OperationOutcome, ReturnReason,
    SessionControl, SessionOptions, SessionTarget, UserInput,
};
use tokio_util::sync::CancellationToken;

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
    let host = Arc::new(Host::from_environment()?);
    let selected = host.models().choose(
        Some(ion_ai::ModelRef {
            provider: args.provider,
            model: args.model,
        }),
        None,
        host.credentials(),
    )?;
    let resources = host.resources(&cwd)?;
    let application = CodingApplication::new(host, None);
    let mut preparation = application.open_session(
        SessionTarget::OpenOrCreate(args.session),
        cwd,
        selected,
        SessionOptions::default(),
        resources,
        CancellationToken::new(),
    );
    let setup = preparation.join().await?;
    let mut control = setup.into_application_control::<()>();
    let mut outcome = prompt(&mut control, args.prompt).await;
    match control.begin_close().join().await {
        Ok(report) => {
            for error in report.cleanup_errors {
                outcome = cleanup(outcome, error.into());
            }
        }
        Err(error) => outcome = cleanup(outcome, error),
    }
    let answer = outcome?;
    // A blocking publisher cannot delay joining application-owned work.
    let mut output = std::io::stdout().lock();
    writeln!(output, "{answer}").context("publish committed answer")?;
    output.flush().context("flush committed answer")
}

fn cleanup<T>(outcome: Result<T>, error: anyhow::Error) -> Result<T> {
    match outcome {
        Ok(_) => Err(error),
        Err(primary) => Err(primary.context(format!("shutdown also failed: {error:#}"))),
    }
}

async fn prompt(control: &mut SessionControl, text: String) -> Result<String> {
    // Registration is fallible before admission, not a detached listener task.
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                text,
                attachments: Vec::new(),
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
                    ReturnReason::PredecessorFailed => anyhow::anyhow!("input was not attempted because preceding work failed"),
                    reason => anyhow::anyhow!("input not accepted: {reason:?}"),
                }),
                ControlUpdate::Unresolved(input) => {
                    rejection = Some(anyhow::anyhow!("input commit unresolved in Session {}; inspect before retrying", input.session));
                    control.request_shutdown();
                }
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
                control.request_shutdown();
                return Err(ion_core::AgentError::Cancelled.into());
            }
            result = owner.background().drain() => result?,
        }
    }
    answer
}
