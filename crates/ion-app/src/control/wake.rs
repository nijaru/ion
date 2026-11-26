//! Keyed wake handoff for an application that owns one Session generation.
use std::future::Future;

use anyhow::{Context, Result, ensure};
use ion_ai::Message;
use ion_core::RequestKey;
use serde::{Deserialize, Serialize};

use super::{
    AdmissionPayload, RecoveryAdmission, RecoveryFailure, RequestAdmission, RequestLaunch,
    SessionControl,
};

/// The work an application durably associates with its wakeup. A target is not
/// authorization. The application must serialize delivery and wake handling,
/// restore the same authority, and fence stale alarms before calling Ion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeTarget {
    pub session: uuid::Uuid,
    pub key: RequestKey,
}

#[derive(Debug)]
pub enum WakeRecovery {
    /// The key owns no unfinished work. Never recreate the input from an alarm.
    Quiescent,
    /// Work is already owned by this instance. The wakeup has been rearmed.
    AlreadyOwned,
    Resumed(RecoveryAdmission),
    /// Preserve the known-refusal versus uncertain-commit classification.
    Refused(RecoveryFailure<()>),
}

impl SessionControl<()> {
    /// Reconcile repeated delivery without changing the alarm. For new work,
    /// `arm` must acknowledge a persisted wakeup for this target before any
    /// input is accepted. A failed arm cannot admit the input. A wakeup left
    /// behind by failed admission is harmless: it must not replay the input.
    /// Use this as the only new-work admission path for the hosted Session.
    pub async fn submit_with_wake<F: Future<Output = Result<()>>>(
        &mut self,
        key: &RequestKey,
        input: Message,
        arm: impl FnOnce(WakeTarget) -> F,
    ) -> Result<RequestAdmission<()>> {
        let plan = self.prepare_request(key.clone(), input).await?;
        if let Some(receipt) = plan.existing() {
            return Ok(RequestAdmission::Existing {
                receipt: receipt.clone(),
                metadata: (),
            });
        }
        self.ensure_replaceable()?;
        ensure!(
            self.agent().recovery_authority().is_some(),
            "scheduled work requires explicit recovery authority"
        );
        ensure!(
            self.session().reader()?.unfinished_turn().await?.is_none(),
            "resolve the unfinished Turn before scheduling new work"
        );
        arm(WakeTarget {
            session: self.session().id(),
            key: key.clone(),
        })
        .await
        .context("arm wakeup before admission")?;
        match self.launch_request(plan, ())? {
            RequestLaunch::Pending(id) => Ok(self.wait_request(id).await?),
            RequestLaunch::Existing { receipt, metadata } => {
                Ok(RequestAdmission::Existing { receipt, metadata })
            }
        }
    }

    /// Rearm before continuing the exact committed cut or returning an
    /// already-owned match. Only the platform which owns the alarm may clear
    /// it, and only if it still owns that target and wake generation.
    pub async fn recover_from_wake<F: Future<Output = Result<()>>>(
        &mut self,
        target: WakeTarget,
        arm: impl FnOnce(WakeTarget) -> F,
    ) -> Result<WakeRecovery> {
        ensure!(
            target.session == self.session().id(),
            "wakeup belongs to another Session"
        );
        let state = self
            .session()
            .reader()?
            .recovery_for_request(&target.key)
            .await?;
        let pending = self.admitting.as_ref().is_some_and(|admission| {
            matches!(&admission.payload, AdmissionPayload::Request { key, .. } if key == &target.key)
        });
        if state.is_none() && !pending {
            return Ok(WakeRecovery::Quiescent);
        }
        // Admission can still be pending even though no receipt is visible.
        // If this wake consumed the only alarm, leave another before returning.
        if pending || self.is_busy() {
            arm(target).await.context("rearm wakeup for owned work")?;
            return Ok(WakeRecovery::AlreadyOwned);
        }
        let state = state.context("missing recovery state for unowned work")?;
        ensure!(
            self.agent().recovery_authority() == Some(&state.authority),
            "wake recovery authority changed"
        );
        arm(target).await.context("rearm wakeup before recovery")?;
        Ok(match self.resume_turn(state.point, ()).await {
            Ok(admission) => WakeRecovery::Resumed(admission),
            Err(failure) => WakeRecovery::Refused(failure),
        })
    }
}
