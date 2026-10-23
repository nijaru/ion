//! Invocation-scoped admission and retained child custody. A bounded foreground
//! join moves the same pinned execution only after durable background transfer.
use std::{future::Future, sync::Arc};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::{CodingSession, CodingSessionError as SessionError};

pub(crate) struct Gateway {
    site: DelegationSite,
    requests: mpsc::Receiver<DelegationRequest>,
}

struct PendingTransfer {
    slot: Box<dyn BackgroundSlot>,
    deadline: tokio::time::Instant,
}
struct Running {
    job: BackgroundJob,
    reply: tokio::sync::oneshot::Sender<Result<DelegationReceipt, String>>,
    transfer: Option<PendingTransfer>,
    stopping: bool,
    transfer_error: Option<String>,
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
        let mut running: Option<Running> = None;
        let mut closed = false;
        let mut fault = None;
        loop {
            if output.is_some() && running.is_none() && closed {
                break;
            }
            let deadline = running
                .as_ref()
                .and_then(|run| run.transfer.as_ref().map(|p| p.deadline));
            let can_stop = running.as_ref().is_some_and(|run| !run.stopping);
            let has_run = running.is_some();
            let (job, reply) = match running.as_mut() {
                Some(run) => (Some(&mut run.job), Some(&mut run.reply)),
                None => (None, None),
            };
            tokio::select! {
                biased;
                () = stop.cancelled(), if can_stop => {
                    let run = running.as_mut().expect("retained foreground child");
                    run.stopping = true;
                    run.transfer = None;
                    run.job.request_stop();
                }
                // Abandonment cannot silently release foreground joining custody.
                // It does not stop accepted work, but removes conditional transfer.
                () = async { reply.expect("pending foreground waiter").closed().await }, if deadline.is_some() => {
                    running.as_mut().expect("retained child").transfer = None;
                }
                // Actual completion wins a ready join-budget race, including cleanup.
                completed = async { job.expect("retained child").join().await }, if has_run => {
                    let run = running.take().expect("joined foreground child");
                    let known = receipt.as_mut().expect("admitted child");
                    let (mut outcome, infrastructure) = completed;
                    if let Some(message) = &run.transfer_error
                        && let DelegationOutcome::Joined { error, answer, partial_output, .. } = &mut outcome
                    {
                        *error = Some(match error.take() {
                            Some(error) => format!("{error}; {message}"),
                            None => message.clone(),
                        });
                        if let Some(text) = answer.take() {
                            let mut output = DelegationPartialOutput::default();
                            output.push_text(&text);
                            *partial_output = Some(output);
                        }
                    }
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
                    } else if let Some(error) = run.transfer_error {
                        Err(error)
                    } else { Ok(known.clone()) };
                    let _ = run.reply.send(result);
                }
                () = async { tokio::time::sleep_until(deadline.expect("pending transfer deadline")).await }, if deadline.is_some() => {
                    let run = running.as_mut().expect("retained foreground child");
                    let transfer = run.transfer.take().expect("pending destination");
                    // A closure racing the select can still be observed before
                    // installation; like stop racing commit, later closure may lose.
                    if run.reply.is_closed() { continue; }
                    match transfer.slot.install(&mut run.job) {
                        Ok(transferred) => {
                            let run = running.take().expect("transferred child");
                            receipt = Some(transferred.clone());
                            let _ = run.reply.send(Ok(transferred));
                        }
                        Err(error) => {
                            // Installation did not consume execution. Stop and join
                            // it here; neither rejection nor storage failure may
                            // drop an already-started callback or release the parent.
                            let (message, infrastructure) = start_error(error);
                            run.transfer_error = Some(message);
                            run.stopping = true;
                            run.job.request_stop();
                            if let Some(error) = infrastructure { fault = Some(error); stop.cancel(); }
                        }
                    }
                }
                result = &mut tool, if output.is_none() => {
                    output = Some(result);
                    // Drain already-accepted requests even if their waiter departs.
                    self.requests.close();
                }
                request = self.requests.recv(), if !closed && !has_run => {
                    let Some(request) = request else { closed = true; continue; };
                    let DelegationRequest { mut task, reply: sender } = request;
                    if let Some(known) = &receipt {
                        let result = if known.record.spec == task.spec && known.record.ownership == task.ownership() {
                            Ok(known.clone())
                        } else { Err("this invocation already delegated different work".into()) };
                        let _ = sender.send(result);
                        continue;
                    }
                    if stop.is_cancelled() || fault.is_some() {
                        let _ = sender.send(Err("delegation admission is closed".into()));
                        continue;
                    }
                    let child_stop = match task.background.as_mut() {
                        Some(destination) => match destination.slot.reserve() {
                            Ok(stop) => stop,
                            Err(error) => { let _ = sender.send(Err(error)); continue; }
                        },
                        None => CancellationToken::new(),
                    };
                    if stop.is_cancelled() || child_stop.is_cancelled() {
                        let _ = sender.send(Err("delegation admission is closed".into()));
                        continue;
                    }
                    // This budget observes the live admission/join, including
                    // materialization and Host setup, not synchronous recipe preflight.
                    let deadline = match task.background.as_ref().filter(|p| !p.join_timeout.is_zero()) {
                        Some(plan) => match tokio::time::Instant::now().checked_add(plan.join_timeout) {
                            Some(deadline) => Some(deadline),
                            None => { let _ = sender.send(Err("join budget exceeds the clock range".into())); continue; }
                        },
                        None => None,
                    };
                    let ownership = task.ownership();
                    match parent.admit_delegation(self.site, task.spec, ownership) {
                        Ok((record, prepared)) => {
                            let mut job = BackgroundJob::new(parent.clone(), record.clone(), prepared, task.work, child_stop, stop.clone());
                            let transfer = match task.background {
                                Some(plan) if plan.join_timeout.is_zero() => {
                                    match plan.slot.install(&mut job) {
                                        Ok(transferred) => {
                                            receipt = Some(transferred.clone());
                                            let _ = sender.send(Ok(transferred));
                                        }
                                        Err(error) => {
                                            // Immediate background execution is still cold:
                                            // no callback was polled before this transfer.
                                            let (message, infrastructure) = start_error(error);
                                            let outcome = DelegationOutcome::Joined { execution_timed_out: false, elapsed_ms: None, partial_output: None, receipt: None, error: Some(message.clone()), answer: None };
                                            if let Err(error) = parent.settle_delegation(&record, outcome.clone()) { fault = Some(error); }
                                            else { fault = infrastructure; }
                                            receipt = Some(DelegationReceipt { record, transferred: false, outcome: Some(outcome) });
                                            if fault.is_some() { stop.cancel(); }
                                            let _ = sender.send(Err(message));
                                        }
                                    }
                                    continue;
                                }
                                Some(plan) => Some(PendingTransfer {
                                    slot: plan.slot, deadline: deadline.expect("checked positive join budget"),
                                }),
                                None => None,
                            };
                            receipt = Some(DelegationReceipt { record, transferred: false, outcome: None });
                            running = Some(Running { job, reply: sender, transfer, stopping: false, transfer_error: None });
                        }
                        Err(SessionError::DelegationTargetUnavailable(message)) => { let _ = sender.send(Err(message)); }
                        Err(SessionError::InvalidForkPoint) => { let _ = sender.send(Err("invalid fork point".into())); }
                        Err(error) => {
                            stop.cancel(); fault = Some(error);
                            let _ = sender.send(Err("delegation intent was not committed".into()));
                        }
                    }
                }
            }
        }
        match fault {
            Some(fault) => Err(fault),
            None => Ok(output.expect("tool joined before invocation exits")),
        }
    }
}

fn start_error(error: BackgroundStartError) -> (String, Option<SessionError>) {
    match error {
        BackgroundStartError::Refused(message) => (message, None),
        BackgroundStartError::Infrastructure(error) => (error.to_string(), Some(error)),
    }
}
