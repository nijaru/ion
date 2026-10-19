//! Foreground publication of shared Host control, not another execution owner.
use std::io;

use anyhow::{Result, anyhow};
use ion_ai::Message;
use ion_core::{CodingAgentError, CodingAgentEvent};
use ion_host::{
    ControlUpdate, Delivery, Observation, OperationOutcome, ReturnReason, SessionControl,
    SessionSetup, UserInput,
};
use serde_json::json;
use tokio::io::Interest;

use crate::{agent_events, publication::Output, stdio::Stdio};

pub(super) enum Request {
    Prompt(Message),
    Compact,
}

pub(super) async fn run(setup: SessionSetup, request: Request, json_output: bool) -> Result<()> {
    let mut control = setup.into_control::<()>();
    let application_owner = control.owner();
    let result = foreground(&mut control, request, json_output).await;
    if let Some(owner) = &application_owner {
        owner.request_shutdown();
    }
    let report = control.shutdown().await;
    let mut result = result;
    if let Some(owner) = &application_owner {
        result = combine(
            result,
            owner.join_shutdown().await,
            "application shutdown also failed",
        );
    }
    for error in report.cleanup_errors {
        result = combine(result, Err(error.into()), "Session shutdown also failed");
    }
    for update in report.updates {
        if let ControlUpdate::Settled(settlement) = update {
            result = combine(
                result,
                answer(settlement.outcome).map(|_| ()),
                "settlement also failed",
            );
        }
    }
    result
}

