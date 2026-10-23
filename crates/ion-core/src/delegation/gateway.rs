//! Invocation-scoped admission and retained child custody. Tool waiters never
//! own a started Host callback, nor can they hide a relationship storage fault.
use std::{future::Future, sync::Arc};

use ion_ai::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{CodingSession, CodingSessionError as SessionError};

type Completion = (DelegationOutcome, Option<String>);

pub(crate) struct Gateway {
    site: DelegationSite,
    requests: mpsc::Receiver<DelegationRequest>,
}

impl Gateway {
    pub(crate) fn new(site: DelegationSite) -> (AdmissionPort, Self) {
        let (port, requests) = mpsc::channel(1);
        (port, Self { site, requests })
    }

    pub(crate) async fn run<T: Send>(
        mut self,
        parent: &Arc<CodingSession>,
        stop: &CancellationToken,
        tool: impl Future<Output = T> + Send,
    ) -> Result<T, SessionError> {
        tokio::pin!(tool);
        let mut output = None;
        let mut receipt: Option<DelegationReceipt> = None;
        let mut running: Option<BoxFuture<'_, Completion>> = None;
        let mut reply = None;
        let mut closed = false;
        let mut fault = None;
        loop {
            if output.is_some() && running.is_none() && closed {
                break;
            }
            tokio::select! {
                result = &mut tool, if output.is_none() => {
                    output = Some(result);
                    // Requests already transferred into the bounded port still
                    // drain; a departing waiter cannot revoke accepted custody.
                    self.requests.close();
                }
                request = self.requests.recv(), if !closed && running.is_none() => {
                    let Some(request) = request else { closed = true; continue; };
                    let DelegationRequest { mut task, reply: sender } = request;
                    if let Some(known) = &receipt {
                        let result = if known.record.spec == task.spec && known.record.ownership == task.ownership() {
                            Ok(known.clone())
                        } else {
                            Err("this invocation already delegated different work".into())
                        };
                        let _ = sender.send(result);
                        continue;
                    }
                    if stop.is_cancelled() || fault.is_some() {
                        let _ = sender.send(Err("delegation admission is closed".into()));
                        continue;
                    }
                    if let Some(destination) = task.background.as_mut()
                        && let Err(error) = destination.reserve() {
                        let _ = sender.send(Err(error));
                        continue;
                    }
                    if stop.is_cancelled() {
                        let _ = sender.send(Err("delegation admission is closed".into()));
                        continue;
                    }
                    let ownership = task.ownership();
                    match parent.admit_delegation(self.site, task.spec, ownership) {
                        Ok((record, context)) => {
                            if let Some(slot) = task.background {
                                let result = slot.install(BackgroundJob {
                                    parent: parent.clone(), record: record.clone(), context,
                                    work: task.work, parent_stop: stop.clone(),
                                });
                                match result {
                                    Ok(transferred) => {
                                        receipt = Some(transferred.clone());
                                        let _ = sender.send(Ok(transferred));
                                    }
                                    Err(error) => {
                                        let (message, infrastructure) = match error {
                                            BackgroundStartError::Refused(message) => (message, None),
                                            BackgroundStartError::Infrastructure(error) => (error.to_string(), Some(error)),
                                        };
                                        let outcome = DelegationOutcome::Joined { execution_timed_out: false, receipt: None, error: Some(message.clone()), answer: None };
                                        if let Err(error) = parent.settle_delegation(&record, outcome.clone()) {
                                            fault = Some(error);
                                        } else { fault = infrastructure; }
                                        receipt = Some(DelegationReceipt { record, transferred: false, outcome: Some(outcome) });
                                        if fault.is_some() { stop.cancel(); }
                                        let _ = sender.send(Err(message));
                                    }
                                }
                            } else {
                                let child_stop = stop.child_token();
                                running = Some(Box::pin(run_child(record.clone(), context, task.work, child_stop)));
                                receipt = Some(DelegationReceipt { record, transferred: false, outcome: None });
                                reply = Some(sender);
                            }
                        }
                        Err(SessionError::InvalidForkPoint) => {
                            let _ = sender.send(Err("invalid fork point".into()));
                        }
                        Err(error) => {
                            stop.cancel();
                            fault = Some(error);
                            let _ = sender.send(Err("delegation intent was not committed".into()));
                        }
                    }
                }
                completed = async { running.as_mut().expect("retained child").await }, if running.is_some() => {
                    running = None;
                    let known = receipt.as_mut().expect("admitted child");
                    let (outcome, infrastructure) = completed;
                    match parent.settle_delegation(&known.record, outcome.clone()) {
                        Ok(()) => known.outcome = Some(outcome),
                        Err(error) => { fault = Some(error); }
                    }
                    if fault.is_none() && let Some(error) = infrastructure {
                        fault = Some(SessionError::DelegationInfrastructure(error));
                    }
                    let result = if fault.is_some() {
                        stop.cancel();
                        Err("delegation infrastructure failed; inspect committed facts and effects".into())
                    } else {
                        Ok(known.clone())
                    };
                    if let Some(sender) = reply.take() { let _ = sender.send(result); }
                }
            }
        }
        if let Some(fault) = fault {
            Err(fault)
        } else {
            Ok(output.expect("tool joined before invocation exits"))
        }
    }
}

