//! Owned, cancellation-safe publication shared by RPC and headless clients.
use anyhow::{Result, ensure};
use serde_json::Value;
use std::{collections::VecDeque, io, sync::Arc};
use tokio::{
    io::{AsyncWrite, AsyncWriteExt},
    sync::{OwnedSemaphorePermit, Semaphore},
};

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

/// RPC reserves a reply slot *before* admitting a command. A lease survives in
/// Host metadata and in its finite set of reply frames until all are delivered.
/// This is a transport bound, independent of Host's input/result capacity.
pub(super) struct ReplyCredits(Arc<Semaphore>);
impl ReplyCredits {
    pub(super) fn new(capacity: usize) -> Self {
        Self(Arc::new(Semaphore::new(capacity)))
    }
    pub(super) fn acquire(&self) -> Option<ReplyCredit> {
        self.0.clone().try_acquire_owned().ok().map(Arc::new)
    }
}
pub(super) type ReplyCredit = Arc<OwnedSemaphorePermit>;

struct Frame {
    bytes: Vec<u8>,
    written: usize,
    _credit: Option<ReplyCredit>,
}

/// No writer task and no deferred typed records. Ordinary publication has an
/// 8 MiB high-water mark (one oversized frame is allowed). RPC's reliable frames
/// use pre-reserved reply slots instead; the RPC encoder bounds frames per slot
/// and admits at most one materialized inspection reply at a time.
pub(super) struct Output<W> {
    writer: W,
    frame: Option<Frame>,
    pending: VecDeque<Frame>,
    pending_bytes: usize,
    healthy: bool,
}
impl<W: AsyncWrite + Unpin> Output<W> {
    pub(super) fn new(writer: W) -> Self {
        Self {
            writer,
            frame: None,
            pending: VecDeque::new(),
            pending_bytes: 0,
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
        self.healthy && self.pending_bytes < MAX_PENDING_BYTES
    }

    pub(super) fn enqueue(&mut self, record: impl Into<Record>) -> Result<()> {
        ensure!(
            self.can_acquire(),
            "publication backlog is full or output is unavailable"
        );
        let bytes = encode(record.into())?;
        self.push(bytes, None);
        Ok(())
    }

    /// Passive observations may be discarded, never promoted to execution faults.
    pub(super) fn try_enqueue(&mut self, record: impl Into<Record>) -> Result<bool> {
        if !self.can_acquire() {
            return Ok(false);
        }
        let bytes = encode(record.into())?;
        if bytes.len() > MAX_PENDING_BYTES.saturating_sub(self.pending_bytes) {
            return Ok(false);
        }
        self.push(bytes, None);
        Ok(true)
    }

    pub(super) fn enqueue_reliable(
        &mut self,
        record: impl Into<Record>,
        credit: ReplyCredit,
    ) -> Result<()> {
        ensure!(self.healthy, "output is unavailable");
        self.push(encode(record.into())?, Some(credit));
        Ok(())
    }
    fn push(&mut self, bytes: Vec<u8>, credit: Option<ReplyCredit>) {
        self.pending_bytes += bytes.len();
        let frame = Frame {
            bytes,
            written: 0,
            _credit: credit,
        };
        if self.frame.is_none() {
            self.frame = Some(frame);
        } else {
            self.pending.push_back(frame);
        }
    }
    pub(super) async fn advance(&mut self) -> Result<()> {
        let frame = self.frame.as_mut().expect("pending publication");
        let result = if frame.written < frame.bytes.len() {
            // write is cancellation safe; commit its cursor before the next await.
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
                    self.pending_bytes -= frame.bytes.len();
                    self.frame = self.pending.pop_front();
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
        let mut publication = Ok(());
        while self.healthy && self.has_pending() {
            if let Err(error) = self.advance().await {
                publication = Err(error);
            }
        }
        // Join native regular-file I/O even when the first publication fault
        // happened during final drain, not only after the foreground loop failed.
        match (publication, self.writer.flush().await) {
            (Err(error), Err(flush)) => {
                Err(error.context(format!("final output flush also failed: {flush}")))
            }
            (Err(error), _) => Err(error),
            (Ok(()), result) => result.map_err(Into::into),
        }
    }
}
fn encode(record: Record) -> Result<Vec<u8>> {
    let mut bytes = match record {
        Record::Json(value) => serde_json::to_vec(&value)?,
        Record::Line(text) => text.into_bytes(),
    };
    bytes.push(b'\n');
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn reply_credit_and_partial_cursor_survive_cancelled_writes() {
        let credits = ReplyCredits::new(1);
        let credit = credits.acquire().unwrap();
        let (writer, mut reader) = tokio::io::duplex(3);
        let mut output = Output::new(writer);
        let record = json!({"text":"partial frame"});
        output
            .enqueue_reliable(record.clone(), credit.clone())
            .unwrap();
        output.advance().await.unwrap();
        // Poll the blocked write before cancelling, rather than selecting a
        // branch which never polls publication at all.
        for _ in 0..3 {
            tokio::select! { biased; result = output.advance() => { result.unwrap(); }, _ = std::future::ready(()) => {} }
        }
        drop(credit); // Host custody ended; the frame still owns the lease.
        assert!(credits.acquire().is_none());
        let reading = tokio::spawn(async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        output.finish().await.unwrap();
        assert!(credits.acquire().is_some());
        drop(output);
        let bytes = reading.await.unwrap();
        assert_eq!(bytes, format!("{record}\n").into_bytes());
    }

    #[tokio::test]
    async fn final_drain_failure_still_joins_flush_and_preserves_both_errors() {
        use std::{
            pin::Pin,
            task::{Context, Poll},
        };
        #[derive(Default)]
        struct Faults {
            writes: usize,
            flushes: usize,
        }
        impl AsyncWrite for Faults {
            fn poll_write(
                mut self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &[u8],
            ) -> Poll<io::Result<usize>> {
                self.writes += 1;
                Poll::Ready(Err(io::Error::other("write fault")))
            }
            fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
                self.flushes += 1;
                Poll::Ready(Err(io::Error::other("flush fault")))
            }
            fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                self.poll_flush(cx)
            }
        }
        let mut faults = Faults::default();
        let mut output = Output::new(&mut faults);
        output.enqueue("final".to_owned()).unwrap();
        let error = output.finish().await.unwrap_err();
        let detail = format!("{error:#}");
        assert!(
            detail.contains("write fault") && detail.contains("flush fault"),
            "{detail}"
        );
        assert!(!output.is_healthy());
        drop(output);
        assert_eq!((faults.writes, faults.flushes), (1, 1));
    }

    #[tokio::test]
    async fn passive_high_water_is_nonfatal_and_reliable_capacity_was_pre_reserved() {
        let mut bytes = Vec::new();
        let mut output = Output::new(&mut bytes);
        assert!(!output.try_enqueue("x".repeat(MAX_PENDING_BYTES)).unwrap());
        let large = "x".repeat(MAX_PENDING_BYTES);
        output.enqueue(large.clone()).unwrap(); // One oversized owner frame.
        assert!(!output.can_acquire());
        assert!(!output.try_enqueue(json!({"type":"lost_progress"})).unwrap());
        assert!(output.is_healthy());
        let credits = ReplyCredits::new(1);
        output
            .enqueue_reliable(json!({"type":"settled"}), credits.acquire().unwrap())
            .unwrap();
        assert!(credits.acquire().is_none());
        output.finish().await.unwrap();
        assert!(credits.acquire().is_some());
        drop(output);
        assert_eq!(
            bytes,
            format!("{large}\n{{\"type\":\"settled\"}}\n").into_bytes()
        );
    }
}
