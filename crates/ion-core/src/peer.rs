//! Peer reconciliation owns the interval from selecting obsolete supervisors
//! through joining them and publishing replacements. A failed stop retains its
//! ownership record: cancellation alone never proves teardown completed.

use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard, oneshot};
use tokio_util::sync::CancellationToken;

const STOP_JOIN_BOUND: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PeerCleanupError {
    #[error("peer monitor failed: {0}")]
    MonitorFailed(String),
    #[error("peer monitor did not drain before the shutdown deadline")]
    DrainTimeout,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PeerServiceError {
    #[error("invalid peer definition: {0}")]
    InvalidDefinition(String),
    #[error("tool catalog is closed; cannot start peer {peer}")]
    CatalogClosed { peer: String },
    #[error("peer {peer} did not acknowledge its initial discovery attempt")]
    StartupAcknowledgementLost { peer: String },
    #[error("peer {peer} initial discovery attempt timed out")]
    StartupTimeout { peer: String },
    #[error("peer {peer} cleanup failed: {error}; replacement is fenced")]
    CleanupFailed {
        peer: String,
        error: PeerCleanupError,
    },
    #[error("peer {peer} did not finish teardown before the deadline; replacement is fenced")]
    StopTimeout { peer: String },
    #[error("peer {peer} lost its teardown acknowledgement; replacement is fenced")]
    StopAcknowledgementLost { peer: String },
}

pub(crate) fn validate_defs<'a>(
    defs: impl Iterator<Item = (&'a String, &'a String, &'a Vec<String>)>,
) -> Result<(), PeerServiceError> {
    let mut names = HashSet::new();
    for (name, command, args) in defs {
        if name.trim().is_empty()
            || command.trim().is_empty()
            || name.contains('\0')
            || command.contains('\0')
            || args.iter().any(|arg| arg.contains('\0'))
        {
            return Err(PeerServiceError::InvalidDefinition(name.clone()));
        }
        if !names.insert(name) {
            return Err(PeerServiceError::InvalidDefinition(format!(
                "duplicate name {name}"
            )));
        }
    }
    Ok(())
}

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
    stopped: oneshot::Receiver<Result<(), PeerCleanupError>>,
    state: PeerState,
}

enum PeerState {
    Running,
    Stopping,
    Joined,
    Failed(PeerServiceError),
}

#[derive(Default)]
pub(crate) struct PeerRegistry {
    supervised: Mutex<HashMap<String, SupervisedPeer>>,
}

pub(crate) struct Reconciliation<'a> {
    supervised: MutexGuard<'a, HashMap<String, SupervisedPeer>>,
}

impl PeerRegistry {
    pub(crate) async fn reconcile(&self) -> Reconciliation<'_> {
        Reconciliation {
            supervised: self.supervised.lock().await,
        }
    }
}

