//! One-shot, secret-safe native helpers retained through cancellation and cleanup.
use std::process::Stdio;

use anyhow::{Result, bail, ensure};
use rustix::process::Pid;
use tokio::{io::AsyncReadExt, process::Command, time::Instant};
use tokio_util::sync::CancellationToken;

pub(super) async fn capture(
    command: Command,
    limit: usize,
    deadline: Instant,
    stop: CancellationToken,
) -> Result<String> {
    let mut preparation = crate::preparation::Preparation::start(stop.child_token(), move |stop| {
        run(command, limit, deadline, stop)
    });
    preparation.join().await
}

async fn run(
    mut command: Command,
    limit: usize,
    deadline: Instant,
    stop: CancellationToken,
) -> Result<String> {
    ensure!(!stop.is_cancelled(), "MCP helper cancelled before start");
    ensure!(Instant::now() < deadline, "MCP helper phase timed out");
    let mut exits = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())
        .map_err(|_| anyhow::anyhow!("cannot observe MCP helper exit"))?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // Credentials must never be logged or spooled as diagnostic data.
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|_| anyhow::anyhow!("cannot start MCP helper"))?;
    let pid = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .and_then(Pid::from_raw)
        .expect("spawned unreaped helper has a positive Unix pid");
    let mut stdout = child.stdout.take().expect("helper stdout is piped");
    let result = {
        let exchange = async {
            let read = async {
                let mut bytes = Vec::new();
                (&mut stdout)
                    .take((limit + 1) as u64)
                    .read_to_end(&mut bytes)
                    .await
                    .map_err(|_| anyhow::anyhow!("cannot read MCP helper output"))?;
                ensure!(bytes.len() <= limit, "MCP helper output exceeds byte limit");
                Ok::<_, anyhow::Error>(bytes)
            };
            let exit = async {
                crate::local_tools::command::observe_exit(pid, &mut exits)
                    .await
                    .map_err(|_| anyhow::anyhow!("cannot observe MCP helper exit"))
            };
            let (bytes, success) = tokio::try_join!(read, exit)?;
            ensure!(success, "MCP helper exited unsuccessfully");
            let value = String::from_utf8(bytes)
                .map_err(|_| anyhow::anyhow!("MCP helper output is not UTF-8"))?;
            let value = value.trim();
            ensure!(!value.is_empty(), "MCP helper output is empty");
            ensure!(!value.contains('\0'), "invalid MCP helper output");
            Ok(value.to_owned())
        };
        tokio::select! {
            biased;
            () = stop.cancelled() => Err(anyhow::anyhow!("MCP helper cancelled")),
            () = tokio::time::sleep_until(deadline) => Err(anyhow::anyhow!("MCP helper phase timed out")),
            result = exchange => result,
        }
    };
    // WNOWAIT reserves the root PID until group termination, even if a helper
    // leaves descendants with closed pipes. No signal follows a reaped PID.
    crate::local_tools::command::kill_child(&mut child, Some(pid))
        .await
        .map_err(|_| anyhow::anyhow!("cannot join MCP helper"))?;
    if stop.is_cancelled() {
        bail!("MCP helper cancelled before publication");
    }
    ensure!(Instant::now() < deadline, "MCP helper phase timed out");
    result
}

#[cfg(test)]
mod tests;
