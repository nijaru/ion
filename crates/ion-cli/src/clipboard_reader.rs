//! Owned native clipboard reads. A helper process, not a blocking runtime
//! worker, makes native transfer stoppable without trusting its timeout policy.
use std::{path::PathBuf, process::Stdio};

use anyhow::{Context, Result, bail, ensure};
use arboard::{Clipboard, Error as ClipboardError};
use ion_ai::ImageContent;
use serde::{Deserialize, Serialize};
use tokio::{
    io::AsyncReadExt,
    process::{Child, Command},
    time::{Duration, timeout},
};
use tokio_util::sync::CancellationToken;

const TRANSFER_LIMIT: usize = 32 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Serialize, Deserialize)]
pub enum PasteContent {
    Files(Vec<PathBuf>),
    Image {
        content: ImageContent,
        note: Option<String>,
    },
    Text(String),
}

/// Only the private helper entry point calls native APIs. File lists take
/// precedence because Finder also supplies icons for selected files.
fn read_native() -> Result<PasteContent> {
    let mut clipboard = Clipboard::new().context("open system clipboard")?;
    match clipboard.get().file_list() {
        Ok(files) if !files.is_empty() => return Ok(PasteContent::Files(files)),
        Ok(_) | Err(ClipboardError::ContentNotAvailable) => {}
        Err(error) => return Err(error).context("read clipboard file list"),
    }
    match clipboard.get().image() {
        Ok(image) => {
            let image = ion_ai::normalize_rgba(image.width, image.height, image.bytes.into_owned())
                .context("invalid clipboard image")?;
            return Ok(PasteContent::Image {
                content: image.content,
                note: image.note,
            });
        }
        Err(ClipboardError::ContentNotAvailable) => {}
        Err(error) => return Err(error).context("read clipboard image"),
    }
    match clipboard.get().text() {
        Ok(text) => Ok(PasteContent::Text(text)),
        Err(ClipboardError::ContentNotAvailable) => bail!("clipboard has no file, image or text"),
        Err(error) => Err(error).context("read clipboard text"),
    }
}

pub fn write_native() -> Result<()> {
    let result = read_native().map_err(|error| format!("{error:#}"));
    serde_json::to_writer(std::io::stdout().lock(), &result).context("write clipboard transfer")
}

pub async fn read(stop: &CancellationToken) -> Result<PasteContent> {
    let mut command = Command::new(std::env::current_exe().context("locate clipboard helper")?);
    command.arg("__clipboard-read").env_clear();
    // The helper needs desktop connection/locale state, not model credentials
    // or arbitrary user configuration from the parent's environment.
    for name in [
        "HOME",
        "USERPROFILE",
        "SystemRoot",
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XDG_RUNTIME_DIR",
        "XAUTHORITY",
        "DBUS_SESSION_BUS_ADDRESS",
        "LANG",
        "LC_ALL",
        "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    ensure!(!stop.is_cancelled(), "clipboard read cancelled");
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("start clipboard reader")?;
    read_child(child, stop, READ_TIMEOUT).await
}

async fn read_child(
    mut child: Child,
    stop: &CancellationToken,
    deadline: Duration,
) -> Result<PasteContent> {
    let result = {
        let transfer = async {
            let stdout = child
                .stdout
                .take()
                .context("clipboard reader has no output")?;
            let mut bytes = Vec::new();
            stdout
                .take((TRANSFER_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .await
                .context("read clipboard transfer")?;
            ensure!(
                bytes.len() <= TRANSFER_LIMIT,
                "clipboard transfer exceeds 32 MiB"
            );
            let status = child.wait().await.context("join clipboard reader")?;
            ensure!(status.success(), "clipboard reader exited with {status}");
            Ok(bytes)
        };
        tokio::select! {
            biased;
            _ = stop.cancelled() => Err(anyhow::anyhow!("clipboard read cancelled")),
            result = timeout(deadline, transfer) => {
                match result {
                    Ok(result) => result,
                    Err(error) => Err(anyhow::Error::new(error).context("clipboard read timed out")),
                }
            }
        }
    };
    // Do not return a timeout/error while the native call is still alive.
    // kill_on_drop is only a panic/abort backstop, not normal settlement.
    if result.is_err() {
        let killed = child.start_kill();
        let joined = child.wait().await;
        killed.context("stop clipboard reader")?;
        joined.context("join stopped clipboard reader")?;
    }
    let bytes = result?;
    // Image validation is bounded CPU work, not an uninterruptible native API.
    // Join it even when input cancellation arrives during decoding.
    tokio::task::spawn_blocking(move || {
        let result: std::result::Result<PasteContent, String> =
            serde_json::from_slice(&bytes).context("invalid clipboard transfer")?;
        result.map_err(anyhow::Error::msg)
    })
    .await
    .context("clipboard transfer decoder stopped")?
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn shell(script: &str) -> Child {
        Command::new("/bin/sh")
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    #[tokio::test]
    async fn owned_transfers_preserve_content_and_native_errors() {
        for result in [
            Ok(PasteContent::Text("literal\n🦀".into())),
            Ok(PasteContent::Files(vec![PathBuf::from("/owned/a b")])),
            {
                let image = ion_ai::normalize_rgba(1, 1, vec![255, 0, 0, 255]).unwrap();
                Ok(PasteContent::Image {
                    content: image.content,
                    note: image.note,
                })
            },
            Err("clipboard owner declined transfer".to_owned()),
        ] {
            let json = serde_json::to_string(&result).unwrap();
            let script = format!("printf '%s' {}", shlex::try_quote(&json).unwrap());
            let actual = read_child(
                shell(&script),
                &CancellationToken::new(),
                Duration::from_secs(5),
            )
            .await;
            match result {
                Ok(expected) => assert_eq!(
                    serde_json::to_value(actual.unwrap()).unwrap(),
                    serde_json::to_value(expected).unwrap()
                ),
                Err(error) => assert!(actual.unwrap_err().to_string().contains(&error)),
            }
        }
    }

    #[tokio::test]
    async fn failed_or_oversized_transfers_leave_no_owned_helper() {
        for (script, expected) in [
            ("printf invalid".to_owned(), "invalid clipboard transfer"),
            ("exit 7".to_owned(), "clipboard reader exited"),
            (
                format!("exec head -c {} /dev/zero", TRANSFER_LIMIT + 1),
                "transfer exceeds",
            ),
        ] {
            let child = shell(&script);
            let pid = child.id().unwrap();
            let error = read_child(child, &CancellationToken::new(), Duration::from_secs(5))
                .await
                .unwrap_err();
            assert!(error.to_string().contains(expected), "{error:#}");
            assert!(
                !std::process::Command::new("/bin/kill")
                    .args(["-0", &pid.to_string()])
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    #[tokio::test]
    async fn timeout_and_cancellation_reap_the_owned_helper() {
        for cancelled in [false, true] {
            let child = shell("exec sleep 30");
            let pid = i32::try_from(child.id().unwrap()).unwrap();
            let stop = CancellationToken::new();
            if cancelled {
                stop.cancel();
            }
            let deadline = if cancelled {
                Duration::from_secs(30)
            } else {
                Duration::ZERO
            };
            let error = timeout(Duration::from_secs(5), read_child(child, &stop, deadline))
                .await
                .unwrap()
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(if cancelled { "cancelled" } else { "timed out" })
            );
            // This is the directly owned helper (exec replaces sh), not a
            // claim that an arbitrary process tree is quiescent.
            assert!(
                !std::process::Command::new("/bin/kill")
                    .args(["-0", &pid.to_string()])
                    .stderr(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }
}
