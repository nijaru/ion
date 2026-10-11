//! One Core operation, with independently pollable publication and cancellation.
use std::{
    io,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use ion_ai::Message;
use ion_core::{CodingAgent, CodingAgentError, CodingSession};
use ion_host::image_input::LoadedImage;
use serde_json::json;
use tokio::{io::Interest, sync::mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    agent_events,
    publication::{Output, Record},
    stdio::Stdio,
};

pub(super) async fn run(
    session: Arc<CodingSession>,
    agent: Arc<CodingAgent>,
    instructions: String,
    prompt: String,
    images: Vec<LoadedImage>,
    json_output: bool,
) -> Result<()> {
    let id = session
        .path()
        .file_stem()
        .context("session has no ID")?
        .to_string_lossy();
    // Capture both streams before changing flags: `2>&1` may alias them.
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
        output.enqueue(json!({"type":"session","id":id,"cwd":session.cwd()}))?;
    } else {
        output.enqueue(format!("[session: {id}]"))?;
    }
    let stop = CancellationToken::new();
    let (sender, mut events) = mpsc::channel(128);
    let observer_error = Arc::new(Mutex::new(None));
    let observer_fault = observer_error.clone();
    let observer_stop = stop.clone();
    // Install the signal handler before scheduling the owned Core task.
    let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    let mut publication_error = None;
    let result = {
        let operation = agent.submit_message(
            session.clone(),
            Message::user_input(prompt, images),
            instructions,
            stop.clone(),
            move |event| {
                let mut observer_error = observer_fault
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if observer_error.is_some() {
                    return;
                }
                let record = if json_output {
                    Some(Record::Json(agent_events::event_record(event)))
                } else {
                    agent_events::diagnostic(event).map(Record::Line)
                };
                if observer_error.is_none()
                    && let Some(record) = record
                    && let Err(error) = sender.try_send(record)
                {
                    *observer_error = Some(anyhow!("headless progress is unavailable: {error}"));
                    observer_stop.cancel();
                }
            },
        );
        tokio::pin!(operation);
        let mut listening = true;
        loop {
            if output.is_healthy()
                && let Err(error) = output.acquire_deferred()
            {
                publication_error.get_or_insert(error);
                stop.cancel();
            }
            tokio::select! {
                // Register the signal handler before polling Core. Once stop is
                // requested, keep polling Core until all owned work settles.
                biased;
                _ = signal.recv(), if listening => {
                    listening = false;
                    stop.cancel();
                }
                Some(record) = events.recv(), if publication_error.is_none() && output.can_acquire() => {
                    if let Err(error) = output.enqueue(record) {
                        publication_error.get_or_insert(error);
                        stop.cancel();
                    }
                }
                published = output.advance(), if output.is_healthy() && output.has_pending() => {
                    if let Err(error) = published {
                        publication_error.get_or_insert(error);
                        stop.cancel();
                    }
                }
                result = &mut operation => break result,
            }
        }
    };
    // Core has settled. These are already admitted records, not fresh progress
    // admission; high-water pressure cannot revoke their delivery eligibility.
    events.close();
    if output.is_healthy() {
        while let Ok(record) = events.try_recv() {
            output.defer([record]);
        }
        if json_output {
            let ending = match &result {
                Ok(_) => json!({"type":"run_end","status":"completed"}),
                Err(CodingAgentError::Cancelled) => {
                    json!({"type":"run_end","status":"cancelled","error":"turn was cancelled"})
                }
                Err(error) => json!({"type":"run_end","status":"failed","error":error.to_string()}),
            };
            output.defer([ending]);
        }
    }
    let finish = output.finish().await;
    let observer_error = observer_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(mut error) = publication_error.or(observer_error) {
        if let Err(settlement) = &result
            && !matches!(settlement, CodingAgentError::Cancelled)
        {
            error = error.context(format!("Core operation also failed: {settlement}"));
        }
        return match finish {
            Ok(()) => Err(error),
            Err(second) => Err(error.context(format!("output settlement also failed: {second:#}"))),
        };
    }
    finish?;
    let answer = result?;
    if !json_output {
        stdout.enqueue(answer)?;
        stdout.finish().await?;
    }
    Ok(())
}
