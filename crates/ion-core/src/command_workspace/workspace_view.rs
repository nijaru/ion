//! Host-private command workspace, separated from the live checkout.

use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use thiserror::Error;

use super::snapshot::{
    Snapshot, SnapshotError, SnapshotLimits, build_snapshot, cleanup_private_tree,
};

const MAX_IGNORE_OUTPUT: u64 = 256 * 1024;
const GIT_DEADLINE: Duration = Duration::from_secs(5);

pub(crate) const VIEW_LIMITS: SnapshotLimits = SnapshotLimits {
    max_entries: 100_000,
    max_bytes: 2 * 1024 * 1024 * 1024,
    max_depth: 64,
    max_path_bytes: 4096,
    max_ignored_paths: 4096,
    max_ignored_bytes: MAX_IGNORE_OUTPUT as usize,
};

#[derive(Debug, Error)]
pub(crate) enum ViewError {
    #[error("Git ignored-path discovery failed or exceeded its time/size bound")]
    GitIgnore,
    #[error("invalid command workspace identity")]
    InvalidIdentity,
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub(crate) struct WorkspaceView {
    parent: File,
    command_leaf: String,
    output_leaf: String,
    command_path: PathBuf,
    pub source: Snapshot,
}

impl WorkspaceView {
    pub fn create(
        live_root: &File,
        live_path: &Path,
        private_parent: &File,
        private_path: &Path,
        attempt_identity: &str,
    ) -> Result<Self, ViewError> {
        if attempt_identity.is_empty()
            || attempt_identity.len() > 128
            || !attempt_identity
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(ViewError::InvalidIdentity);
        }
        let command_leaf = format!("exec-{attempt_identity}-command");
        let output_leaf = format!("exec-{attempt_identity}-output");
        let ignored = git_ignored_paths(live_path)?;
        let parent = private_parent.try_clone()?;
        let source = build_snapshot(
            live_root,
            private_parent,
            &command_leaf,
            VIEW_LIMITS,
            &ignored,
        )?;
        Ok(Self {
            parent,
            command_path: private_path.join(&command_leaf),
            command_leaf,
            output_leaf,
            source,
        })
    }

    pub fn command_path(&self) -> &Path {
        &self.command_path
    }

    pub fn capture_output(&self) -> Result<Snapshot, ViewError> {
        let ignored = git_ignored_paths(&self.command_path)?;
        Ok(build_snapshot(
            &self.source.root,
            &self.parent,
            &self.output_leaf,
            VIEW_LIMITS,
            &ignored,
        )?)
    }

    /// Called only after positive scope termination. Failed cleanup keeps the
    /// command claim for explicit operator reconciliation.
    pub fn cleanup(&self) -> Result<(), ViewError> {
        match cleanup_private_tree(&self.parent, &self.output_leaf) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        cleanup_private_tree(&self.parent, &self.command_leaf)?;
        self.parent.sync_all()?;
        Ok(())
    }
}

fn git_ignored_paths(root: &Path) -> Result<Vec<String>, ViewError> {
    match fs::symlink_metadata(root.join(".git")) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let mut child = Command::new("/usr/bin/git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-C",
            root.to_str().ok_or(ViewError::GitIgnore)?,
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| ViewError::GitIgnore)?;
    let stdout = child.stdout.take().ok_or(ViewError::GitIgnore)?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(MAX_IGNORE_OUTPUT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + GIT_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(ViewError::GitIgnore);
            }
        }
    };
    let bytes = reader.join().map_err(|_| ViewError::GitIgnore)??;
    if !status.success() || bytes.len() as u64 > MAX_IGNORE_OUTPUT {
        return Err(ViewError::GitIgnore);
    }
    let mut paths = Vec::new();
    for raw in bytes
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
    {
        let path = std::str::from_utf8(raw)
            .map_err(|_| ViewError::GitIgnore)?
            .trim_end_matches('/');
        if path.is_empty() || paths.len() >= VIEW_LIMITS.max_ignored_paths {
            return Err(ViewError::GitIgnore);
        }
        paths.push(path.to_owned());
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn view_excludes_git_ignored_builds_and_socket() {
        let root =
            std::env::temp_dir().join(format!("iv-{}", &uuid::Uuid::now_v7().to_string()[..8]));
        let live = root.join("live");
        let private = root.join("private");
        fs::create_dir_all(&live).unwrap();
        fs::create_dir(&private).unwrap();
        let init = Command::new("git")
            .args(["-C", live.to_str().unwrap(), "init", "-q"])
            .status()
            .unwrap();
        assert!(init.success());
        fs::write(live.join(".gitignore"), "target/\n").unwrap();
        fs::write(live.join("source.rs"), "fn main() {}\n").unwrap();
        fs::create_dir(live.join("target")).unwrap();
        fs::write(live.join("target/build"), "ignored").unwrap();
        let _socket = UnixListener::bind(live.join(".git/socket")).unwrap();
        let view = WorkspaceView::create(
            &File::open(&live).unwrap(),
            &live,
            &File::open(&private).unwrap(),
            &private,
            "abc",
        )
        .unwrap();
        assert!(view.command_path().join("source.rs").is_file());
        assert!(!view.command_path().join("target").exists());
        assert!(!view.command_path().join(".git/socket").exists());
        assert_eq!(view.source.omitted.len(), 2);
        fs::write(
            view.command_path().join("source.rs"),
            "fn main() { println!(\"ok\"); }\n",
        )
        .unwrap();
        fs::create_dir(view.command_path().join("target")).unwrap();
        fs::write(view.command_path().join("target/build"), "ignored again").unwrap();
        let output = view.capture_output().unwrap();
        assert!(
            !output
                .manifest
                .iter()
                .any(|entry| entry.path.starts_with("target"))
        );
        drop(output);
        view.cleanup().unwrap();
        drop(_socket);
        fs::remove_dir_all(root).unwrap();
    }
}
