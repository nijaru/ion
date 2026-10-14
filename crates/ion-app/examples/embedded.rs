//! A single-prompt SDK host using Ion's shared binding and owned operation.

use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use clap::Parser;
use ion_core::CodingSession;
use ion_host::{Host, SessionBinding};
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
    let session = Arc::new(if args.session.try_exists()? {
        CodingSession::open(&args.session)?
    } else {
        CodingSession::create(&args.session, &cwd)?
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

async fn submit(args: Args, cwd: &Path, session: Arc<CodingSession>) -> Result<()> {
    ensure!(
        session.cwd() == cwd,
        "Session belongs to a different working directory"
    );
    let host = Arc::new(Host::from_environment()?);
    let selected = host.models().choose(
        Some(args.provider),
        Some(args.model),
        None,
        host.credentials(),
    )?;
    // This example connects no MCP servers and restores the saved reasoning
    // preference instead of overriding it. There is no client-owned agent loop.
    let binding = SessionBinding::new(host, session.clone(), selected, None, None)?;
    let mut operation = binding.agent().submit(
        session,
        args.prompt,
        binding.instructions().to_owned(),
        CancellationToken::new(),
        |_| {}, // Observers must not block, including on stdout publication.
    );
    let answer = tokio::select! {
        result = &mut operation => result.map_err(anyhow::Error::from),
        signal = tokio::signal::ctrl_c() => {
            operation.request_stop();
            // Even a signal-registration error cannot discard started work.
            let result = operation.await.map_err(anyhow::Error::from);
            match (signal, result) {
                (Ok(()), result) => result,
                (Err(signal), Ok(_)) => Err(signal).context("listen for Ctrl-C"),
                (Err(signal), Err(error)) => {
                    Err(error.context(format!("Ctrl-C listener also failed: {signal}")))
                }
            }
        }
    }?;

    // Only the committed answer is published. Failure is not retry authority;
    // the outer caller still shuts down before returning a publication error.
    let mut output = std::io::stdout().lock();
    writeln!(output, "{answer}").context("publish committed answer")?;
    output.flush().context("flush committed answer")
}
