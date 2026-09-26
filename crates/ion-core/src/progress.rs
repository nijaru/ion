//! Bounded, process-local model and tool progress. This never enters durable history.

use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{AttemptId, StepPurpose, TurnId};

pub const MAX_PROGRESS_PREVIEW_BYTES: usize = 4096;
const PROGRESS_EVENTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProgress {
    pub attachment_epoch: Uuid,
    pub turn: TurnId,
    pub attempt: AttemptId,
    pub update: ProgressUpdate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressUpdate {
    ModelText {
        text: String,
        omitted_prefix: bool,
    },
    ToolOutput {
        stream: ToolOutputStream,
        text: String,
        omitted_prefix: bool,
    },
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolOutputStream {
    Stdout,
    Stderr,
}

#[derive(Debug)]
pub(crate) struct ProgressHub {
    epoch: Uuid,
    sender: broadcast::Sender<SessionProgress>,
}

impl ProgressHub {
    pub(crate) fn new() -> Self {
        let (sender, _) = broadcast::channel(PROGRESS_EVENTS);
        Self {
            epoch: Uuid::now_v7(),
            sender,
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<SessionProgress> {
        self.sender.subscribe()
    }

    pub(crate) fn model_attempt(
        &self,
        turn: TurnId,
        attempt: AttemptId,
        purpose: &StepPurpose,
    ) -> ModelProgressGuard<'_> {
        ModelProgressGuard {
            hub: self,
            turn,
            attempt,
            visible: matches!(purpose, StepPurpose::Generate),
            preview: String::new(),
            omitted_prefix: false,
        }
    }

    pub(crate) fn tool_attempt(
        &self,
        turn: TurnId,
        attempt: AttemptId,
    ) -> (ToolProgressPublisher, ToolProgressGuard) {
        let state = Arc::new(ToolProgressState {
            epoch: self.epoch,
            sender: self.sender.clone(),
            turn,
            attempt,
            inner: Mutex::new(ToolProgressInner {
                active: true,
                stdout: BytePreview::default(),
                stderr: BytePreview::default(),
            }),
        });
        (
            ToolProgressPublisher {
                state: Some(Arc::clone(&state)),
            },
            ToolProgressGuard { state },
        )
    }

    fn publish(&self, turn: TurnId, attempt: AttemptId, update: ProgressUpdate) {
        let _ = self.sender.send(SessionProgress {
            attachment_epoch: self.epoch,
            turn,
            attempt,
            update,
        });
    }
}

pub(crate) struct ModelProgressGuard<'a> {
    hub: &'a ProgressHub,
    turn: TurnId,
    attempt: AttemptId,
    visible: bool,
    preview: String,
    omitted_prefix: bool,
}

impl ModelProgressGuard<'_> {
    pub(crate) fn text(&mut self, fragment: &str) {
        if !self.visible || self.hub.sender.receiver_count() == 0 || fragment.is_empty() {
            return;
        }
        if fragment.len() > MAX_PROGRESS_PREVIEW_BYTES {
            let mut start = fragment.len() - MAX_PROGRESS_PREVIEW_BYTES;
            while !fragment.is_char_boundary(start) {
                start += 1;
            }
            self.preview.clear();
            self.preview.push_str(&fragment[start..]);
            self.omitted_prefix = true;
        } else {
            self.preview.push_str(fragment);
        }
        if self.preview.len() > MAX_PROGRESS_PREVIEW_BYTES {
            let mut start = self.preview.len() - MAX_PROGRESS_PREVIEW_BYTES;
            while !self.preview.is_char_boundary(start) {
                start += 1;
            }
            self.preview.drain(..start);
            self.omitted_prefix = true;
        }
        self.hub.publish(
            self.turn,
            self.attempt,
            ProgressUpdate::ModelText {
                text: self.preview.clone(),
                omitted_prefix: self.omitted_prefix,
            },
        );
    }
}

impl Drop for ModelProgressGuard<'_> {
    fn drop(&mut self) {
        if self.visible {
            self.hub
                .publish(self.turn, self.attempt, ProgressUpdate::End);
        }
    }
}

/// Process-local output capability for one physical tool attempt. The engine
/// revokes it when the owned backend call ends; retained clones cannot publish
/// after the final End event.
#[derive(Debug, Clone)]
pub struct ToolProgressPublisher {
    state: Option<Arc<ToolProgressState>>,
}

impl ToolProgressPublisher {
    #[must_use]
    pub fn disabled() -> Self {
        Self { state: None }
    }

