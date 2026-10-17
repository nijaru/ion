//! A live, single-use transfer, not authority reconstructed from a record.
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{gateway::run_child, *};
use crate::{CodingSessionError, Operation, OperationError, operation::OperationOwner};

/// A trusted live Host destination. Core calls `reserve` after reconciliation
/// and before durable admission, so an exact retry needs no extra capacity.
/// Implementations must retain the operation before acknowledging installation, close on
/// infrastructure failure, and stop/join before releasing their environment.
pub trait BackgroundSlot: Send {
    fn owner(&self) -> uuid::Uuid;
    fn reserve(&mut self) -> Result<(), String>;
    fn install(
        self: Box<Self>,
        job: BackgroundJob,
    ) -> Result<DelegationReceipt, BackgroundStartError>;
}

/// Refusal or infrastructure failure while installing a background child.
#[derive(Debug, thiserror::Error)]
pub enum BackgroundStartError {
    #[error("background start refused: {0}")]
    Refused(String),
    // An explicit source preserves the Session error itself for classification;
    // a transparent wrapper would delegate past it in the error chain.
    #[error("{0}")]
    Infrastructure(#[source] CodingSessionError),
}

/// Only a live invocation gateway can create this cold transfer. Historical
/// coordinates, owner UUIDs and serialized receipts cannot construct one.
pub struct BackgroundJob {
    pub(super) parent: Arc<CodingSession>,
    pub(super) record: DelegationRecord,
    pub(super) context: Vec<crate::SessionEntry>,
    pub(super) work: DelegationWork,
    pub(super) parent_stop: CancellationToken,
}

struct FailOnDrop<F: FnOnce()>(Option<F>);
impl<F: FnOnce()> Drop for FailOnDrop<F> {
    fn drop(&mut self) {
        if let Some(fail) = self.0.take() {
            fail();
        }
    }
}

impl BackgroundJob {
    /// The actual retained parent, not an authority reconstructed from its ID.
    /// Destinations may keep a weak handle for rebinding while work is live;
    /// unread results must not keep the exclusive writer alive after settlement.
    pub fn parent(&self) -> &Arc<CodingSession> {
        &self.parent
    }

    /// Install under the destination owner's admission lock. The existing
    /// retained-operation primitive checks runtime/custody before the transfer
    /// transaction and starts work only afterward. A cancelled parent observed
    /// here refuses transfer; cancellation racing this commit may lose to it.
    /// After this boundary only the destination's token controls child lifetime.
    /// The failure hook must not panic or block on the installation lock: it can
    /// run synchronously when a spawned task is destroyed before its first poll.
    pub fn start(
        self,
        destination: uuid::Uuid,
        stop: CancellationToken,
        infrastructure_failed: impl FnOnce() + Send + 'static,
    ) -> Result<
        (
            DelegationReceipt,
            Operation<DelegationReceipt, CodingSessionError>,
        ),
        BackgroundStartError,
    > {
        if self.record.ownership != (DelegationOwnership::Background { owner: destination }) {
            return Err(BackgroundStartError::Infrastructure(
                CodingSessionError::InvalidHistory,
            ));
        }
        let owner = Arc::new(OperationOwner::default());
        let retained_owner = owner.clone();
        let lease = Arc::new(());
        let receipt = DelegationReceipt {
            record: self.record.clone(),
            transferred: true,
            outcome: None,
        };
        let parent = self.parent.clone();
        let record = self.record.clone();
        let prepared = owner.start_prepared(stop.clone(), || {
            if self.parent_stop.is_cancelled() || stop.is_cancelled() {
                return Err(CodingSessionError::Operation(OperationError::Closed));
            }
            parent.transfer_delegation(&record, &lease)?;
            // Arm after commit but before spawn. A task destroyed before its
            // first poll must also close admission. The Host hook must not
            // block on the destination's installation lock.
            let failure = FailOnDrop(Some(infrastructure_failed));
            Ok((
                receipt.clone(),
                Some(async move {
                    // Weak leases are live only while this actual task retains
                    // custody. On panic/loss they expire; they never grant replay.
                    let _retained_owner = retained_owner;
                    let _lease = lease;
                    let mut failure = failure;
                    let (outcome, infrastructure) =
                        run_child(self.record.clone(), self.context, self.work, stop).await;
                    let result = self
                        .parent
                        .settle_delegation(&self.record, outcome.clone())
                        .and_then(|()| match infrastructure {
                            Some(error) => Err(CodingSessionError::DelegationInfrastructure(error)),
                            None => Ok(DelegationReceipt {
                                record: self.record,
                                transferred: true,
                                outcome: Some(outcome),
                            }),
                        });
                    if result.is_ok() {
                        failure.0 = None;
                    }
                    result
                }),
            ))
        });
        match prepared {
            Ok((receipt, Some(operation))) => Ok((receipt, operation)),
            Ok((_, None)) => unreachable!("a transfer always retains work"),
            Err(CodingSessionError::Operation(OperationError::Closed)) => Err(
                BackgroundStartError::Refused("stopped before ownership transfer".into()),
            ),
            Err(error) => Err(BackgroundStartError::Infrastructure(error)),
        }
    }
}
