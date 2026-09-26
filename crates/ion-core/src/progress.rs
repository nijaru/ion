//! Bounded, process-local model progress. This never enters durable history.

use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{AttemptId, StepPurpose, TurnId};

pub const MAX_MODEL_PROGRESS_BYTES: usize = 4096;
const PROGRESS_EVENTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProgress {
    pub attachment_epoch: Uuid,
    pub turn: TurnId,
    pub attempt: AttemptId,
    pub update: ModelProgressUpdate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelProgressUpdate {
    Preview { text: String, omitted_prefix: bool },
    End,
}

#[derive(Debug)]
pub(crate) struct ProgressHub {
    epoch: Uuid,
    sender: broadcast::Sender<ModelProgress>,
}

impl ProgressHub {
    pub(crate) fn new() -> Self {
        let (sender, _) = broadcast::channel(PROGRESS_EVENTS);
        Self {
            epoch: Uuid::now_v7(),
            sender,
        }
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ModelProgress> {
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

    fn publish(&self, turn: TurnId, attempt: AttemptId, update: ModelProgressUpdate) {
        let _ = self.sender.send(ModelProgress {
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
        if fragment.len() > MAX_MODEL_PROGRESS_BYTES {
            let mut start = fragment.len() - MAX_MODEL_PROGRESS_BYTES;
            while !fragment.is_char_boundary(start) {
                start += 1;
            }
            self.preview.clear();
            self.preview.push_str(&fragment[start..]);
            self.omitted_prefix = true;
        } else {
            self.preview.push_str(fragment);
        }
        if self.preview.len() > MAX_MODEL_PROGRESS_BYTES {
            let mut start = self.preview.len() - MAX_MODEL_PROGRESS_BYTES;
            while !self.preview.is_char_boundary(start) {
                start += 1;
            }
            self.preview.drain(..start);
            self.omitted_prefix = true;
        }
        self.hub.publish(
            self.turn,
            self.attempt,
            ModelProgressUpdate::Preview {
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
                .publish(self.turn, self.attempt, ModelProgressUpdate::End);
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
            ModelProgressUpdate::Preview {
                text,
                omitted_prefix: true
            } if text.len() <= MAX_MODEL_PROGRESS_BYTES && text.chars().all(|ch| ch == '界')
        ));
        assert!(matches!(
            receiver.try_recv().unwrap().update,
            ModelProgressUpdate::End
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
}
