//! Framed publication and connection-owned cancellation/settlement.
use std::io::Write;

use anyhow::{Result, ensure};
use serde_json::Value;
use tokio::{io::AsyncBufRead, sync::mpsc};

use super::{
    Control, failure,
    input::{CommandReader, Input},
};
use crate::write_json_record_to;

/// The connection alone publishes frames. Failed output closes publication,
/// not operation settlement.
struct Output<W> {
    writer: W,
    healthy: bool,
}

impl<W: Write> Output<W> {
    fn write(&mut self, record: &Value) -> Result<()> {
        ensure!(self.healthy, "RPC output is unavailable");
        match write_json_record_to(&mut self.writer, record) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.healthy = false;
                Err(error.into())
            }
        }
    }

    fn records(&mut self, records: impl IntoIterator<Item = Value>) -> Result<()> {
        for record in records {
            self.write(&record)?;
        }
        Ok(())
    }
}

/// Connection exit owns cancellation and joining, including input/output faults.
/// Closing the progress receiver makes further publication fail immediately;
/// the operation still settles its owned work before the connection returns.
pub(super) async fn connection<R: AsyncBufRead + Unpin, W: Write>(
    mut control: Control,
    mut input: CommandReader<R>,
    mut events: mpsc::Receiver<Value>,
    writer: W,
) -> Result<()> {
    let mut output = Output {
        writer,
        healthy: true,
    };
    let mut operation_error = None;
    let result: Result<()> = async {
        let mut closing = false;
        loop {
            let busy = control.active.is_some();
            tokio::select! {
                Some(record) = events.recv(), if busy => {
                    output.write(&record)?;
                }
                joined = async { (&mut control.active.as_mut().expect("active operation").task).await }, if busy => {
                    // A polled-complete handle is consumed once, including panic.
                    let completion = control.finish_operation(joined);
                    operation_error = completion.error;
                    while let Ok(record) = events.try_recv() { output.write(&record)?; }
                    output.records(completion.records)?;
                    if operation_error.is_some() { break; }
                    if !closing { control.start_next_follow_up()?; }
                }
                line = input.next(), if !closing => {
                    match line? {
                        Input::Line(line) => {
                            if let Some(record) = control.command(&line) { output.write(&record)?; }
                        }
                        Input::TooLarge => output.write(&failure(None, "parse", "command exceeds 8 MiB"))?,
                        Input::Incomplete => output.write(&failure(None, "parse", "command is missing its final newline"))?,
                        Input::Eof => {
                            closing = true;
                            if let Some(active) = &control.active { active.stop.cancel(); }
                        }
                    }
                }
            }
            if closing && control.active.is_none() { break; }
        }
        Ok(())
    }.await;

    // Close admission and unblock progress sends before joining. Input failure
    // does not revoke a healthy stdout's ability to deliver terminal/recovery.
    events.close();
    let records = if let Some(active) = control.active.as_mut() {
        active.stop.cancel();
        let joined = (&mut active.task).await;
        let completion = control.finish_operation(joined);
        operation_error = completion.error;
        completion.records
    } else {
        Vec::new()
    };
    let result = combine_errors(
        result,
        operation_error.map_or(Ok(()), Err),
        "settlement also failed",
    );
    let publication = if output.healthy {
        (|| {
            while let Ok(record) = events.try_recv() {
                output.write(&record)?;
            }
            output.records(records)?;
            output.records(control.take_uncommitted_follow_ups())
        })()
    } else {
        Ok(())
    };
    combine_errors(result, publication, "RPC recovery output also failed")
}

fn combine_errors(first: Result<()>, second: Result<()>, context: &str) -> Result<()> {
    match (first, second) {
        (Err(error), Err(second)) => Err(error.context(format!("{context}: {second:#}"))),
        (Err(error), _) => Err(error),
        (Ok(()), second) => second,
    }
}
