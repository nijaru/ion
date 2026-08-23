//! Owned subprocess lifecycle for external effects.
//!
//! Every Ion-owned subprocess runs in its own process group on Unix. The
//! guard keeps that group armed while the owning future is alive, while Tokio
//! provides a direct-child kill fallback when a future is dropped. Callers
//! explicitly wait and disarm only after their output/transport cleanup is
//! complete.

use std::process::ExitStatus;

use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};

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

    pub(crate) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
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
