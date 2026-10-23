//! Explicit child recipes use ordinary assembly and shared Session control.
use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Result, ensure};
use ion_ai::{Message, ModelRef, Reasoning};
use ion_core::{
    AuthorityScope, CodingAgentError, CodingSessionError, DelegationCompletion, DelegationFailure,
    DelegationSpec, DelegationTask,
};

use crate::{Application, ControlUpdate, OperationOutcome, SessionCatalog, SessionSetup};

/// Explicit child recipe, independent of the parent configuration. The caller
/// authenticates this scope and attests its actual environment and authority.
/// Context sharing is explicit, separate from live model/tools/workspace and
/// credentials. Forking shares historical context facts (including instructions
/// and tool outputs), not execution grants; authorize that disclosure. Matching
/// deferred child definitions can regain exposure without adding parent executors.
/// Tasks default to foreground joining. For explicit transfer, pass
/// `BackgroundOwner::destination()` to `DelegationTask::in_background`; Core
/// reserves custody only after exact reconciliation and before new admission.
pub struct SessionDelegation {
    pub context: ion_core::DelegationContext,
    pub directory: PathBuf,
    pub cwd: Option<PathBuf>,
    pub application: Arc<dyn Application>,
    pub model: Option<ModelRef>,
    pub reasoning: Reasoning,
    /// Runtime budget from linked child request submission; `None` leaves policy
    /// to this application. Expiry requests stop and still joins all cleanup.
    pub execution_timeout_ms: Option<u64>,
    pub authority: AuthorityScope,
}

impl SessionDelegation {
    /// Preflight captures the complete runtime/resources before relationship
    /// admission. The returned task is cold and owns everything needed to join
    /// preparation, Core execution and shutdown outside the tool's borrowed wait.
    pub fn prepare(&self, input: Message) -> Result<DelegationTask> {
        ensure!(
            self.directory.is_absolute(),
            "child directory must be absolute"
        );
        if let Some(ms) = self.execution_timeout_ms {
            ensure!(ms != 0, "child execution timeout must be positive");
            ensure!(
                tokio::time::Instant::now()
                    .checked_add(Duration::from_millis(ms))
                    .is_some(),
                "child execution timeout exceeds the clock range"
            );
        }
        let cwd = self.cwd.as_deref().map(std::fs::canonicalize).transpose()?;
        ensure!(
            cwd.as_ref().is_none_or(|cwd| cwd.is_dir()),
            "child cwd must be a directory"
        );
        let resources = self.application.resources(cwd.as_deref())?;
        let runtime = self
            .application
            .runtime(cwd.as_deref(), self.model.as_ref(), None)?;
        crate::binding::validate_model(&runtime, self.model.as_ref())?;
        runtime.agent.validate_reasoning(self.reasoning)?;
        let spec = DelegationSpec {
            context: self.context,
            directory: self.directory.clone(),
            cwd: cwd.clone(),
            model: runtime.agent.model().clone(),
            reasoning: self.reasoning,
            execution_timeout_ms: self.execution_timeout_ms,
            authority: self.authority.clone(),
            input,
        };
        let catalog = SessionCatalog::new(self.directory.clone(), cwd);
        let application = self.application.clone();
        let reasoning = self.reasoning;
        let execution_timeout_ms = self.execution_timeout_ms;
        DelegationTask::new(spec, move |child, request, stop| {
            Box::pin(async move {
                // Keep deadline provenance even if stop/join or cleanup fails.
                let mut execution_timed_out = false;
                let result = async {
                    let child_id = child.id();
                    let processes = runtime.processes.clone();
                    let setup = SessionSetup::from_prepared(
                        child,
                        catalog,
                        application,
                        resources,
                        runtime,
                        Some(reasoning),
                        None,
                    )
                    .map_err(|error| DelegationFailure::Infrastructure(error.to_string()))?;
                    let mut control = setup.into_control::<()>();
                    let deadline = execution_timeout_ms
                        .map(|ms| {
                            tokio::time::Instant::now()
                                .checked_add(Duration::from_millis(ms))
                                .ok_or_else(|| {
                                    DelegationFailure::Execution(
                                        "child execution timeout exceeds the clock range".into(),
                                    )
                                })
                        })
                        .transpose()?;
                    let admission = control.submit_linked_request(
                        &request.key,
                        request.input,
                        (),
                        stop.clone(),
                    );
                    let mut failure = admission
                        .err()
                        .map(|rejected| admission_failure(rejected.reason));
                    {
                        let expires = async {
                            match deadline {
                                Some(deadline) => tokio::time::sleep_until(deadline).await,
                                None => std::future::pending().await,
                            }
                        };
                        tokio::pin!(expires);
                        while control.is_busy() {
                            tokio::select! {
                                biased;
                                () = stop.cancelled() => { control.request_stop(); break; }
                                () = &mut expires => {
                                    execution_timed_out = true;
                                    control.request_stop();
                                    break;
                                }
                                _ = control.maintain() => {}
                            }
                        }
                    }
                    // Completion facts precede Host settlement. Always join the whole
                    // owner, even after admission refusal or a parent stop.
                    let report = control.shutdown().await;
                    // The child cannot transfer a process grant by returning its ID.
                    // Close this quiescent scope before releasing its answer, even
                    // when its Core Turn ended normally or its waiter was cancelled.
                    if let Some(processes) = processes
                        && let Err(error) = processes.close_session(child_id).await
                    {
                        retain_failure(
                            &mut failure,
                            DelegationFailure::Infrastructure(error.to_string()),
                        );
                    }
                    let mut answer = None;
                    for update in report.updates {
                        if let ControlUpdate::Settled(settlement) = update
                            && let OperationOutcome::Turn(outcome) = settlement.outcome
                        {
                            match outcome {
                                Ok(text) => answer = Some(text),
                                Err(error) => {
                                    let error = if infrastructure(&error) {
                                        DelegationFailure::Infrastructure(error.to_string())
                                    } else {
                                        DelegationFailure::Execution(error.to_string())
                                    };
                                    retain_failure(&mut failure, error);
                                }
                            }
                        }
                    }
                    for error in report.cleanup_errors {
                        retain_failure(
                            &mut failure,
                            DelegationFailure::Infrastructure(error.to_string()),
                        );
                    }
                    if let Some(failure) = failure {
                        return Err(failure);
                    }
                    answer.map(Some).ok_or_else(|| {
                        DelegationFailure::Infrastructure(
                            "child control joined without its successful answer".into(),
                        )
                    })
                }
                .await;
                DelegationCompletion {
                    result,
                    execution_timed_out,
                }
            })
        })
        .map_err(anyhow::Error::msg)
    }
}