impl Reconciliation<'_> {
    pub(crate) fn contains(&self, key: &str) -> bool {
        self.supervised.contains_key(key)
    }

    pub(crate) fn obsolete(&mut self, desired: &[String]) -> Vec<String> {
        for (key, peer) in self.supervised.iter_mut() {
            if matches!(peer.state, PeerState::Running | PeerState::Stopping) {
                let name = key.split('\0').next().unwrap_or(key).to_owned();
                match peer.stopped.try_recv() {
                    Ok(Ok(())) => peer.state = PeerState::Joined,
                    Ok(Err(error)) => {
                        peer.state =
                            PeerState::Failed(PeerServiceError::CleanupFailed { peer: name, error })
                    }
                    Err(oneshot::error::TryRecvError::Closed) => {
                        peer.state = PeerState::Failed(PeerServiceError::StopAcknowledgementLost {
                            peer: name,
                        })
                    }
                    Err(oneshot::error::TryRecvError::Empty) => {}
                }
            }
        }
        self.supervised
            .iter()
            .filter(|(key, peer)| {
                !matches!(peer.state, PeerState::Running)
                    || peer.cancel.is_cancelled()
                    || !desired.contains(key)
            })
            .map(|(key, _)| key.clone())
            .collect()
    }

    pub(crate) fn record(
        &mut self,
        key: String,
        cancel: CancellationToken,
        stopped: oneshot::Receiver<Result<(), PeerCleanupError>>,
    ) {
        let previous = self.supervised.insert(
            key,
            SupervisedPeer {
                cancel,
                stopped,
                state: PeerState::Running,
            },
        );
        assert!(
            previous.is_none(),
            "replacement requires joined prior supervisor"
        );
    }

    pub(crate) async fn stop(&mut self, key: &str) -> Result<bool, PeerServiceError> {
        self.stop_with_timeout(key, STOP_JOIN_BOUND).await
    }

    async fn stop_with_timeout(
        &mut self,
        key: &str,
        deadline: Duration,
    ) -> Result<bool, PeerServiceError> {
        let Some(peer) = self.supervised.get_mut(key) else {
            return Ok(false);
        };
        if matches!(peer.state, PeerState::Joined) {
            self.supervised.remove(key);
            return Ok(true);
        }
        let name = key.split('\0').next().unwrap_or(key).to_owned();
        peer.cancel.cancel();
        if let PeerState::Failed(error) = &peer.state {
            return Err(error.clone());
        }
        peer.state = PeerState::Stopping;
        match tokio::time::timeout(deadline, &mut peer.stopped).await {
            Ok(Ok(Ok(()))) => {
                self.supervised.remove(key);
                Ok(true)
            }
            Ok(result) => {
                let error = match result {
                    Ok(Err(error)) => PeerServiceError::CleanupFailed { peer: name, error },
                    Err(_) => PeerServiceError::StopAcknowledgementLost { peer: name },
                    Ok(Ok(())) => unreachable!("success handled above"),
                };
                peer.state = PeerState::Failed(error.clone());
                Err(error)
            }
            Err(_) => Err(PeerServiceError::StopTimeout { peer: name }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn timeout_retains_ownership_and_retry_joins_before_replacement() {
        let registry = PeerRegistry::default();
        let mut reconciliation = registry.reconcile().await;
        let cancel = CancellationToken::new();
        let (tx, rx) = oneshot::channel();
        reconciliation.record("peer".into(), cancel.clone(), rx);
        assert!(matches!(
            reconciliation
                .stop_with_timeout("peer", Duration::ZERO)
                .await,
            Err(PeerServiceError::StopTimeout { .. })
        ));
        assert!(cancel.is_cancelled());
        assert!(reconciliation.contains("peer"));
        assert_eq!(reconciliation.obsolete(&["peer".into()]), ["peer"]);
        tx.send(Ok(())).expect("late acknowledgement");
        assert!(reconciliation.stop("peer").await.expect("retry joins"));
        assert!(!reconciliation.contains("peer"));
    }

    #[tokio::test]
    async fn dropped_acknowledgement_remains_a_stable_failure() {
        let registry = PeerRegistry::default();
        let mut reconciliation = registry.reconcile().await;
        let (tx, rx) = oneshot::channel();
        reconciliation.record("peer".into(), CancellationToken::new(), rx);
        drop(tx);
        for _ in 0..2 {
            assert!(matches!(
                reconciliation.stop("peer").await,
                Err(PeerServiceError::StopAcknowledgementLost { .. })
            ));
            assert!(reconciliation.contains("peer"));
        }
    }

    #[tokio::test]
    async fn cleanup_failure_is_not_hidden_by_unchanged_configuration() {
        let registry = PeerRegistry::default();
        let mut reconciliation = registry.reconcile().await;
        let (tx, rx) = oneshot::channel();
        reconciliation.record("peer".into(), CancellationToken::new(), rx);
        tx.send(Err(PeerCleanupError::DrainTimeout))
            .expect("cleanup result");
        assert_eq!(reconciliation.obsolete(&["peer".into()]), ["peer"]);
        for _ in 0..2 {
            assert!(matches!(
                reconciliation.stop("peer").await,
                Err(PeerServiceError::CleanupFailed {
                    error: PeerCleanupError::DrainTimeout,
                    ..
                })
            ));
        }
    }

    #[tokio::test]
    async fn cancelled_reconciliation_keeps_pending_stop_owned() {
        let registry = PeerRegistry::default();
        let (tx, rx) = oneshot::channel();
        registry
            .reconcile()
            .await
            .record("peer".into(), CancellationToken::new(), rx);
        let mut reconciliation = registry.reconcile().await;
        {
            let stop = reconciliation.stop("peer");
            tokio::pin!(stop);
            assert!(
                tokio::time::timeout(Duration::ZERO, &mut stop)
                    .await
                    .is_err()
            );
        }
        drop(reconciliation);
        let mut retry = registry.reconcile().await;
        assert!(retry.contains("peer"));
        tx.send(Ok(())).expect("ack");
        assert!(retry.stop("peer").await.expect("join"));
    }
}
