//! Incoming MCP transport budgets.
//!
//! Stdio and HTTP each hand framing to a different SDK path. Keep the byte
//! accounting here so both paths reject oversize input before a decoder retains
//! it, without changing the SDK's protocol semantics.

use std::{
    fmt, io,
    pin::Pin,
    task::{Context, Poll},
};

use futures_util::{Stream, StreamExt};
use tokio::io::{AsyncRead, ReadBuf};

/// Aggregate ceiling for one stdio JSON line or HTTP JSON/error body.
pub(super) const MAX_MCP_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// The SDK's default per-event SSE ceiling. Streamable HTTP callers may select
/// a smaller event budget, but never one above the aggregate message ceiling.
pub(super) const DEFAULT_MAX_SSE_EVENT_BYTES: usize = 16 * 1024 * 1024;

/// An `AsyncRead` adapter that rejects one JSON line over its byte budget.
///
/// The SDK's line reader buffers until `\n`; the budget is therefore enforced
/// here, before those bytes can grow that buffer. A delimiter resets the
/// budget, including the `\r\n` pair. No bytes for an over-budget line are
/// delivered to the SDK, and the reader stays poisoned so a caller cannot
/// resume at an arbitrary later line boundary.
pub(super) struct BoundedLineReader<R> {
    inner: R,
    max_bytes: usize,
    used: usize,
    overflowed: bool,
}

impl<R> BoundedLineReader<R> {
    pub(super) fn new(inner: R, max_bytes: usize) -> Self {
        Self {
            inner,
            max_bytes,
            used: 0,
            overflowed: false,
        }
    }

    fn overflow(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "MCP JSON line exceeded the maximum size of {} bytes",
                self.max_bytes
            ),
        )
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for BoundedLineReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.overflowed {
            return Poll::Ready(Err(this.overflow()));
        }

        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                for &byte in &buf.filled()[before..] {
                    if this.used == this.max_bytes {
                        this.overflowed = true;
                        return Poll::Ready(Err(this.overflow()));
                    }
                    this.used += 1;
                    if byte == b'\n' {
                        this.used = 0;
                    }
                }
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
        }
    }
}

/// An error returned by [`bounded_sse_events`].
#[derive(Debug)]
pub(super) enum BoundedSseError<E> {
    Source(E),
    EventTooLarge { max_size: usize },
}

impl<E: fmt::Display> fmt::Display for BoundedSseError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => error.fmt(formatter),
            Self::EventTooLarge { max_size } => write!(
                formatter,
                "SSE event exceeded the maximum size of {max_size} bytes"
            ),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for BoundedSseError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::EventTooLarge { .. } => None,
        }
    }
}

struct SseEventBudget {
    max_size: usize,
    retained_size: usize,
    line_size: usize,
    line_is_comment: bool,
    previous_was_cr: bool,
}

impl SseEventBudget {
    fn new(max_size: usize) -> Self {
        Self {
            max_size,
            retained_size: 0,
            line_size: 0,
            line_is_comment: false,
            previous_was_cr: false,
        }
    }

    fn observe(&mut self, chunk: &[u8]) -> Result<(), ()> {
        for &byte in chunk {
            if self.previous_was_cr {
                self.previous_was_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }

            match byte {
                b'\r' => {
                    self.finish_line()?;
                    self.previous_was_cr = true;
                }
                b'\n' => self.finish_line()?,
                _ => {
                    if self.line_size == 0 {
                        self.line_is_comment = byte == b':';
                    }
                    self.line_size = self.line_size.saturating_add(1);
                    self.check_limit()?;
                }
            }
        }
        Ok(())
    }

    fn finish_line(&mut self) -> Result<(), ()> {
        if self.line_size == 0 {
            self.retained_size = 0;
        } else if !self.line_is_comment {
            // The SSE parser joins multiple data lines with a newline.
            self.retained_size = self
                .retained_size
                .saturating_add(self.line_size)
                .saturating_add(1);
        }
        self.line_size = 0;
        self.line_is_comment = false;
        self.check_limit()
    }

    fn check_limit(&self) -> Result<(), ()> {
        if self.retained_size.saturating_add(self.line_size) > self.max_size {
            Err(())
        } else {
            Ok(())
        }
    }
}