pub(super) async fn run_child(
    record: DelegationRecord,
    context: Vec<crate::SessionEntry>,
    work: DelegationWork,
    stop: CancellationToken,
) -> Completion {
    if stop.is_cancelled() {
        return (
            DelegationOutcome::Joined {
                execution_timed_out: false,
                receipt: None,
                error: Some("stopped before child materialization".into()),
                answer: None,
            },
            None,
        );
    }
    let child = match CodingSession::materialize_delegation(&record, context) {
        Ok(child) => Arc::new(child),
        Err(error) => {
            let message = format!(
                "child materialization failed at {}: {error}",
                record.path.display()
            );
            return (
                DelegationOutcome::Joined {
                    execution_timed_out: false,
                    receipt: None,
                    error: Some(message.clone()),
                    answer: None,
                },
                Some(message),
            );
        }
    };
    let submission = request(&record);
    let key = submission.key.clone();
    // The callback receives the already-linked scope before child admission;
    // Host must use it rather than start with an independent fresh token.
    let completion = work(child.clone(), submission, stop).await;
    let shutdown = child.shutdown().await;
    let receipt = child.reconcile_request(&key, &record.spec.input);
    let mut infrastructure = None;
    let mut errors = Vec::new();
    let mut answer = None;
    if completion.execution_timed_out {
        errors.push("child execution deadline exceeded".into());
    }
    match completion.result {
        Ok(observed) => answer = observed,
        Err(DelegationFailure::Execution(error)) => errors.push(error),
        Err(DelegationFailure::Infrastructure(error)) => {
            infrastructure = Some(error.clone());
            errors.push(error);
        }
    }
    if let Err(error) = shutdown {
        infrastructure = Some(error.to_string());
        errors.push(error.to_string());
    }
    let receipt = match receipt {
        Ok(receipt) => receipt,
        Err(error) => {
            infrastructure = Some(error.to_string());
            errors.push(error.to_string());
            None
        }
    };
    if receipt.as_ref().is_none_or(|receipt| receipt.end.is_none()) && errors.is_empty() {
        let error = "child callback returned without a committed settlement".to_owned();
        infrastructure = Some(error.clone());
        errors.push(error);
    }
    if answer.is_some()
        && receipt
            .as_ref()
            .is_none_or(|receipt| receipt.end != Some(crate::TurnEndReason::Completed))
    {
        let error = "child reported an answer without its completed request".to_owned();
        infrastructure = Some(error.clone());
        errors.push(error);
    }
    if !errors.is_empty() {
        answer = None;
    }
    (
        DelegationOutcome::Joined {
            execution_timed_out: completion.execution_timed_out,
            receipt,
            error: (!errors.is_empty()).then(|| errors.join("; ")),
            answer,
        },
        infrastructure,
    )
}
