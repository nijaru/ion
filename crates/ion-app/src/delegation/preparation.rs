//! Child preflight has retained custody before Core admits a relationship.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, ensure};
use tokio::{task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

use super::PreparedDelegation;
use crate::preparation::Preparation;

/// Pre-admission termination, distinct from a child's execution timeout.
/// Error context retains any preparation failure observed while joining.
#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum DelegationPreparationError {
    #[error("child startup cancelled; started preparation has been joined")]
    Cancelled,
    #[error(
        "child startup exceeded {timeout_ms} ms (observed at {elapsed_ms} ms); started preparation has been joined"
    )]
    TimedOut { timeout_ms: u64, elapsed_ms: u64 },
}

#[derive(Default)]
enum StartupState {
    #[default]
    Pending,
    Complete,
    Stopped(DelegationPreparationError),
}

// This timer has no native work. The actual blocking worker owns it, including
// when its borrowed waiter is abandoned, and aborts it on completion or panic.
struct DeadlineMonitor(Option<JoinHandle<()>>);
impl Drop for DeadlineMonitor {
    fn drop(&mut self) {
        if let Some(timer) = &self.0 {
            timer.abort();
        }
    }
}

/// Retained startup ownership. Cancelling a borrowed join does not remove the
/// actual blocking task. Drop requests stop; its custodian retains that task
/// through physical completion. No child Session or relationship is created.
pub struct DelegationPreparation {
    inner: Preparation<PreparedDelegation>,
    state: Arc<Mutex<StartupState>>,
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
        let state = Arc::new(Mutex::new(StartupState::Pending));
        let stop = parent.child_token();
        let timed_out = move || DelegationPreparationError::TimedOut {
            timeout_ms: timeout_ms.expect("deadline has a budget"),
            elapsed_ms: u64::try_from(started.elapsed().as_millis())
                .expect("monotonic startup fits milliseconds"),
        };
        let timer = DeadlineMonitor(deadline.map(|deadline| {
            let state = state.clone();
            let stop = stop.clone();
            let parent = parent.clone();
            tokio::spawn(async move {
                let reason = tokio::select! {
                    biased;
                    () = parent.cancelled() => DelegationPreparationError::Cancelled,
                    () = tokio::time::sleep_until(deadline) => timed_out(),
                };
                let pending = {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if matches!(*state, StartupState::Pending) {
                        *state = StartupState::Stopped(reason);
                        true
                    } else {
                        false
                    }
                };
                if pending {
                    stop.cancel();
                }
            })
        }));
        let completion = state.clone();
        let parent = parent.clone();
        Ok(Self {
            state,
            inner: Preparation::start_blocking(stop, move |stop| {
                let _timer = timer;
                let result = check_stop(&stop).and_then(|()| work(stop.clone()));
                let reason = {
                    let mut state = completion
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if matches!(*state, StartupState::Pending) {
                        *state = if parent.is_cancelled() || stop.is_cancelled() {
                            StartupState::Stopped(DelegationPreparationError::Cancelled)
                        } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                            StartupState::Stopped(timed_out())
                        } else {
                            StartupState::Complete
                        };
                    }
                    match *state {
                        StartupState::Stopped(reason) => Some(reason),
                        _ => None,
                    }
                };
                match reason {
                    Some(reason) => terminated(result, reason),
                    None => result,
                }
            }),
        })
    }

    pub fn request_stop(&self) {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(*state, StartupState::Pending) {
                *state = StartupState::Stopped(DelegationPreparationError::Cancelled);
            }
        }
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
        // A later explicit stop revokes a completed cold task, not its parent
        // token. It cannot replace an earlier retained timeout or failure.
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
