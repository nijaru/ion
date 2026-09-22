//! Commit-addressed bounded observation for one Session.
//!
//! Durable updates are structural batches emitted only after their semantic
//! transaction commits. Provisional provider/tool progress belongs on a
//! separate future channel.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, Weak};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::Notify;

use crate::{
    CommitSeq, Conversation, ConversationId, Entry, EntryId, Input, InstalledConfig, ModelAttempt,
    ModelStep, SessionId, Turn,
};

pub const MAX_SNAPSHOT_INPUTS: usize = 256;
pub const MAX_SNAPSHOT_ENTRIES: usize = 256;
pub const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_WATCH_RECEIPTS: usize = 512;
pub const MAX_WATCH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitReceipt {
    pub seq: CommitSeq,
    pub update: SessionUpdate,
}

impl CommitReceipt {
    fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |encoded| encoded.len())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionUpdate {
    pub changes: Vec<SessionChange>,
}

impl SessionUpdate {
    #[must_use]
    pub fn new(changes: Vec<SessionChange>) -> Self {
        Self { changes }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SessionChange {
    Conversation(Conversation),
    Config {
        conversation: ConversationId,
        installed: InstalledConfig,
    },
    Input(Input),
    Entry(Entry),
    Turn(Turn),
    ModelStep(ModelStep),
    ModelAttempt(ModelAttempt),
    ToolInvocation(crate::ToolInvocation),
    ToolAttempt(crate::ToolAttempt),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRequest {
    pub conversation: ConversationId,
    pub max_inputs: usize,
    pub max_entries: usize,
    pub max_bytes: usize,
}

impl SnapshotRequest {
    pub fn validate(self) -> Result<Self, ObservationError> {
        if self.max_inputs > MAX_SNAPSHOT_INPUTS {
            return Err(ObservationError::Limit {
                field: "max_inputs",
                requested: self.max_inputs,
                maximum: MAX_SNAPSHOT_INPUTS,
            });
        }
        if self.max_entries > MAX_SNAPSHOT_ENTRIES {
            return Err(ObservationError::Limit {
                field: "max_entries",
                requested: self.max_entries,
                maximum: MAX_SNAPSHOT_ENTRIES,
            });
        }
        if self.max_bytes == 0 || self.max_bytes > MAX_SNAPSHOT_BYTES {
            return Err(ObservationError::Limit {
                field: "max_bytes",
                requested: self.max_bytes,
                maximum: MAX_SNAPSHOT_BYTES,
            });
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchQueueLimits {
    pub max_receipts: usize,
    pub max_bytes: usize,
}

impl Default for WatchQueueLimits {
    fn default() -> Self {
        Self {
            max_receipts: 128,
            max_bytes: 1024 * 1024,
        }
    }
}

impl WatchQueueLimits {
    pub fn validate(self) -> Result<Self, ObservationError> {
        if self.max_receipts == 0 || self.max_receipts > MAX_WATCH_RECEIPTS {
            return Err(ObservationError::Limit {
                field: "watch.max_receipts",
                requested: self.max_receipts,
                maximum: MAX_WATCH_RECEIPTS,
            });
        }
        if self.max_bytes == 0 || self.max_bytes > MAX_WATCH_BYTES {
            return Err(ObservationError::Limit {
                field: "watch.max_bytes",
                requested: self.max_bytes,
                maximum: MAX_WATCH_BYTES,
            });
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchRequest {
    pub snapshot: SnapshotRequest,
    pub queue: WatchQueueLimits,
}

impl WatchRequest {
    pub fn validate(self) -> Result<Self, ObservationError> {
        self.snapshot.validate()?;
        self.queue.validate()?;
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub coverage: CommitSeq,
    pub session: SessionId,
    pub primary_conversation: ConversationId,
    pub conversation: Conversation,
    pub config: InstalledConfig,
    pub unfinished_turn: Option<Turn>,
    pub current_model_step: Option<ModelStep>,
    pub model_attempts: Vec<ModelAttempt>,
    pub queued_inputs: Vec<Input>,
    pub has_more_inputs: bool,
    pub transcript_tail: Vec<Entry>,
    pub has_older_entries: bool,
}

impl SessionSnapshot {
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        serde_json::to_vec(self).map_or(usize::MAX, |encoded| encoded.len())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntryPage {
    pub entries: Vec<Entry>,
    pub has_more: bool,
    pub next_before: Option<EntryId>,
}

#[derive(Debug)]
pub struct SnapshotWatch {
    pub snapshot: SessionSnapshot,
    pub watch: SessionWatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ObservationError {
    #[error("{field} requested {requested}; maximum is {maximum}")]
    Limit {
        field: &'static str,
        requested: usize,
        maximum: usize,
    },
    #[error("snapshot mandatory state exceeds the requested {maximum}-byte bound")]
    SnapshotTooLarge { maximum: usize },
    #[error("watch overflowed or reset; establish a fresh snapshot/watch")]
    ResnapshotRequired,
    #[error("session watch is closed")]
    Closed,
    #[error("no committed update is currently queued")]
    Empty,
}

#[derive(Debug)]
pub struct SessionWatch {
    inner: Arc<Subscriber>,
}

impl SessionWatch {
    pub async fn recv(&self) -> Result<CommitReceipt, ObservationError> {
        loop {
            let notified = self.inner.notify.notified();
            match self.inner.poll() {
                Ok(Some(receipt)) => return Ok(receipt),
                Ok(None) => notified.await,
                Err(error) => return Err(error),
            }
        }
    }

    pub fn try_recv(&self) -> Result<CommitReceipt, ObservationError> {
        match self.inner.poll()? {
            Some(receipt) => Ok(receipt),
            None => Err(ObservationError::Empty),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ObservationHub {
    subscribers: Arc<Mutex<Vec<Weak<Subscriber>>>>,
}

impl ObservationHub {
    pub(crate) fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn subscribe(
        &self,
        limits: WatchQueueLimits,
    ) -> Result<Subscription, ObservationError> {
        limits.validate()?;
        let subscriber = Arc::new(Subscriber {
            state: Mutex::new(SubscriberState {
                queue: VecDeque::new(),
                bytes: 0,
                overflowed: false,
            }),
            notify: Notify::new(),
            limits,
        });
        self.subscribers
            .lock()
            .expect("observation subscriber list poisoned")
            .push(Arc::downgrade(&subscriber));
        Ok(Subscription { inner: subscriber })
    }

    pub(crate) fn publish(&self, receipt: CommitReceipt) {
        let mut subscribers = self
            .subscribers
            .lock()
            .expect("observation subscriber list poisoned");
        subscribers.retain(|weak| {
            let Some(subscriber) = weak.upgrade() else {
                return false;
            };
            subscriber.push(receipt.clone());
            true
        });
    }
}

#[derive(Debug)]
pub(crate) struct Subscription {
    inner: Arc<Subscriber>,
}

impl Subscription {
    pub(crate) fn handoff(self, coverage: CommitSeq) -> Result<SessionWatch, ObservationError> {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("observation subscriber poisoned");
            if state.overflowed {
                return Err(ObservationError::ResnapshotRequired);
            }
            while state
                .queue
                .front()
                .is_some_and(|receipt| receipt.seq <= coverage)
            {
                if let Some(receipt) = state.queue.pop_front() {
                    state.bytes = state.bytes.saturating_sub(receipt.encoded_len());
                }
            }
        }
        Ok(SessionWatch { inner: self.inner })
    }
}

#[derive(Debug)]
struct SubscriberState {
    queue: VecDeque<CommitReceipt>,
    bytes: usize,
    overflowed: bool,
}

#[derive(Debug)]
struct Subscriber {
    state: Mutex<SubscriberState>,
    notify: Notify,
    limits: WatchQueueLimits,
}

impl Subscriber {
    fn push(&self, receipt: CommitReceipt) {
        let encoded_len = receipt.encoded_len();
        let mut state = self.state.lock().expect("observation subscriber poisoned");
        if state.overflowed {
            return;
        }
        let next_receipts = state.queue.len().saturating_add(1);
        let next_bytes = state.bytes.saturating_add(encoded_len);
        if next_receipts > self.limits.max_receipts || next_bytes > self.limits.max_bytes {
            state.queue.clear();
            state.bytes = 0;
            state.overflowed = true;
            drop(state);
            self.notify.notify_waiters();
            return;
        }
        state.bytes = next_bytes;
        state.queue.push_back(receipt);
        drop(state);
        self.notify.notify_one();
    }

    fn poll(&self) -> Result<Option<CommitReceipt>, ObservationError> {
        let mut state = self.state.lock().expect("observation subscriber poisoned");
        if state.overflowed {
            return Err(ObservationError::ResnapshotRequired);
        }
        let Some(receipt) = state.queue.pop_front() else {
            return Ok(None);
        };
        state.bytes = state.bytes.saturating_sub(receipt.encoded_len());
        Ok(Some(receipt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(seq: i64) -> CommitReceipt {
        CommitReceipt {
            seq: CommitSeq::new(seq).expect("seq"),
            update: SessionUpdate::new(Vec::new()),
        }
    }

    #[test]
    fn handoff_discards_covered_commits_and_keeps_later_ones() {
        let hub = ObservationHub::new();
        let subscription = hub
            .subscribe(WatchQueueLimits {
                max_receipts: 4,
                max_bytes: 4096,
            })
            .expect("subscribe");
        hub.publish(receipt(4));
        hub.publish(receipt(5));
        let watch = subscription
            .handoff(CommitSeq::new(4).expect("coverage"))
            .expect("handoff");
        assert_eq!(watch.try_recv().expect("later receipt").seq.get(), 5);
        assert!(matches!(watch.try_recv(), Err(ObservationError::Empty)));
    }

    #[test]
    fn overflow_before_handoff_invalidates_the_handshake() {
        let hub = ObservationHub::new();
        let subscription = hub
            .subscribe(WatchQueueLimits {
                max_receipts: 1,
                max_bytes: 4096,
            })
            .expect("subscribe");
        hub.publish(receipt(2));
        hub.publish(receipt(3));
        assert!(matches!(
            subscription.handoff(CommitSeq::new(1).expect("coverage")),
            Err(ObservationError::ResnapshotRequired)
        ));
    }

    #[test]
    fn overflow_after_handoff_requires_resnapshot() {
        let hub = ObservationHub::new();
        let subscription = hub
            .subscribe(WatchQueueLimits {
                max_receipts: 1,
                max_bytes: 4096,
            })
            .expect("subscribe");
        let watch = subscription
            .handoff(CommitSeq::new(1).expect("coverage"))
            .expect("handoff");
        hub.publish(receipt(2));
        hub.publish(receipt(3));
        assert!(matches!(
            watch.try_recv(),
            Err(ObservationError::ResnapshotRequired)
        ));
    }
}
