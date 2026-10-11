//! Connection-owned cancellation and settlement.
use super::{
    Control, failure,
    input::{CommandReader, Input},
};
use crate::publication::Output;
use anyhow::Result;
use serde_json::Value;
use tokio::{
    io::{AsyncBufRead, AsyncWrite},
    sync::mpsc,
};

/// Controls and owned settlement remain pollable while publication is stalled.
/// EOF closes admission and joins work before draining final output. As with any
/// pipe protocol, final delivery still requires the recipient to read or close it.
pub(super) async fn connection<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(
    mut control: Control,
    mut input: CommandReader<R>,
    mut events: mpsc::Receiver<Value>,
    mut output: Output<W>,
) -> Result<()> {
    let mut operation_error = None;
    let result: Result<()> = async {
        let mut closing = false;
        loop {
            output.acquire_deferred()?;
            let busy = control.active.is_some();
            let preparing = control.pending_input.is_some();
            tokio::select! {
                published = output.advance(), if output.has_pending() => {
                    published?;
                }
                Some(record) = events.recv(), if busy && output.can_acquire() => {
                    output.enqueue(record)?;
                }
                joined = async { (&mut control.active.as_mut().expect("active operation").completion).await }, if busy => {
                    let completion = control.finish_operation(joined);
                    operation_error = completion.error;
                    while let Ok(record) = events.try_recv() { output.defer([record]); }
                    output.defer(completion.records);
                    if operation_error.is_some() { break; }
                    if !closing { control.start_next_follow_up()?; }
                }
                joined = async { (&mut control.pending_input.as_mut().expect("pending input").task).await }, if preparing => {
                    let completion = control.finish_preparation(joined);
                    operation_error = completion.error;
                    output.defer(completion.records);
                    if operation_error.is_some() { break; }
                    if !closing { control.start_next_follow_up()?; }
                }
                line = input.next(), if !closing => {
                    match line? {
                        Input::Line(line) => {
                            if let Some(record) = control.command(&line) { output.enqueue(record)?; }
                        }
                        Input::TooLarge => output.enqueue(failure(None, "parse", "command exceeds 8 MiB"))?,
                        Input::Incomplete => output.enqueue(failure(None, "parse", "command is missing its final newline"))?,
                        Input::Eof => {
                            closing = true;
                            if let Some(active) = &control.active { active.stop.cancel(); }
                            control.cancel_preparation();
                        }
                    }
                }
            }
            if closing && control.active.is_none() && control.pending_input.is_none() { break; }
        }
        Ok(())
    }.await;

    // Stop admission/publication acquisition before joining. Input failure
    // does not revoke a healthy stdout's ability to deliver terminal/recovery.
    events.close();
    control.cancel_preparation();
    let mut settlement = operation_error.map_or(Ok(()), Err);
    let mut records = Vec::new();
    if let Some(active) = control.active.as_mut() {
        active.stop.cancel();
        let joined = (&mut active.completion).await;
        let completion = control.finish_operation(joined);
        settlement = combine_errors(
            settlement,
            completion.error.map_or(Ok(()), Err),
            "operation settlement also failed",
        );
        records.extend(completion.records);
    }
    if let Some(pending) = control.pending_input.as_mut() {
        let joined = (&mut pending.task).await;
        let completion = control.finish_preparation(joined);
        settlement = combine_errors(
            settlement,
            completion.error.map_or(Ok(()), Err),
            "input settlement also failed",
        );
        records.extend(completion.records);
    }
    let result = combine_errors(result, settlement, "settlement also failed");
    if output.is_healthy() {
        while let Ok(record) = events.try_recv() {
            output.defer([record]);
        }
        output.defer(records);
        output.defer(control.take_uncommitted_follow_ups());
    }
    let publication = output.finish().await;
    combine_errors(result, publication, "RPC recovery output also failed")
}

fn combine_errors(first: Result<()>, second: Result<()>, context: &str) -> Result<()> {
    match (first, second) {
        (Err(error), Err(second)) => Err(error.context(format!("{context}: {second:#}"))),
        (Err(error), _) => Err(error),
        (Ok(()), second) => second,
    }
}
