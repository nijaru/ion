//! Owned subprocess lifecycle for external effects.
//!
//! Every Ion-owned subprocess runs in its own process group on Unix. The
//! guard keeps that group armed while the owning future is alive, while Tokio
//! provides a direct-child kill fallback when a future is dropped. Callers
//! explicitly wait and disarm only after their output/transport cleanup is
//! complete.

use std::path::Path;
use std::process::ExitStatus;

use tokio::process::{ChildStderr, ChildStdout, Command};

/// OS enforcement selected for native shell effects.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// Select the strongest backend available on this host.
    #[default]
    Auto,
    /// Run directly under the host process policy.
    Unconfined,
    /// macOS Seatbelt (`sandbox-exec`) workspace-write policy.
    Seatbelt,
    /// Linux Bubblewrap workspace-write policy.
    Bubblewrap,
    /// An explicitly requested backend is unavailable on this host.
    #[serde(skip)]
    Unavailable,
}

impl SandboxMode {
    /// Resolve `Auto` once at tool-catalog construction so the capability
    /// description and the actual executor report the same mode.
    #[must_use]
    pub fn resolve(self) -> Self {
        match self {
            Self::Auto => {
                #[cfg(target_os = "macos")]
                if executable_on_path("sandbox-exec") {
                    return Self::Seatbelt;
                }
                #[cfg(target_os = "linux")]
                if executable_on_path("bwrap") {
                    return Self::Bubblewrap;
                }
                Self::Unconfined
            }
            Self::Seatbelt => {
                #[cfg(target_os = "macos")]
                if executable_on_path("sandbox-exec") {
                    return Self::Seatbelt;
                }
                Self::Unavailable
            }
            Self::Bubblewrap => {
                #[cfg(target_os = "linux")]
                if executable_on_path("bwrap") {
                    return Self::Bubblewrap;
                }
                Self::Unavailable
            }
            mode => mode,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Unconfined => "unconfined",
            Self::Seatbelt => "seatbelt",
            Self::Bubblewrap => "bubblewrap",
            Self::Unavailable => "unavailable",
        }
    }

    /// Build the command that executes one external program under this
    /// mode. Sandboxed modes deny network and permit writes only below the
    /// canonical workspace root.
    pub(crate) fn command(
        self,
        cwd: &Path,
        program: &str,
        args: &[&str],
    ) -> Result<Command, String> {
        match self {
            Self::Auto => Err("sandbox mode must be resolved before execution".to_owned()),
            Self::Unavailable => Err("requested sandbox backend is unavailable".to_owned()),
            Self::Unconfined => {
                let mut command = Command::new(program);
                command.args(args).current_dir(cwd);
                Ok(command)
            }
            Self::Seatbelt => {
                let canonical = canonical_cwd(cwd)?;
                let mut command = Command::new("sandbox-exec");
                command
                    .arg("-p")
                    .arg(seatbelt_profile(&canonical))
                    .arg(program)
                    .args(args)
                    .current_dir(cwd);
                Ok(command)
            }
            Self::Bubblewrap => {
                let canonical = canonical_cwd(cwd)?;
                let mut command = Command::new("bwrap");
                command
                    .args([
                        "--die-with-parent",
                        "--unshare-user",
                        "--unshare-pid",
                        "--unshare-net",
                        "--ro-bind",
                        "/",
                        "/",
                    ])
                    .arg("--bind")
                    .arg(&canonical)
                    .arg(&canonical)
                    .args(["--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp"])
                    .arg("--chdir")
                    .arg(&canonical)
                    .arg("--")
                    .arg(program)
                    .args(args)
                    .current_dir(cwd);
                Ok(command)
            }
        }
    }
}

impl std::fmt::Display for SandboxMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

fn canonical_cwd(cwd: &Path) -> Result<std::path::PathBuf, String> {
    cwd.canonicalize()
        .map_err(|err| format!("sandbox workspace {}: {err}", cwd.display()))
}

fn executable_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(name))
        .any(|path| path.is_file())
}

fn seatbelt_profile(cwd: &Path) -> String {
    let cwd = cwd
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        "(version 1) \
         (deny default) \
         (allow file-read*) \
         (allow process-exec) \
         (allow process-fork) \
         (allow signal (target self)) \
         (allow sysctl-read) \
         (allow file-write* (subpath \"{cwd}\")) \
         (allow file-write* (literal \"/dev/null\"))"
    )
}

/// Own a spawned process until it has been waited or explicitly terminated.
pub(crate) struct ProcessGuard {
    child: tokio::process::Child,
    process_group: Option<i32>,
    armed: bool,
}

impl ProcessGuard {
    pub(crate) fn spawn(command: &mut Command) -> Result<Self, std::io::Error> {
        #[cfg(unix)]
        command.process_group(0);
        let child = command.kill_on_drop(true).spawn()?;
        let process_group = child.id().map(|pid| pid as i32);
        Ok(Self {
            child,
            process_group,
            armed: true,
        })
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    pub(crate) async fn wait(&mut self) -> Result<ExitStatus, std::io::Error> {
        self.child.wait().await
    }

    pub(crate) async fn kill_and_wait(&mut self) -> Result<ExitStatus, std::io::Error> {
        self.kill_owned_processes();
        #[cfg(not(unix))]
        self.child.kill().await?;
        self.child.wait().await
    }

    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }

    fn kill_owned_processes(&mut self) {
        if let Some(process_group) = self.process_group {
            kill_process_group(process_group);
        }
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if self.armed {
            self.kill_owned_processes();
        }
    }
}

/// Kill a child process group. On Unix, a negative pid targets the whole
/// group, so descendants die with the owned peer. A race with a naturally
/// finishing command is harmless because ESRCH is ignored. Non-Unix uses
/// Tokio's direct-child kill-on-drop/cancellation fallback.
fn kill_process_group(pgid: i32) {
    #[cfg(unix)]
    #[allow(unsafe_code)]
    unsafe {
        let _ = libc::kill(-pgid, libc::SIGKILL);
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
    }
}