/// Bound raw SSE bytes before the parser joins multi-line data, recognizes
/// comments, or normalizes CRLF. Comments do not replenish the data budget;
/// an empty line completes the event and resets it.
pub(super) fn bounded_sse_events<S, C, E>(
    stream: S,
    max_event_size: usize,
) -> impl Stream<Item = Result<C, BoundedSseError<E>>> + Send
where
    S: Stream<Item = Result<C, E>> + Send + 'static,
    C: AsRef<[u8]> + Send + 'static,
    E: std::error::Error + Send + Sync + 'static,
{
    let stream = Box::pin(stream);
    let budget = Some(SseEventBudget::new(max_event_size));
    futures_util::stream::unfold((stream, budget), |(mut stream, budget)| async move {
        let mut budget = budget?;
        match stream.next().await {
            Some(Ok(chunk)) => {
                if budget.observe(chunk.as_ref()).is_err() {
                    Some((
                        Err(BoundedSseError::EventTooLarge {
                            max_size: budget.max_size,
                        }),
                        (stream, None),
                    ))
                } else {
                    Some((Ok(chunk), (stream, Some(budget))))
                }
            }
            Some(Err(error)) => Some((Err(BoundedSseError::Source(error)), (stream, None))),
            None => None,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::{
        model::ServerJsonRpcMessage,
        transport::{Transport, async_rw::AsyncRwTransport},
    };
    use tokio::{
        io::{AsyncWriteExt, duplex, sink},
        sync::oneshot,
    };

    const NOTIFICATION: &[u8] = br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;

    fn notification_line() -> Vec<u8> {
        let mut line = NOTIFICATION.to_vec();
        line.push(b'\n');
        line
    }

    async fn receive_line(line: &[u8], max_bytes: usize) -> Option<ServerJsonRpcMessage> {
        let mut transport =
            AsyncRwTransport::new_client(BoundedLineReader::new(line, max_bytes), sink());
        transport.receive().await
    }

    #[tokio::test]
    async fn sdk_receives_exact_line_and_rejects_one_byte_over_budget() {
        let mut line = notification_line();
        assert!(
            receive_line(&line, line.len()).await.is_some(),
            "exact stdio line budget was rejected"
        );

        line.insert(line.len() - 1, b' ');
        assert!(
            receive_line(&line, line.len() - 1).await.is_none(),
            "over-budget line reached the SDK decoder"
        );
    }

    #[tokio::test]
    async fn crlf_bytes_count_against_the_same_line_budget() {
        let lf = notification_line();
        let mut crlf = NOTIFICATION.to_vec();
        crlf.extend_from_slice(b"\r\n");

        assert!(
            receive_line(&crlf, lf.len()).await.is_none(),
            "CRLF pair was charged as a single delimiter byte"
        );
        assert!(
            receive_line(&crlf, crlf.len()).await.is_some(),
            "CRLF line at the exact total budget was rejected"
        );
    }

    #[tokio::test]
    async fn cancelled_partial_line_resumes_with_the_same_budget() {
        let line = notification_line();
        let line_len = line.len();
        let split = line_len / 2;
        let (reader, mut writer) = duplex(line_len);
        let (started_tx, started) = oneshot::channel();
        let (resume_tx, resume) = oneshot::channel::<()>();
        let writer_task = tokio::spawn(async move {
            writer.write_all(&line[..split]).await.unwrap();
            started_tx.send(()).unwrap();
            resume.await.unwrap();
            writer.write_all(&line[split..]).await.unwrap();
        });
        let mut transport =
            AsyncRwTransport::new_client(BoundedLineReader::new(reader, line_len), sink());

        started.await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), transport.receive())
                .await
                .is_err(),
            "partial line completed without its remainder"
        );
        resume_tx.send(()).unwrap();
        assert!(
            transport.receive().await.is_some(),
            "cancelled partial read reset the retained line"
        );
        writer_task.await.unwrap();
    }

    #[test]
    fn sse_budget_counts_multiline_data_crlf_and_comments() {
        let mut budget = SseEventBudget::new(20);
        assert!(budget.observe(b"data: a\r\n").is_ok());
        assert!(budget.observe(b"data: b\r\n").is_ok());
        // Two data lines retain their joined length plus one newline each.
        assert!(budget.observe(b":x\r\n").is_ok());
        // A comment does not reset the retained event or replenish its budget.
        assert!(budget.observe(b"data: c").is_err());
    }

    #[tokio::test]
    async fn actual_64_mib_stdio_boundary_is_checked_before_sdk_decode() {
        let mut line = Vec::with_capacity(MAX_MCP_MESSAGE_BYTES + 1);
        line.extend_from_slice(NOTIFICATION);
        line.resize(MAX_MCP_MESSAGE_BYTES - 1, b' ');
        line.push(b'\n');
        assert_eq!(line.len(), MAX_MCP_MESSAGE_BYTES);
        assert!(
            receive_line(&line, MAX_MCP_MESSAGE_BYTES).await.is_some(),
            "exact 64 MiB stdio line was rejected"
        );

        line[MAX_MCP_MESSAGE_BYTES - 1] = b' ';
        line.push(b'\n');
        assert_eq!(line.len(), MAX_MCP_MESSAGE_BYTES + 1);
        assert!(
            receive_line(&line, MAX_MCP_MESSAGE_BYTES).await.is_none(),
            "64 MiB + 1 stdio line reached the SDK decoder"
        );
    }
}