fn infrastructure(error: &CodingAgentError) -> bool {
    matches!(
        error,
        CodingAgentError::Session(_)
            | CodingAgentError::ObserverPanicked
            | CodingAgentError::ObserverSettlementFailed(_)
    )
}

fn admission_failure(error: anyhow::Error) -> DelegationFailure {
    // Anyhow context preserves root downcasts, but standard error wrappers can
    // put the Session fault deeper in the causal chain. Neither form may turn
    // an infrastructure failure into a catchable tool refusal.
    if error.chain().any(|cause| {
        cause.is::<CodingSessionError>()
            || cause
                .downcast_ref::<CodingAgentError>()
                .is_some_and(infrastructure)
    }) {
        DelegationFailure::Infrastructure(error.to_string())
    } else {
        DelegationFailure::Execution(error.to_string())
    }
}

fn retain_failure(current: &mut Option<DelegationFailure>, next: DelegationFailure) {
    let Some(previous) = current.take() else {
        *current = Some(next);
        return;
    };
    let infrastructure = matches!(previous, DelegationFailure::Infrastructure(_))
        || matches!(next, DelegationFailure::Infrastructure(_));
    let (DelegationFailure::Execution(previous) | DelegationFailure::Infrastructure(previous)) =
        previous;
    let (DelegationFailure::Execution(next) | DelegationFailure::Infrastructure(next)) = next;
    let errors = format!("{previous}; {next}");
    *current = Some(if infrastructure {
        DelegationFailure::Infrastructure(errors)
    } else {
        DelegationFailure::Execution(errors)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_faults_in_causal_chains_remain_infrastructure_failures() {
        for error in [
            anyhow::Error::new(CodingSessionError::InvalidHistory).context("receipt lookup"),
            anyhow::Error::new(CodingAgentError::Session(
                CodingSessionError::InvalidHistory,
            )),
            anyhow::Error::new(ion_core::BackgroundStartError::Infrastructure(
                CodingSessionError::InvalidHistory,
            ))
            .context("install child"),
        ] {
            assert!(matches!(
                admission_failure(error),
                DelegationFailure::Infrastructure(_)
            ));
        }
        let message = CodingSessionError::InvalidHistory.to_string();
        for error in [
            anyhow::Error::msg(message.clone()),
            anyhow::Error::new(ion_core::BackgroundStartError::Refused(message))
                .context("install child"),
            anyhow::Error::new(CodingAgentError::IncompleteModelResponse).context("child request"),
        ] {
            assert!(matches!(
                admission_failure(error),
                DelegationFailure::Execution(_)
            ));
        }
    }

    #[test]
    fn independent_cleanup_failures_remain_infrastructure_failures() {
        let mut failure = Some(admission_failure(anyhow::anyhow!("ordinary refusal")));
        assert!(matches!(failure, Some(DelegationFailure::Execution(_))));
        retain_failure(
            &mut failure,
            DelegationFailure::Infrastructure("first cleanup".into()),
        );
        retain_failure(
            &mut failure,
            DelegationFailure::Infrastructure("second cleanup".into()),
        );
        assert!(
            matches!(failure, Some(DelegationFailure::Infrastructure(error)) if error == "ordinary refusal; first cleanup; second cleanup")
        );
    }
}
