//! Live-peer supervision registry (DESIGN.md §19, §24.4).
//!
//! One registry per service (MCP servers, extensions) tracks each
//! configured peer's supervisor by structural identity —
//! `(name, command, args)`. Service `ensure` methods diff the desired
//! defs against the live set through this registry:
//!
//! - identical def → the running supervisor is left alone (a
//!   `/reload` that changed nothing restarts no processes);
//! - changed def → the old supervisor is stopped and joined before
//!   the replacement starts (no two supervisors ever race one scope
//!   or one peer map entry);
//! - removed def → the supervisor is stopped and its live generation
//!   unpublished through the existing close path; the declared scope
//!   stays, so a later re-add re-admits without new authority (§19:
//!   transient peer loss only unpublishes a generation, never
//!   revokes).
//!
//! Stopping is cancel + bounded await of the supervisor's `stopped`
//! signal; supervisors watch their token at every await point, so the
//! bound only guards a wedged task (backoff is capped below it).

use std::collections::HashMap;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// How long a stop waits for the supervisor to exit before giving up
/// on joining (the supervisor exits at its next await; restart
/// backoff is capped at 2s, so this only trips on a wedged task).
const STOP_JOIN_BOUND: std::time::Duration = std::time::Duration::from_secs(3);

/// Structural identity of one def: name, command, and args joined
/// with NUL, which cannot appear inside a command-line word.
pub(crate) fn peer_key(name: &str, command: &str, args: &[String]) -> String {
    let mut key = format!("{name}\0{command}");
    for arg in args {
        key.push('\0');
        key.push_str(arg);
    }
    key
}

struct SupervisedPeer {
    cancel: CancellationToken,
    stopped: oneshot::Receiver<()>,
}

/// Serialized-caller registry: the TUI run loop resolves one reload at
/// a time, so `record`/`stop` never race. The mutex covers only map
/// access, never an await.
#[derive(Default)]
pub(crate) struct PeerRegistry {
    supervised: std::sync::Mutex<HashMap<String, SupervisedPeer>>,
}

impl PeerRegistry {
    /// Whether a peer with this exact identity is running.
    pub(crate) fn contains(&self, key: &str) -> bool {
        self.supervised
            .lock()
            .expect("peer registry poisoned")
            .contains_key(key)
    }

    /// Live supervisor identities (tests/diagnostics).
    pub(crate) fn keys(&self) -> Vec<String> {
        self.supervised
            .lock()
            .expect("peer registry poisoned")
            .keys()
            .cloned()
            .collect()
    }

    /// Record one started supervisor. The `stopped` receiver fires
    /// when the supervisor task returns; `cancel` stops it.
    pub(crate) fn record(
        &self,
        key: String,
        cancel: CancellationToken,
        stopped: oneshot::Receiver<()>,
    ) {
        self.supervised
            .lock()
            .expect("peer registry poisoned")
            .insert(key, SupervisedPeer { cancel, stopped });
    }

    /// Stop the peer with this identity, joining its exit within
    /// [`STOP_JOIN_BOUND`]. Returns whether a peer was stopped; a
    /// timeout still removes the entry (the token stays cancelled, so
    /// a late exit cannot publish anything after removal).
    pub(crate) async fn stop(&self, key: &str) -> bool {
        let Some(peer) = self
            .supervised
            .lock()
            .expect("peer registry poisoned")
            .remove(key)
        else {
            return false;
        };
        peer.cancel.cancel();
        let _ = tokio::time::timeout(STOP_JOIN_BOUND, peer.stopped).await;
        true
    }
}
