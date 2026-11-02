//! A live, single-use transfer, not authority reconstructed from a record.
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{
    execution::{Completion, run_child},
    *,
};
use crate::{Operation, OperationError, SessionError, SessionIo, operation::OperationOwner};

/// A trusted live Host destination. Core reserves custody before durable
/// admission, returning the destination's stop scope. Exact retries need no
/// extra capacity. Installation must take execution only after transfer commits;
/// on refusal/error Core retains and joins any work already started. Destinations
/// retain the operation before acknowledgement and stop/join before shutdown.
pub trait BackgroundSlot: Send {
    fn owner(&self) -> uuid::Uuid;
    fn reserve(&mut self) -> Result<CancellationToken, String>;
    fn install<'a>(
        self: Box<Self>,
        job: &'a mut BackgroundJob,
    ) -> BoxFuture<'a, Result<DelegationReceipt, BackgroundStartError>>;
}

/// Refusal or infrastructure failure while installing a background child.
#[derive(Debug, thiserror::Error)]
pub enum BackgroundStartError {
    #[error("background start refused: {0}")]
    Refused(String),
    #[error("{0}")]
    Infrastructure(#[source] SessionError),
}

/// Only a live invocation gateway can create this single-use transfer. It can
/// contain a cold or already-polled callback; moving it never recreates work.
/// Historical coordinates, owner UUIDs and serialized receipts cannot make one.
pub struct BackgroundJob {
    io: Arc<SessionIo>,
    record: DelegationRecord,
    transfer_lease: Option<Arc<()>>,
    execution: Option<ion_ai::BoxFuture<'static, Completion>>,
    stop: CancellationToken,
    parent_stop: CancellationToken,
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
    pub(super) fn new(
        io: Arc<SessionIo>,
        child_io: Arc<SessionIo>,
        record: DelegationRecord,
        prepared: ChildMaterialization,
        work: DelegationWork,
        stop: CancellationToken,
        parent_stop: CancellationToken,
    ) -> Self {
        Self {
            execution: Some(Box::pin(run_child(
                child_io,
                record.clone(),
                prepared,
                work,
                stop.clone(),
            ))),
            io,
            record,
            transfer_lease: None,
            stop,
            parent_stop,
        }
    }

    pub(super) fn request_stop(&self) {
        self.stop.cancel();
    }

    pub(super) async fn join(&mut self) -> Completion {
        self.execution
            .as_mut()
            .expect("gateway retains execution until transfer")
            .await
    }

    /// The actual retained parent, not authority reconstructed from its ID.
    /// Unread results must not keep its exclusive writer after settlement.
    pub fn parent(&self) -> &Arc<crate::Session> {
        self.io.session()
    }

    /// Commit transfer off-executor before taking the same pinned execution.
    /// The destination must retain its reservation across this wait, without
    /// holding a lifecycle lock. On returned error execution remains here, so
    /// the gateway can stop and fully join it before release. An abandoned wait
    /// retains the actual I/O and its lease; this job still owns foreground work
    /// and must be joined, not retried or treated as rolled back.
    /// A parent stop observed before commit refuses transfer; a racing stop may
    /// lose to commit. After commit the destination's reserved stop scope owns it.
    /// A joined infrastructure error carries its committed receipt in
    /// `SessionError::DelegationSettledFailure`; failed settlement does not.
    /// The failure hook must not panic or block on the installation lock.
    pub async fn start(
        &mut self,
        destination: uuid::Uuid,
        infrastructure_failed: impl FnOnce() + Send + 'static,
    ) -> Result<
        (
            DelegationReceipt,
            Operation<DelegationReceipt, SessionError>,
        ),
        BackgroundStartError,
    > {
        if self.execution.is_none()
            || self.transfer_lease.is_some()
            || self.record.ownership != (DelegationOwnership::Background { owner: destination })
        {
            return Err(BackgroundStartError::Infrastructure(
                SessionError::InvalidHistory,
            ));
        }
        tokio::runtime::Handle::try_current().map_err(|_| {
            BackgroundStartError::Infrastructure(SessionError::Operation(
                OperationError::RuntimeUnavailable,
            ))
        })?;
        let owner = Arc::new(OperationOwner::default());
        let retained_owner = owner.clone();
        let lease = Arc::new(());
        self.transfer_lease = Some(lease.clone());
        let record = self.record.clone();
        let parent_stop = self.parent_stop.clone();
        let stop = self.stop.clone();
        let transfer = self
            .io
            .run_open(move |parent| {
                if parent_stop.is_cancelled() || stop.is_cancelled() {
                    return Err(SessionError::Operation(OperationError::Closed));
                }
                parent.transfer_delegation(&record, &lease)
            })
            .await;
        match transfer {
            Ok(()) => {}
            Err(SessionError::Operation(OperationError::Closed)) => {
                return Err(BackgroundStartError::Refused(
                    "stopped before ownership transfer".into(),
                ));
            }
            Err(error) => return Err(BackgroundStartError::Infrastructure(error)),
        }
        let receipt = DelegationReceipt {
            record: self.record.clone(),
            transferred: true,
            outcome: None,
        };
        let io = self.io.clone();
        let record = self.record.clone();
        let prepared = owner.start_prepared(self.stop.clone(), || {
            let execution = self
                .execution
                .take()
                .expect("validated single-use transfer");
            // Arm before spawn, including task destruction before its first poll.
            let failure = FailOnDrop(Some(infrastructure_failed));
            let lease = self
                .transfer_lease
                .take()
                .expect("committed transfer lease");
            Ok((
                receipt.clone(),
                Some(async move {
                    let _retained_owner = retained_owner;
                    let mut failure = failure;
                    let (outcome, infrastructure) = execution.await;
                    let saved_record = record.clone();
                    let saved_outcome = outcome.clone();
                    let result = io
                        .run(move |parent| {
                            // Retain live custody until the actual settlement
                            // finishes, even if its async waiter is abandoned.
                            let _lease = lease;
                            parent.settle_delegation(&saved_record, saved_outcome)
                        })
                        .await
                        .and_then(|()| {
                            let receipt = DelegationReceipt {
                                record,
                                transferred: true,
                                outcome: Some(outcome),
                            };
                            match infrastructure {
                                Some(error) => Err(SessionError::DelegationSettledFailure {
                                    receipt: Box::new(receipt),
                                    error: Box::new(SessionError::DelegationInfrastructure(error)),
                                }),
                                None => Ok(receipt),
                            }
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
            Err(SessionError::Operation(OperationError::Closed)) => Err(
                BackgroundStartError::Refused("stopped before ownership transfer".into()),
            ),
            Err(error) => Err(BackgroundStartError::Infrastructure(error)),
        }
    }
}
