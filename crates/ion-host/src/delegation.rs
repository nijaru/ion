//! Explicit child recipes use ordinary assembly and shared Session control.
use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Result, ensure};
use ion_ai::{Message, ModelRef, Reasoning};
use ion_core::{
    AuthorityScope, CodingAgentError, CodingSessionError, DelegationCompletion, DelegationFailure,
    DelegationSpec, DelegationTask,
};

use crate::{Application, ControlUpdate, SessionCatalog, SessionSetup};

mod progress;
pub use progress::{ChildActivity, ChildActivitySnapshot, ChildProgress, ChildProgressSnapshot};
mod report;

mod steering;
pub use steering::{ChildSteering, ChildSteeringReport, PreparedDelegation};

/// Explicit child recipe, independent of the parent configuration. The caller
/// authenticates this scope and attests its actual environment and authority.
/// Context sharing is explicit, separate from live model/tools/workspace and
/// credentials. Forking shares historical context facts (including instructions
/// and tool outputs), not execution grants; authorize that disclosure. Matching
/// deferred child definitions can regain exposure without adding parent executors.
/// Use the prepared task for foreground joining, or transfer it with
/// `PreparedDelegation::in_background` to retain its matching live control grant.
/// Core reserves custody only after exact reconciliation and before new admission.
pub struct SessionDelegation {
    pub target: ion_core::DelegationTarget,
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
    pub fn prepare(&self, input: Message) -> Result<PreparedDelegation> {
        let directory = self
            .target
            .directory()
            .filter(|path| path.is_absolute())
            .ok_or_else(|| anyhow::anyhow!("child target directory must be absolute"))?;
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
            target: self.target.clone(),
            cwd: cwd.clone(),
            model: runtime.agent.model().clone(),
            reasoning: self.reasoning,
            execution_timeout_ms: self.execution_timeout_ms,
            authority: self.authority.clone(),
            input,
        };
        let catalog = SessionCatalog::new(directory.to_path_buf(), cwd);
        let application = self.application.clone();
        let reasoning = self.reasoning;
        let execution_timeout_ms = self.execution_timeout_ms;
        let (steering, ready) = ChildSteering::new(runtime.agent.limits());
        let progress = ChildProgress::default();
        let observed = progress.clone();
        let task = DelegationTask::new(spec, move |child, request, stop| {
            Box::pin(async move {
                // Runtime ends at observed stop/settlement, not after sweeping
                // descendants. Both the report and timeout cause survive cleanup.
                let mut execution_timed_out = false;
                let mut elapsed_ms = None;
                let mut report = report::ChildReport::new(observed);
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
                    let started = tokio::time::Instant::now();
                    report
                        .progress
                        .start(child_id, request.key.clone(), started);
                    let elapsed = || {
                        u64::try_from(started.elapsed().as_millis())
                            .expect("monotonic runtime fits milliseconds")
                    };
                    let deadline = execution_timeout_ms
                        .map(|ms| {
                            started
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
                    if let Some(port) = control.steering() {
                        ready.send_replace(Some((child_id, request.key.clone(), port)));
                    }
                    report.failure = admission
                        .err()
                        .map(|rejected| admission_failure(rejected.reason));
                    let expires = async {
                        match deadline {
                            Some(deadline) => tokio::time::sleep_until(deadline).await,
                            None => std::future::pending().await,
                        }
                    };
                    tokio::pin!(expires);
                    let mut stopping = false;
                    while control.is_busy() {
                        tokio::select! {
                            biased;
                            () = stop.cancelled(), if !stopping => {
                                let runtime = elapsed();
                                elapsed_ms = Some(runtime);
                                report.progress.stop(runtime);
                                stopping = true;
                                control.request_shutdown();
                            }
                            () = &mut expires, if !stopping => {
                                let runtime = elapsed();
                                elapsed_ms = Some(runtime);
                                report.progress.stop(runtime);
                                execution_timed_out = true;
                                stopping = true;
                                control.request_shutdown();
                            }
                            update = control.next() => {
                                if matches!(update, ControlUpdate::Settled(_)) {
                                    let runtime = *elapsed_ms.get_or_insert_with(elapsed);
                                    report.progress.freeze(runtime);
                                }
                                report.observe(update);
                            }
                        }
                    }
                    report
                        .progress
                        .freeze(*elapsed_ms.get_or_insert_with(elapsed));
                    // Still join the whole owner and collect reliable settlements.
                    // Unlike shutdown's discarded provisional stream, next() above
                    // preserves safe visible progress while stopped cleanup joins.
                    let shutdown = control.shutdown().await;
                    for update in shutdown.updates {
                        report.observe(update);
                    }
                    if let Some(processes) = processes
                        && let Err(error) = processes.close_session(child_id).await
                    {
                        report.fail(DelegationFailure::Infrastructure(error.to_string()));
                    }
                    for error in shutdown.cleanup_errors {
                        report.fail(DelegationFailure::Infrastructure(error.to_string()));
                    }
                    report.take_result()
                }
                .await;
                DelegationCompletion {
                    result,
                    execution_timed_out,
                    elapsed_ms,
                    partial_output: report.output(),
                }
            })
        })
        .map_err(anyhow::Error::msg)?;
        Ok(PreparedDelegation {
            task,
            steering,
            progress,
            completion: None,
        })
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
