//! Framed publication and connection-owned cancellation/settlement.
use std::{collections::VecDeque, io};

use anyhow::{Result, ensure};
use serde_json::Value;
use tokio::{
    io::{AsyncBufRead, AsyncWrite, AsyncWriteExt},
    sync::mpsc,
};

use super::{
    Control, failure,
    input::{CommandReader, Input},
};

const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;

struct Frame {
    bytes: Vec<u8>,
    written: usize,
}

/// One in-flight frame plus a bounded backlog. A single large inspection frame
/// is allowed; it stops further progress acquisition until publication catches
/// up. No writer task is detached, and partial frames survive select cancellation.
pub(super) struct Output<W> {
    writer: W,
    frame: Option<Frame>,
    pending: VecDeque<Vec<u8>>,
    pending_bytes: usize,
    deferred: VecDeque<Value>,
    healthy: bool,
}

impl<W: AsyncWrite + Unpin> Output<W> {
    pub(super) fn new(writer: W) -> Self {
        Self {
            writer,
            frame: None,
            pending: VecDeque::new(),
            pending_bytes: 0,
            deferred: VecDeque::new(),
            healthy: true,
        }
    }

    pub(super) fn enqueue(&mut self, record: Value) -> Result<()> {
        ensure!(self.healthy, "RPC output is unavailable");
        ensure!(
            self.pending_bytes < MAX_PENDING_BYTES,
            "RPC publication backlog exceeds 8 MiB"
        );
        if self.deferred.is_empty() {
            self.encode(record)
        } else {
            // Replies cannot overtake the previous operation's settlement.
            self.deferred.push_back(record);
            Ok(())
        }
    }

    fn encode(&mut self, record: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&record)?;
        bytes.push(b'\n');
        if self.frame.is_none() {
            self.frame = Some(Frame { bytes, written: 0 });
        } else {
            self.pending_bytes += bytes.len();
            self.pending.push_back(bytes);
        }
        Ok(())
    }

    fn defer(&mut self, records: impl IntoIterator<Item = Value>) {
        // These records are already owned by the bounded progress/input paths.
        // Settlement is not fresh admission: retain it until the pipe drains.
        self.deferred.extend(records);
    }

    fn acquire_deferred(&mut self) -> Result<()> {
        while self.pending_bytes < MAX_PENDING_BYTES {
            let Some(record) = self.deferred.pop_front() else {
                break;
            };
            self.encode(record)?;
        }
        Ok(())
    }

    async fn advance(&mut self) -> Result<()> {
        let frame = self.frame.as_mut().expect("pending publication");
        let result = if frame.written < frame.bytes.len() {
            // AsyncWrite::write is cancellation safe. Commit each completed
            // write before yielding, so controls cannot duplicate frame bytes.
            match self.writer.write(&frame.bytes[frame.written..]).await {
                Ok(0) => Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "RPC output stopped accepting bytes",
                )),
                Ok(count) => {
                    frame.written += count;
                    Ok(())
                }
                Err(error) => Err(error),
            }
        } else {
            match self.writer.flush().await {
                Ok(()) => {
                    self.frame = self.pending.pop_front().map(|bytes| {
                        self.pending_bytes -= bytes.len();
                        Frame { bytes, written: 0 }
                    });
                    Ok(())
                }
                Err(error) => Err(error),
            }
        };
        if result.is_err() {
            self.healthy = false;
        }
        result.map_err(Into::into)
    }

    async fn finish(&mut self) -> Result<()> {
        if self.healthy {
            while self.frame.is_some() || !self.deferred.is_empty() {
                self.acquire_deferred()?;
                self.advance().await?;
            }
        }
        // Regular-file publication may have already started native file I/O.
        // Join it even when admission/publication has failed.
        self.writer.flush().await.map_err(Into::into)
    }
}

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
                published = output.advance(), if output.frame.is_some() => {
                    published?;
                }
                Some(record) = events.recv(), if busy && output.deferred.is_empty() && output.pending_bytes < MAX_PENDING_BYTES => {
                    output.enqueue(record)?;
                }
                joined = async { (&mut control.active.as_mut().expect("active operation").task).await }, if busy => {
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
        let joined = (&mut active.task).await;
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
    if output.healthy {
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
