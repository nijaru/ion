//! One-shot child control uses the ordinary Host input owner, not another queue.
use std::sync::{Arc, Mutex};

use anyhow::{Result, ensure};
use ion_ai::Message;
use ion_core::{AgentLimits, DelegationTask, InputBudget, RequestKey};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{BackgroundOwner, InputId, SteeringReceipt, SteeringStatus, TurnSteering};

type Ready = Option<(uuid::Uuid, RequestKey, TurnSteering<()>)>;
pub(super) type Readiness = watch::Sender<Ready>;

/// The task and its live control grant belong to the same prepared application.
/// Keeping control does not keep its child Session writer or execution alive.
pub struct PreparedDelegation {
    pub task: DelegationTask,
    pub steering: ChildSteering,
    pub progress: super::ChildProgress,
}
impl PreparedDelegation {
    /// Transfer execution and its matching control grant to the same owner.
    pub fn in_background(self, owner: &BackgroundOwner) -> DelegationTask {
        self.task.in_background(
            owner
                .destination()
                .with_steering(self.steering)
                .with_progress(self.progress),
        )
    }

    /// Join within a budget, transferring the same execution and its grant only
    /// if it still needs joining at expiry. Capacity is reserved before work.
    pub fn in_background_after(
        self,
        owner: &BackgroundOwner,
        join_timeout: std::time::Duration,
    ) -> DelegationTask {
        self.task.in_background_after(
            owner
                .destination()
                .with_steering(self.steering)
                .with_progress(self.progress),
            join_timeout,
        )
    }
}

#[derive(Clone, Serialize)]
pub struct ChildSteeringReport {
    pub request: RequestKey,
    pub id: InputId,
    pub status: SteeringStatus,
}
struct Submission {
    request: RequestKey,
    fingerprint: [u8; 32],
    receipt: SteeringReceipt,
}
impl Submission {
    fn report(&self) -> ChildSteeringReport {
        ChildSteeringReport {
            request: self.request.clone(),
            id: self.receipt.id,
            status: self.receipt.status(),
        }
    }
}

/// Ephemeral control of one prepared child. Request keys reconcile only within
/// this retained controller, not after process loss. Acknowledged job IDs cannot
/// recover the grant from their former owner.
#[derive(Clone)]
pub struct ChildSteering {
    ready: watch::Receiver<Ready>,
    submissions: Arc<Mutex<Vec<Submission>>>,
    limits: AgentLimits,
    changed: Arc<Mutex<Option<std::task::Waker>>>,
}
impl ChildSteering {
    pub(super) fn new(limits: AgentLimits) -> (Self, Readiness) {
        let (sender, ready) = watch::channel(None);
        (
            Self {
                ready,
                submissions: Arc::new(Mutex::new(Vec::new())),
                limits,
                changed: Arc::new(Mutex::new(None)),
            },
            sender,
        )
    }
    pub(crate) fn observe_changes(&self, changed: std::task::Waker) {
        *self
            .changed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(changed);
    }
    pub fn snapshot(&self) -> Vec<ChildSteeringReport> {
        self.submissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .map(Submission::report)
            .collect()
    }
    /// Wait for readiness, then synchronously queue literal guidance. Cancelling
    /// readiness never stops the child; once queued, abandoning this wait cannot
    /// recall the input. A queued report is not committed model delivery.
    pub async fn steer(
        &self,
        request: &RequestKey,
        input: &Message,
        stop: &CancellationToken,
    ) -> Result<ChildSteeringReport> {
        self.steer_run(None, request, input, stop).await
    }
    pub(crate) async fn steer_run(
        &self,
        expected: Option<(uuid::Uuid, RequestKey)>,
        request: &RequestKey,
        input: &Message,
        stop: &CancellationToken,
    ) -> Result<ChildSteeringReport> {
        // Validate before waiting, without retaining a second input queue.
        drop(InputBudget::default().reserve(input, &(), self.limits)?);
        let fingerprint: [u8; 32] = Sha256::digest(serde_json::to_vec(input)?).into();
        let mut readiness = self.ready.clone();
        loop {
            ensure!(!stop.is_cancelled(), "steering stopped; child continues");
            let ready = readiness.borrow_and_update().clone();
            if let Some((child, run, port)) = ready {
                ensure!(
                    expected
                        .as_ref()
                        .is_none_or(|(session, request)| *session == child && *request == run),
                    "child control grant does not match its owned run"
                );
                let mut submissions = self
                    .submissions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(existing) = submissions.iter().find(|s| &s.request == request) {
                    ensure!(
                        existing.fingerprint == fingerprint,
                        "conflicting child steering request"
                    );
                    return Ok(existing.report());
                }
                let changed = self
                    .changed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                let receipt = port
                    .submit_observed(input.clone(), (), changed, Some(stop))
                    .map_err(anyhow::Error::new)?;
                submissions.push(Submission {
                    request: request.clone(),
                    fingerprint,
                    receipt,
                });
                return Ok(submissions.last().expect("just retained steering").report());
            }
            tokio::select! {
                biased;
                () = stop.cancelled() => anyhow::bail!("steering stopped; child continues"),
                changed = readiness.changed() => changed.map_err(|_| anyhow::anyhow!("child control is no longer available"))?,
            }
        }
    }
}
