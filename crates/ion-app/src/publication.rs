//! Owned, cancellation-safe publication shared by RPC and headless clients.
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{collections::VecDeque, io};
use tokio::io::{AsyncWrite, AsyncWriteExt};

pub(super) enum Record {
    Json(Value),
    Line(String),
}

impl From<Value> for Record {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

impl From<String> for Record {
    fn from(text: String) -> Self {
        Self::Line(text)
    }
}

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
    deferred: VecDeque<Record>,
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

    pub(super) fn has_pending(&self) -> bool {
        self.frame.is_some()
    }
    pub(super) fn is_healthy(&self) -> bool {
        self.healthy
    }
    pub(super) fn can_acquire(&self) -> bool {
        self.healthy && self.deferred.is_empty() && self.pending_bytes < MAX_PENDING_BYTES
    }

    pub(super) fn enqueue(&mut self, record: impl Into<Record>) -> Result<()> {
        ensure!(self.healthy, "output is unavailable");
        ensure!(
            self.pending_bytes < MAX_PENDING_BYTES,
            "publication backlog exceeds 8 MiB"
        );
        if self.deferred.is_empty() {
            self.encode(record.into())
        } else {
            // Replies cannot overtake the previous operation's settlement.
            self.deferred.push_back(record.into());
            Ok(())
        }
    }

    fn encode(&mut self, record: Record) -> Result<()> {
        let mut bytes = match record {
            Record::Json(value) => serde_json::to_vec(&value)?,
            Record::Line(text) => text.into_bytes(),
        };
        bytes.push(b'\n');
        if self.frame.is_none() {
            self.frame = Some(Frame { bytes, written: 0 });
        } else {
            self.pending_bytes += bytes.len();
            self.pending.push_back(bytes);
        }
        Ok(())
    }

    pub(super) fn defer<T: Into<Record>>(&mut self, records: impl IntoIterator<Item = T>) {
        // These records are already owned by the bounded progress/input paths.
        // Settlement is not fresh admission: retain it until the pipe drains.
        self.deferred.extend(records.into_iter().map(Into::into));
    }

    pub(super) fn acquire_deferred(&mut self) -> Result<()> {
        while self.pending_bytes < MAX_PENDING_BYTES {
            let Some(record) = self.deferred.pop_front() else {
                break;
            };
            self.encode(record)?;
        }
        Ok(())
    }

    pub(super) async fn advance(&mut self) -> Result<()> {
        let frame = self.frame.as_mut().expect("pending publication");
        let result = if frame.written < frame.bytes.len() {
            // AsyncWrite::write is cancellation safe. Commit each completed
            // write before yielding, so controls cannot duplicate frame bytes.
            match self.writer.write(&frame.bytes[frame.written..]).await {
                Ok(0) => Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "output stopped accepting bytes",
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

    pub(super) async fn finish(&mut self) -> Result<()> {
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