    pub fn output(&self, stream: ToolOutputStream, chunk: &[u8]) {
        let Some(state) = &self.state else {
            return;
        };
        if chunk.is_empty() || state.sender.receiver_count() == 0 {
            return;
        }
        let mut inner = state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !inner.active {
            return;
        }
        let preview = match stream {
            ToolOutputStream::Stdout => &mut inner.stdout,
            ToolOutputStream::Stderr => &mut inner.stderr,
        };
        preview.append(chunk);
        let (text, omitted_prefix) = preview.render();
        let _ = state.sender.send(SessionProgress {
            attachment_epoch: state.epoch,
            turn: state.turn,
            attempt: state.attempt,
            update: ProgressUpdate::ToolOutput {
                stream,
                text,
                omitted_prefix,
            },
        });
    }
}

#[derive(Debug)]
struct ToolProgressState {
    epoch: Uuid,
    sender: broadcast::Sender<SessionProgress>,
    turn: TurnId,
    attempt: AttemptId,
    inner: Mutex<ToolProgressInner>,
}

#[derive(Debug)]
struct ToolProgressInner {
    active: bool,
    stdout: BytePreview,
    stderr: BytePreview,
}

#[derive(Debug, Default)]
struct BytePreview {
    bytes: Vec<u8>,
    omitted_prefix: bool,
}

impl BytePreview {
    fn append(&mut self, chunk: &[u8]) {
        if chunk.len() > MAX_PROGRESS_PREVIEW_BYTES {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len() - MAX_PROGRESS_PREVIEW_BYTES..]);
            self.omitted_prefix = true;
            return;
        }
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > MAX_PROGRESS_PREVIEW_BYTES {
            let drop_bytes = self.bytes.len() - MAX_PROGRESS_PREVIEW_BYTES;
            self.bytes.drain(..drop_bytes);
            self.omitted_prefix = true;
        }
    }

    fn render(&self) -> (String, bool) {
        let text = String::from_utf8_lossy(&self.bytes);
        if text.len() <= MAX_PROGRESS_PREVIEW_BYTES {
            return (text.into_owned(), self.omitted_prefix);
        }
        let mut start = text.len() - MAX_PROGRESS_PREVIEW_BYTES;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        (text[start..].to_owned(), true)
    }
}

pub(crate) struct ToolProgressGuard {
    state: Arc<ToolProgressState>,
}

impl Drop for ToolProgressGuard {
    fn drop(&mut self) {
        let mut inner = self
            .state
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner.active {
            inner.active = false;
            let _ = self.state.sender.send(SessionProgress {
                attachment_epoch: self.state.epoch,
                turn: self.state.turn,
                attempt: self.state.attempt,
                update: ProgressUpdate::End,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_is_bounded_utf8_and_compaction_stays_private() {
        let hub = ProgressHub::new();
        let mut receiver = hub.subscribe();
        let turn = TurnId::new(1).unwrap();
        let attempt = AttemptId::new(2).unwrap();
        {
            let mut progress = hub.model_attempt(turn, attempt, &StepPurpose::Generate);
            progress.text(&"界".repeat(3000));
        }
        let event = receiver.try_recv().unwrap();
        assert_eq!(event.turn, turn);
        assert_eq!(event.attempt, attempt);
        assert!(matches!(
            event.update,
            ProgressUpdate::ModelText {
                text,
                omitted_prefix: true
            } if text.len() <= MAX_PROGRESS_PREVIEW_BYTES && text.chars().all(|ch| ch == '界')
        ));
        assert!(matches!(
            receiver.try_recv().unwrap().update,
            ProgressUpdate::End
        ));
        {
            let mut progress = hub.model_attempt(turn, attempt, &StepPurpose::Compact);
            progress.text("internal checkpoint");
        }
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn tool_output_is_bounded_and_revoked_at_attempt_end() {
        let hub = ProgressHub::new();
        let mut receiver = hub.subscribe();
        let turn = TurnId::new(1).unwrap();
        let attempt = AttemptId::new(2).unwrap();
        let (publisher, guard) = hub.tool_attempt(turn, attempt);
        publisher.output(ToolOutputStream::Stdout, &vec![b'a'; 9000]);
        let preview = receiver.try_recv().unwrap();
        assert_eq!(preview.turn, turn);
        assert_eq!(preview.attempt, attempt);
        assert!(matches!(
            preview.update,
            ProgressUpdate::ToolOutput {
                stream: ToolOutputStream::Stdout,
                text,
                omitted_prefix: true,
            } if text.len() == MAX_PROGRESS_PREVIEW_BYTES && text.bytes().all(|byte| byte == b'a')
        ));
        publisher.output(ToolOutputStream::Stderr, &vec![0xff; 9000]);
        assert!(matches!(
            receiver.try_recv().unwrap().update,
            ProgressUpdate::ToolOutput {
                stream: ToolOutputStream::Stderr,
                text,
                omitted_prefix: true,
            } if text.len() <= MAX_PROGRESS_PREVIEW_BYTES
        ));
        drop(guard);
        assert!(matches!(
            receiver.try_recv().unwrap().update,
            ProgressUpdate::End
        ));
        publisher.output(ToolOutputStream::Stderr, b"late");
        assert!(matches!(
            receiver.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
    }
}
