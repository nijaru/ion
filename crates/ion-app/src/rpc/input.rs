//! Bounded, cancellation-safe LF framing for the sustained controller.
use anyhow::Result;
use tokio::io::AsyncBufRead;

pub(super) const MAX_COMMAND_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Input {
    Line(Vec<u8>),
    TooLarge,
    Incomplete,
    Eof,
}

/// Consumed bytes belong to the connection, not a cancellable `select!` branch.
/// Dropping `next()` to publish progress must preserve the unfinished frame.
pub(super) struct CommandReader<R> {
    reader: R,
    line: Vec<u8>,
    too_large: bool,
}

impl<R: AsyncBufRead + Unpin> CommandReader<R> {
    pub(super) fn new(reader: R) -> Self {
        Self {
            reader,
            line: Vec::new(),
            too_large: false,
        }
    }

    pub(super) async fn next(&mut self) -> Result<Input> {
        use tokio::io::AsyncBufReadExt;

        loop {
            let chunk = self.reader.fill_buf().await?;
            if chunk.is_empty() {
                return Ok(self.finish(false));
            }
            let end = chunk.iter().position(|byte| *byte == b'\n');
            let count = end.map_or(chunk.len(), |index| index + 1);
            if !self.too_large {
                if self.line.len() + count > MAX_COMMAND_BYTES {
                    self.too_large = true;
                    self.line.clear();
                } else {
                    self.line.extend_from_slice(&chunk[..count]);
                }
            }
            self.reader.consume(count);
            if end.is_some() {
                return Ok(self.finish(true));
            }
        }
    }

    fn finish(&mut self, terminated: bool) -> Input {
        if std::mem::take(&mut self.too_large) {
            return Input::TooLarge;
        }
        let mut line = std::mem::take(&mut self.line);
        if !terminated {
            return if line.is_empty() {
                Input::Eof
            } else {
                Input::Incomplete
            };
        }
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        Input::Line(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn interrupted_read_keeps_partial_frame_and_oversize_state() {
        use std::{future::Future, task::Poll};

        for (prefix, suffix, expected) in [
            (
                b"{\"id\":\"split\",".to_vec(),
                b"\"type\":\"get_state\"}\n".as_slice(),
                Input::Line(b"{\"id\":\"split\",\"type\":\"get_state\"}".to_vec()),
            ),
            (
                vec![b'x'; MAX_COMMAND_BYTES + 1],
                b"{}\n".as_slice(),
                Input::TooLarge,
            ),
        ] {
            let (mut writer, stream) = tokio::io::duplex(MAX_COMMAND_BYTES + 64);
            writer.write_all(&prefix).await.unwrap();
            let mut reader =
                CommandReader::new(BufReader::with_capacity(MAX_COMMAND_BYTES + 64, stream));
            let mut pending = Box::pin(reader.next());
            std::future::poll_fn(|context| {
                assert!(matches!(pending.as_mut().poll(context), Poll::Pending));
                Poll::Ready(())
            })
            .await;
            drop(pending); // A progress event wins the RPC loop's select.
            writer.write_all(suffix).await.unwrap();
            assert_eq!(reader.next().await.unwrap(), expected);
            writer.write_all(b"{}\n").await.unwrap();
            assert_eq!(reader.next().await.unwrap(), Input::Line(b"{}".to_vec()));
        }
    }

    #[tokio::test]
    async fn framing_is_lf_only_and_recovers_after_oversize() {
        let mut input = Vec::new();
        input.extend_from_slice(b"{\"message\":\"a\xE2\x80\xA8b\"}\r\n");
        input.extend(std::iter::repeat_n(b'x', MAX_COMMAND_BYTES + 1));
        input.extend_from_slice(b"\n{}\n");
        let mut reader = CommandReader::new(BufReader::new(input.as_slice()));
        assert_eq!(
            reader.next().await.unwrap(),
            Input::Line(b"{\"message\":\"a\xE2\x80\xA8b\"}".to_vec())
        );
        assert_eq!(reader.next().await.unwrap(), Input::TooLarge);
        assert_eq!(reader.next().await.unwrap(), Input::Line(b"{}".to_vec()));
        assert_eq!(reader.next().await.unwrap(), Input::Eof);

        let mut incomplete =
            CommandReader::new(BufReader::new(b"{\"type\":\"get_state\"}".as_slice()));
        assert_eq!(incomplete.next().await.unwrap(), Input::Incomplete);
        assert_eq!(incomplete.next().await.unwrap(), Input::Eof);
    }
}
