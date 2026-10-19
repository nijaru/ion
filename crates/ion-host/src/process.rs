//! Application-lifetime ownership of managed native programs.
//!
//! Pipe-backed programs remain the default. PTY-backed programs extend the
//! same owner, scoping, receipt, stop and join contract with a fresh session,
//! controlling terminal, merged raw output and a derived bounded screen.
//! Controls are scoped by Core invocation provenance and the canonical working
//! directory captured when the tools were composed. Handles are owner-issued
//! UUIDs, never PIDs. Startup is registered before spawn; explicit close and
//! application shutdown request a process-group stop and join the real driver.
mod driver;
mod pty;
mod ring;
mod screen;
mod tools;
pub use tools::ProcessTools;
#[cfg(test)]
mod tests;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Result, ensure};
use ion_core::ToolOrigin;
use tokio::{sync::Mutex, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use driver::ProcessState;
use ring::Ring;

/// One application-lifetime live supervisor. It is not a daemon, durable job
/// store or process-resurrection mechanism.
pub struct ProcessOwner {
    capacity: usize,
    stop: CancellationToken,
    entries: Mutex<BTreeMap<Uuid, Entry>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Backend {
    Pipe,
    Pty,
}

pub(in crate::process) enum Capture {
    Pipe {
        stdout: Arc<Ring>,
        stderr: Arc<Ring>,
    },
    Pty {
        raw: Arc<Ring>,
        screen: Arc<screen::Terminal>,
    },
}

#[derive(Clone)]
struct Entry {
    session: Uuid,
    cwd: PathBuf,
    state: Arc<ProcessState>,
    gate: Arc<JoinGate>,
}

struct JoinGate {
    process: Arc<ProcessState>,
    state: Mutex<JoinState>,
}

enum JoinState {
    Running(JoinHandle<Result<(), String>>),
    Joined(Result<(), String>),
}

impl JoinGate {
    fn new(process: Arc<ProcessState>, handle: JoinHandle<Result<(), String>>) -> Self {
        Self {
            process,
            state: Mutex::new(JoinState::Running(handle)),
        }
    }

    /// Cancellation-safe single-flight join. A dropped wait drops only the
    /// mutex guard; the retained handle and eventual result stay in the gate.
    async fn join(&self) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if let JoinState::Joined(result) = &*state {
            return result.clone();
        }
        let result = match &mut *state {
            JoinState::Running(handle) => match handle.await {
                Ok(result) => result,
                Err(error) => Err(format!("process supervision task failed: {error}")),
            },
            JoinState::Joined(result) => return result.clone(),
        };
        self.process.mark_joined(result.as_ref().err().cloned());
        *state = JoinState::Joined(result.clone());
        result
    }

    fn is_joined(&self) -> bool {
        self.state
            .try_lock()
            .is_ok_and(|state| matches!(&*state, JoinState::Joined(_)))
    }
}

impl ProcessOwner {
    /// Capacity counts running processes and retained joined/failed-start
    /// results together, never RSS or output volume.
    pub fn new(capacity: usize) -> Result<Self> {
        ensure!(
            (1..=64).contains(&capacity),
            "process capacity must be 1..=64"
        );
        Ok(Self {
            capacity,
            stop: CancellationToken::new(),
            entries: Mutex::new(BTreeMap::new()),
        })
    }

    /// Synchronous and independent of the registry lock: closes new starts and
    /// requests stop for every retained driver.
    pub fn request_shutdown(&self) {
        self.stop.cancel();
    }

    /// Join every retained driver, writer and output reader. Concurrent callers
    /// share each gate; a cancelled caller leaves the retained join untouched.
    pub async fn join_shutdown(&self) -> Result<()> {
        self.request_shutdown();
        let gates = self
            .entries
            .lock()
            .await
            .values()
            .map(|entry| entry.gate.clone())
            .collect::<Vec<_>>();
        let mut failures = Vec::new();
        for gate in gates {
            if let Err(error) = gate.join().await {
                failures.push(error);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(failures.join("; ")))
        }
    }

    /// End and release one completed execution scope. The caller must first
    /// close and join Core admission for this Session; this is not an atomic
    /// admission barrier against another trusted runtime starting new work.
    pub async fn close_session(&self, session: Uuid) -> Result<()> {
        let entries = self
            .entries
            .lock()
            .await
            .iter()
            .filter(|(_, entry)| entry.session == session)
            .map(|(id, entry)| (*id, entry.clone()))
            .collect::<Vec<_>>();
        for (_, entry) in &entries {
            entry.state.stop.cancel();
        }
        let mut failures = Vec::new();
        for (_, entry) in &entries {
            if let Err(error) = entry.gate.join().await {
                failures.push(error);
            }
        }
        let mut retained = self.entries.lock().await;
        for (id, _) in entries {
            retained.remove(&id);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(failures.join("; ")))
        }
    }

    fn closed(&self) -> bool {
        self.stop.is_cancelled()
    }

    async fn scoped(&self, origin: ToolOrigin, cwd: &Path, id: Uuid) -> Result<Entry, String> {
        let entries = self.entries.lock().await;
        match entries.get(&id) {
            Some(entry) if entry.session == origin.session() && entry.cwd == cwd => {
                Ok(entry.clone())
            }
            _ => Err(unavailable()),
        }
    }
}

impl Drop for ProcessOwner {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

fn unavailable() -> String {
    "process is unavailable (unknown, foreign Session/cwd, acknowledged, or previous owner)".into()
}
