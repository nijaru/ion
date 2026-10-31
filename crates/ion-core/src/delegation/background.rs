//! A live, single-use transfer, not authority reconstructed from a record.
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::{
    execution::{Completion, run_child},
    *,
};
use crate::{Operation, OperationError, SessionError, operation::OperationOwner};

/// A trusted live Host destination. Core reserves custody before durable
/// admission, returning the destination's stop scope. Exact retries need no
/// extra capacity. Installation must take execution only after transfer commits;
/// on refusal/error Core retains and joins any work already started. Destinations
/// retain the operation before acknowledgement and stop/join before shutdown.
pub trait BackgroundSlot: Send {
    fn owner(&self) -> uuid::Uuid;
    fn reserve(&mut self) -> Result<CancellationToken, String>;
    fn install(
        self: Box<Self>,
        job: &mut BackgroundJob,
    ) -> Result<DelegationReceipt, BackgroundStartError>;
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
    parent: Arc<crate::Session>,
    record: DelegationRecord,
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
        parent: Arc<crate::Session>,
        record: DelegationRecord,
        prepared: ChildMaterialization,
        work: DelegationWork,
        stop: CancellationToken,
        parent_stop: CancellationToken,
    ) -> Self {
        Self {
            execution: Some(Box::pin(run_child(
                record.clone(),
                prepared,
                work,
                stop.clone(),
            ))),
            parent,
            record,
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
        &self.parent
    }

    /// Install under the destination's admission lock. The operation primitive
    /// checks runtime/custody before the transfer transaction. Only after commit
    /// is the same pinned execution taken from this job. On any returned error
    /// it remains here, so the gateway can stop and fully join it before release.
    /// A parent stop observed before commit refuses transfer; a racing stop may
    /// lose to commit. After commit the destination's reserved stop scope owns it.
    /// A joined infrastructure error carries its committed receipt in
    /// `SessionError::DelegationSettledFailure`; failed settlement does not.
    /// The failure hook must not panic or block on the installation lock.
    pub fn start(
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
            || self.record.ownership != (DelegationOwnership::Background { owner: destination })
        {
            return Err(BackgroundStartError::Infrastructure(
                SessionError::InvalidHistory,
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
        let prepared = owner.start_prepared(self.stop.clone(), || {
            if self.parent_stop.is_cancelled() || self.stop.is_cancelled() {
                return Err(SessionError::Operation(OperationError::Closed));
            }
            parent.transfer_delegation(&record, &lease)?;
            let execution = self
                .execution
                .take()
                .expect("validated single-use transfer");
            // Arm before spawn, including task destruction before its first poll.
            let failure = FailOnDrop(Some(infrastructure_failed));
            Ok((
                receipt.clone(),
                Some(async move {
                    let _retained_owner = retained_owner;
                    let _lease = lease;
                    let mut failure = failure;
                    let (outcome, infrastructure) = execution.await;
                    let result =
                        parent
                            .settle_delegation(&record, outcome.clone())
                            .and_then(|()| {
                                let receipt = DelegationReceipt {
                                    record,
                                    transferred: true,
                                    outcome: Some(outcome),
                                };
                                match infrastructure {
                                    Some(error) => Err(SessionError::DelegationSettledFailure {
                                        receipt: Box::new(receipt),
                                        error: Box::new(SessionError::DelegationInfrastructure(
                                            error,
                                        )),
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