async fn foreground(
    control: &mut SessionControl,
    request: Request,
    json_output: bool,
) -> Result<()> {
    // Capture both streams before changing flags: `2>&1` can alias them.
    let (stdout, stderr) = Stdio::open(
        (io::stdout(), Interest::WRITABLE),
        (io::stderr(), Interest::WRITABLE),
    )?;
    let mut stdout = Output::new(stdout);
    let mut stderr = Output::new(stderr);
    let output = if json_output {
        &mut stdout
    } else {
        &mut stderr
    };
    if json_output {
        output.enqueue(
            json!({"type":"session","id":control.session_id(),"cwd":control.session().cwd()}),
        )?;
    } else {
        output.enqueue(format!("[session: {}]", control.session_id()))?;
    }
    // Install before scheduling work; a listener error cannot strand an operation.
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    match request {
        Request::Prompt(message) => {
            control
                .submit(Delivery::Start, UserInput::Message(message), ())
                .map_err(|rejected| rejected.reason)?;
        }
        Request::Compact => {
            control.compact(()).map_err(|(_, error)| error)?;
        }
    }
    let mut listening = true;
    let mut publication_error = None;
    let mut rejected = None;
    let mut omitted = 0u64;
    // Once this publisher loses a prefix, suppress the remaining provisional
    // stream. The reliable committed final below is the reconciliation boundary.
    let mut suppress_progress = false;
    let application_owner = control.owner();
    let request_shutdown = || {
        if let Some(owner) = &application_owner {
            owner.request_shutdown();
        }
    };
    let background = control.background();
    let mut background_revision = 0;
    let mut parent_result = None;
    let result = loop {
        if parent_result.is_some() && !background.as_ref().is_some_and(|owner| owner.has_work()) {
            let result = parent_result.take().expect("settled parent");
            break match &background {
                Some(owner) => match (result, owner.check()) {
                    (result, Ok(())) => result,
                    (Ok(_), Err(error)) => Err(error.into()),
                    (Err(parent), Err(error)) => Err(anyhow::Error::new(error)
                        .context(format!("parent settlement also failed: {parent:#}"))),
                },
                None => result,
            };
        }
        tokio::select! {
            _ = signal.recv(), if listening => {
                listening = false;
                control.request_stop();
                request_shutdown();
                if parent_result.is_some() { parent_result = Some(Err(CodingAgentError::Cancelled.into())); }
            }
            published = output.advance(), if output.is_healthy() && output.has_pending() => {
                if let Err(error) = published {
                    publication_error = Some(error);
                    control.request_stop();
                    request_shutdown();
                }
            }
            revision = async { background.as_ref().expect("guarded owner").changed(background_revision).await }, if background.is_some() => {
                background_revision = revision;
                let owner = background.as_ref().expect("guarded owner");
                if owner.check().is_err() { control.request_stop(); request_shutdown(); }
                // Invalidation only: actual results remain in bounded owner
                // custody and committed facts, independent of this publisher.
                if output.can_acquire() {
                    let record = if json_output { output.enqueue(json!({"type":"background_changed","revision":revision,"resync":"inspect"})) }
                        else { output.enqueue("[background task state changed; results remain inspectable]".to_owned()) };
                    if let Err(error) = record { publication_error = Some(error); control.request_stop(); request_shutdown(); }
                }
            }
            update = control.next(), if parent_result.is_none() => match update {
                ControlUpdate::Activity(Observation::Event { event, .. }) => {
                    // Final is delivered reliably from settlement, never twice
                    // and never dependent on a provisional observation ring.
                    if matches!(event.as_ref(), CodingAgentEvent::Final(_)) { continue; }
                    if suppress_progress || !output.can_acquire() {
                        omitted = omitted.saturating_add(1);
                        suppress_progress = true;
                        continue;
                    }
                    let queued = if json_output {
                        output.enqueue(agent_events::event_record(&event))
                    } else if let Some(text) = agent_events::diagnostic(&event) {
                        output.enqueue(text)
                    } else { Ok(()) };
                    if let Err(error) = queued {
                        publication_error = Some(error);
                        control.request_stop();
                        request_shutdown();
                    }
                }
                ControlUpdate::Activity(Observation::Gap { skipped }) => {
                    omitted = omitted.saturating_add(skipped);
                    suppress_progress = true;
                }
                ControlUpdate::Returned(input) => {
                    rejected = Some(match input.reason {
                        ReturnReason::Preparation(error) => error,
                        ReturnReason::Stopped => CodingAgentError::Cancelled.into(),
                        reason => anyhow!("input was not accepted: {reason:?}"),
                    });
                    if let Some(error) = input.preparation_error {
                        rejected = rejected.map(|first| first.context(format!("preparation also failed: {error:#}")));
                    }
                }
                ControlUpdate::Settled(settlement) => {
                    let result = answer(settlement.outcome);
                    if result.is_err() { request_shutdown(); }
                    parent_result = Some(result);
                }
                ControlUpdate::Idle => {
                    request_shutdown();
                    parent_result = Some(Err(rejected.take().unwrap_or_else(|| anyhow!("operation ended without settlement"))));
                }
                ControlUpdate::Input { .. }
                | ControlUpdate::Dispatched { .. }
                | ControlUpdate::Accepted { .. }
                | ControlUpdate::QuestionChanged => {}
            }
        }
    };

    // Delegated work has settled; managed servers must now stop as well.
    // Final publication may block indefinitely on an otherwise healthy pipe.
    let result = if let Some(owner) = &application_owner {
        match (result, owner.join_shutdown().await) {
            (result, Ok(())) => result,
            (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(cleanup)) => {
                Err(error.context(format!("application shutdown also failed: {cleanup:#}")))
            }
        }
    } else {
        result
    };
    let mut publication = output.finish().await;
    if publication.is_ok() && output.is_healthy() {
        if suppress_progress {
            let notice = if json_output {
                output
                    .enqueue(json!({"type":"observation_gap","skipped":omitted,"resync":"inspect"}))
            } else {
                output.enqueue(
                    "[progress omitted; committed Session history remains inspectable]".to_owned(),
                )
            };
            publication = combine(publication, notice, "gap notice also failed");
            publication = combine(
                publication,
                output.finish().await,
                "gap publication also failed",
            );
        }
        if json_output && publication.is_ok() {
            if let Ok(text) = &result {
                publication = output.enqueue(json!({"type":"final","text":text}));
                publication = combine(
                    publication,
                    output.finish().await,
                    "final publication also failed",
                );
            }
            if publication.is_ok() {
                let status = match &result {
                    Ok(_) => "completed",
                    Err(error)
                        if matches!(
                            error.downcast_ref::<CodingAgentError>(),
                            Some(CodingAgentError::Cancelled)
                        ) =>
                    {
                        "cancelled"
                    }
                    Err(_) => "failed",
                };
                let mut end = json!({"type":"run_end","status":status});
                if let Err(error) = &result {
                    end["error"] = json!(format!("{error:#}"));
                }
                publication = output.enqueue(end);
                publication = combine(
                    publication,
                    output.finish().await,
                    "terminal publication also failed",
                );
            }
        }
    }
    if let Some(error) = publication_error {
        publication = combine(Err(error), publication, "output settlement also failed");
    }
    let text = combine(result, publication, "publication also failed")?;
    if !json_output {
        stdout.enqueue(text)?;
        stdout.finish().await?;
    }
    Ok(())
}

fn answer(outcome: OperationOutcome) -> Result<String> {
    match outcome {
        OperationOutcome::Turn(result) => result.map_err(Into::into),
        OperationOutcome::Compact(result) => result
            .map(|changed| {
                if changed {
                    "Context summarized; raw Session history retained"
                } else {
                    "No settled history to summarize"
                }
                .to_owned()
            })
            .map_err(Into::into),
        OperationOutcome::Shell(_) => unreachable!("headless request never dispatches shell"),
    }
}

fn combine<T>(first: Result<T>, second: Result<()>, context: &str) -> Result<T> {
    match (first, second) {
        (Err(error), Err(second)) => Err(error.context(format!("{context}: {second:#}"))),
        (Err(error), _) => Err(error),
        (Ok(value), Ok(_)) => Ok(value),
        (Ok(_), Err(error)) => Err(error),
    }
}
