//! Child preflight has retained custody before Core admits a relationship.
use std::time::Duration;

use anyhow::{Result, ensure};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::PreparedDelegation;
use crate::preparation::Preparation;

/// Pre-admission termination, distinct from a child's execution timeout.
/// Error context retains any preparation failure observed while joining.
#[derive(Debug, thiserror::Error)]
pub enum DelegationPreparationError {
    #[error("child startup cancelled; started preparation has been joined")]
    Cancelled,
    #[error(
        "child startup exceeded {timeout_ms} ms (observed at {elapsed_ms} ms); started preparation has been joined"
    )]
    TimedOut { timeout_ms: u64, elapsed_ms: u64 },
}

/// Retained startup ownership. Cancelling a borrowed join does not remove the
/// worker. Drop requests stop; its custodian still joins started blocking work.
/// Preparation creates no child Session or relationship; admission does that.
pub struct DelegationPreparation {
    inner: Preparation<PreparedDelegation>,
}
impl DelegationPreparation {
    pub(crate) fn start(
        timeout_ms: Option<u64>,
        parent: &CancellationToken,
        work: impl FnOnce(CancellationToken) -> Result<PreparedDelegation> + Send + 'static,
    ) -> Result<Self> {
        let started = Instant::now();
        let deadline = timeout_ms
            .map(|ms| {
                ensure!(ms != 0, "child startup timeout must be positive");
                started
                    .checked_add(Duration::from_millis(ms))
                    .ok_or_else(|| anyhow::anyhow!("child startup timeout exceeds the clock range"))
            })
            .transpose()?;
        let stop = parent.child_token();
        Ok(Self {
            inner: Preparation::start(stop, move |stop| async move {
                check_stop(&stop)?;
                let timed_out = || DelegationPreparationError::TimedOut {
                    timeout_ms: timeout_ms.expect("deadline has a budget"),
                    elapsed_ms: u64::try_from(started.elapsed().as_millis())
                        .expect("monotonic startup fits milliseconds"),
                };
                if deadline.is_some_and(|deadline| deadline <= Instant::now()) {
                    return Err(timed_out().into());
                }
                let worker_stop = stop.clone();
                let mut worker = tokio::task::spawn_blocking(move || {
                    let result = check_stop(&worker_stop).and_then(|()| work(worker_stop));
                    (result, Instant::now())
                });
                let expires = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                };
                tokio::pin!(expires);
                let reason = tokio::select! {
                    biased;
                    () = stop.cancelled() => DelegationPreparationError::Cancelled,
                    result = &mut worker => {
                        let (result, finished) = worker_result(result);
                        return if stop.is_cancelled() {
                            terminated(result, DelegationPreparationError::Cancelled)
                        } else if deadline.is_some_and(|deadline| finished >= deadline) {
                            stop.cancel();
                            terminated(result, timed_out())
                        } else {
                            result
                        };
                    }
                    () = &mut expires => timed_out(),
                };
                stop.cancel();
                // A filesystem call or application hook cannot be forcibly
                // interrupted. Never detach it or publish its late cold task.
                terminated(worker_result(worker.await).0, reason)
            }),
        })
    }

    pub fn request_stop(&self) {
        self.inner.request_stop();
    }

    /// Join once. This borrowed wait can be cancelled and resumed safely.
    pub async fn join(&mut self) -> Result<PreparedDelegation> {
        let result = match self.inner.join().await {
            Err(error)
                if error
                    .downcast_ref::<tokio::task::JoinError>()
                    .is_some_and(tokio::task::JoinError::is_panic) =>
            {
                std::panic::resume_unwind(
                    error
                        .downcast::<tokio::task::JoinError>()
                        .expect("checked panic")
                        .into_panic(),
                )
            }
            result => result,
        };
        // Completion can precede a resumed borrowed wait. Local stop still
        // revokes publication of that retained cold task, not its parent token.
        if result.is_ok() && self.inner.is_stopped() {
            return Err(DelegationPreparationError::Cancelled.into());
        }
        result
    }
}

pub(crate) fn check_stop(stop: &CancellationToken) -> Result<()> {
    if stop.is_cancelled() {
        Err(DelegationPreparationError::Cancelled.into())
    } else {
        Ok(())
    }
}

fn terminated(
    result: Result<PreparedDelegation>,
    reason: DelegationPreparationError,
) -> Result<PreparedDelegation> {
    match result {
        Ok(_) => Err(reason.into()),
        Err(error) => Err(error.context(reason)),
    }
}

fn worker_result(
    result: Result<(Result<PreparedDelegation>, Instant), tokio::task::JoinError>,
) -> (Result<PreparedDelegation>, Instant) {
    match result {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => (Err(error.into()), Instant::now()),
    }
}
