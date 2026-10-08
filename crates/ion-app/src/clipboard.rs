//! Clipboard output for the terminal client.
use std::{io::Write, process::Stdio};

use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use ion_terminal::TerminalSession;
use tokio::{
    io::AsyncWriteExt,
    process::Command,
    time::{Duration, timeout},
};

const OSC52_LIMIT: usize = 100_000;

pub enum CopyOutcome {
    Copied,
    RequestedFromTerminal,
}

pub async fn copy(text: &str, terminal: &mut TerminalSession) -> Result<CopyOutcome> {
    let mut commands: Vec<(&str, &[&str])> = Vec::new();
    if cfg!(target_os = "macos") {
        commands.push(("pbcopy", &[]));
    } else if cfg!(target_os = "linux") {
        if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            commands.push(("wl-copy", &[]));
        }
        if std::env::var_os("DISPLAY").is_some() {
            commands.push(("xclip", &["-selection", "clipboard"]));
            commands.push(("xsel", &["--clipboard", "--input"]));
        }
    }
    for (program, args) in commands {
        if try_command(program, args, text).await {
            return Ok(CopyOutcome::Copied);
        }
    }
    let remote = std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_CLIENT").is_some()
        || std::env::var_os("MOSH_CONNECTION").is_some();
    let display =
        std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    if remote || (cfg!(target_os = "linux") && !display) {
        let encoded = STANDARD.encode(text);
        if encoded.len() > OSC52_LIMIT {
            bail!("answer exceeds the terminal clipboard request size limit");
        }
        let mut output = terminal.output()?;
        output.write_all(format!("\x1b]52;c;{encoded}\x07").as_bytes())?;
        output.flush()?;
        return Ok(CopyOutcome::RequestedFromTerminal);
    }
    bail!("clipboard is unavailable in this environment")
}

async fn try_command(program: &str, args: &[&str], text: &str) -> bool {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    let Some(mut stdin) = child.stdin.take() else {
        return false;
    };
    if !matches!(
        timeout(Duration::from_secs(5), stdin.write_all(text.as_bytes())).await,
        Ok(Ok(()))
    ) {
        let _ = child.kill().await;
        return false;
    }
    drop(stdin);
    match timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        _ => {
            let _ = child.kill().await;
            false
        }
    }
}
