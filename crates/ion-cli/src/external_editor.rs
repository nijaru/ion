//! Edit an unsent terminal draft with the user's standard terminal editor.
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    process::Stdio,
};

use anyhow::{Context, Result, bail, ensure};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub async fn edit(
    content: &str,
    max_bytes: usize,
    termination: &CancellationToken,
) -> Result<String> {
    let command = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".into());
    // Native input is bound to the controlling terminal, not possibly
    // redirected stdin. The editor must receive the same interactive source.
    let input = fs::File::open("/dev/tty").context("cannot open terminal input for editor")?;
    edit_with_command(&command, content, max_bytes, input.into(), termination).await
}

async fn edit_with_command(
    command: &str,
    content: &str,
    max_bytes: usize,
    input: Stdio,
    termination: &CancellationToken,
) -> Result<String> {
    ensure!(
        !termination.is_cancelled(),
        "editor cancelled by termination request"
    );
    let args = shlex::split(command).context("editor command has invalid quoting")?;
    let (program, options) = args.split_first().context("editor command is empty")?;
    let directory = std::env::temp_dir().join(format!("ion-editor-{}", uuid::Uuid::now_v7()));
    DirBuilder::new().mode(0o700).create(&directory)?;
    let _guard = DraftDir(directory.clone());
    let path = directory.join("prompt.md");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?
        .write_all(content.as_bytes())?;
    ensure!(
        !termination.is_cancelled(),
        "editor cancelled by termination request"
    );
    let mut child = Command::new(program)
        .args(options)
        .arg(&path)
        .stdin(input)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("cannot launch editor {program}"))?;
    let status = tokio::select! {
        biased;
        status = child.wait() => status.context("join editor")?,
        _ = termination.cancelled() => {
            // Only this managed PID, never the shared foreground process group
            // or an unrelated GUI instance. Keep custody until it actually joins.
            let sent = child.id().map(|pid| i32::try_from(pid).expect("Unix child PID fits pid_t")).and_then(rustix::process::Pid::from_raw)
                .map(|pid| rustix::process::kill_process(pid, rustix::process::Signal::TERM))
                .transpose();
            let joined = child.wait().await.context("join terminated editor");
            joined?;
            if let Err(error) = sent {
                ensure!(error == rustix::io::Errno::SRCH, "cannot terminate editor: {error}");
            }
            bail!("editor cancelled by termination request");
        }
    };
    ensure!(status.success(), "editor exited with {status}");
    // Editors may atomically replace the draft. Validate and bound the opened
    // descriptor, not a separate path stat that can race or admit a FIFO.
    // Cleanup removes at most a three-byte BOM and one final CRLF.
    let read_limit = max_bytes
        .checked_add(5)
        .context("editor draft byte bound overflow")?;
    let bytes = ion_app::file_io::read_bounded(&path, read_limit)
        .with_context(|| format!("cannot read edited draft within {max_bytes}-byte limit"))?;
    let mut edited = String::from_utf8(bytes).context("edited draft is not UTF-8")?;
    if edited.starts_with('\u{feff}') {
        edited.remove(0);
    }
    if edited.ends_with("\r\n") {
        edited.truncate(edited.len() - 2);
    } else if edited.ends_with(['\r', '\n']) {
        edited.pop();
    }
    if edited.len() > max_bytes {
        bail!("edited draft exceeds {max_bytes} bytes");
    }
    Ok(edited)
}

struct DraftDir(PathBuf);

impl Drop for DraftDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn termination_joins_the_owned_editor_and_removes_its_draft() {
        let root = std::env::temp_dir().join(format!("ion-editor-stop-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let marker = root.join("started");
        let script = format!(
            "printf '%s\\n%s\\n' $$ \"$0\" > {}.tmp; mv {}.tmp {}; exec sleep 30",
            shlex::try_quote(marker.to_str().unwrap()).unwrap(),
            shlex::try_quote(marker.to_str().unwrap()).unwrap(),
            shlex::try_quote(marker.to_str().unwrap()).unwrap()
        );
        let command = format!("/bin/sh -c {}", shlex::try_quote(&script).unwrap());
        let termination = CancellationToken::new();
        let editor = edit_with_command(&command, "original", 64, Stdio::null(), &termination);
        let observer = async {
            let started = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !marker.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                fs::read_to_string(&marker).map(|text| {
                    let present = text
                        .lines()
                        .nth(1)
                        .is_some_and(|path| std::path::Path::new(path).is_file());
                    (text, present)
                })
            })
            .await;
            termination.cancel();
            started
        };
        let (result, started) = tokio::join!(editor, observer);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("termination request")
        );
        let (observed, draft_was_present) = started.unwrap().unwrap();
        assert!(
            draft_was_present,
            "the actual editor draft must exist before termination"
        );
        let mut lines = observed.lines();
        let pid = rustix::process::Pid::from_raw(lines.next().unwrap().parse().unwrap()).unwrap();
        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH)
        );
        assert!(
            !std::path::Path::new(lines.next().unwrap())
                .parent()
                .unwrap()
                .exists()
        );
        assert!(
            edit_with_command("/bin/false", "original", 64, Stdio::null(), &termination)
                .await
                .unwrap_err()
                .to_string()
                .contains("termination request")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn editor_success_and_failure_keep_draft_contract() {
        let termination = CancellationToken::new();
        let edited = edit_with_command(
            "/bin/sh -c 'printf changed > \"$0\"'",
            "original",
            64,
            Stdio::null(),
            &termination,
        )
        .await
        .unwrap();
        assert_eq!(edited, "changed");
        assert_eq!(
            edit_with_command(
                r#"/bin/sh -c 'printf "\357\273\277abcd\n" > "$0"'"#,
                "original",
                4,
                Stdio::null(),
                &termination
            )
            .await
            .unwrap(),
            "abcd"
        );
        for command in [
            r#"/bin/sh -c 'printf "\357\273\277abcd\r\n" > "$0"'"#,
            r#"/bin/sh -c 'printf "\357\273\277abcd\r" > "$0"'"#,
        ] {
            assert_eq!(
                edit_with_command(command, "original", 4, Stdio::null(), &termination)
                    .await
                    .unwrap(),
                "abcd"
            );
        }
        assert_eq!(
            edit_with_command(
                r#"/bin/sh -c 'printf "a\r\nb\r\n" > "$0"'"#,
                "original",
                4,
                Stdio::null(),
                &termination
            )
            .await
            .unwrap(),
            "a\r\nb"
        );
        assert!(
            edit_with_command(
                "/bin/sh -c 'exit 7'",
                "original",
                64,
                Stdio::null(),
                &termination
            )
            .await
            .is_err()
        );
        for command in [
            "/bin/sh -c 'printf long-output > \"$0\"'",
            r#"/bin/sh -c 'printf "\377" > "$0"'"#,
        ] {
            assert!(
                edit_with_command(command, "original", 4, Stdio::null(), &termination)
                    .await
                    .is_err()
            );
        }
    }
}
