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

pub async fn edit(content: &str, max_bytes: usize) -> Result<String> {
    let command = std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "vi".into());
    edit_with_command(&command, content, max_bytes).await
}

async fn edit_with_command(command: &str, content: &str, max_bytes: usize) -> Result<String> {
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
    let status = Command::new(program)
        .args(options)
        .arg(&path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .with_context(|| format!("cannot launch editor {program}"))?;
    ensure!(status.success(), "editor exited with {status}");
    ensure!(
        fs::metadata(&path)?.len() <= (max_bytes + 3) as u64,
        "edited draft exceeds {max_bytes} bytes"
    );
    let mut edited = fs::read_to_string(&path).context("edited draft is not UTF-8")?;
    if edited.starts_with('\u{feff}') {
        edited.remove(0);
    }
    if edited.ends_with('\n') {
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
    async fn editor_success_and_failure_keep_draft_contract() {
        let edited = edit_with_command("/bin/sh -c 'printf changed > \"$0\"'", "original", 64)
            .await
            .unwrap();
        assert_eq!(edited, "changed");
        assert!(
            edit_with_command("/bin/sh -c 'exit 7'", "original", 64)
                .await
                .is_err()
        );
        assert!(
            edit_with_command("/bin/sh -c 'printf long-output > \"$0\"'", "original", 4)
                .await
                .is_err()
        );
    }
}
