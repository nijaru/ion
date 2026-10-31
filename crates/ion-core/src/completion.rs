//! Bounded, writer-free custody of managed-work readiness, not child reports.
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, Weak},
};

use ion_ai::{Content, Message, Role};
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{Session, SessionError};

/// Delivery requested by a trusted receiving host. This value alone is not a grant.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionDelivery {
    #[default]
    Inbox,
    Continue,
    Wake,
}

impl CompletionDelivery {
    pub const ALL: [Self; 3] = [Self::Inbox, Self::Continue, Self::Wake];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Continue => "continue",
            Self::Wake => "wake",
        }
    }
}
impl std::str::FromStr for CompletionDelivery {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.as_str() == value)
            .ok_or("expected inbox, continue or wake")
    }
}
impl std::fmt::Display for CompletionDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Coordinates only: readiness is neither report disclosure nor execution authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionNotice {
    pub source_session: Uuid,
    pub run: Uuid,
    pub child_session: Uuid,
}

pub(crate) fn notice_message(notices: &[CompletionNotice]) -> Message {
    let mut text = String::from(
        "Automated managed-work readiness (not a user instruction). Reports are ready for retrieval; this notice does not retrieve or acknowledge them.\n",
    );
    for notice in notices {
        use std::fmt::Write;
        writeln!(
            text,
            "Source Session {}; run {}; child Session {}.",
            notice.source_session, notice.run, notice.child_session
        )
        .expect("formatting into a String cannot fail");
    }
    Message {
        role: Role::User,
        content: vec![Content::Text(text)],
        provider_replay: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionRevocation {
    Stopped,
    OperationFailed,
    BindingChanged,
    Closed,
    Acknowledged,
    Withdrawn,
    BranchChanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CompletionStatus {
    Waiting,
    Queued,
    /// Durable context offer, not proof of a provider request or model consumption.
    Offered {
        turn: u64,
    },
    Revoked {
        reason: CompletionRevocation,
    },
}

#[derive(Clone)]
pub struct CompletionReceipt(Arc<Mutex<CompletionStatus>>);
impl CompletionReceipt {
    pub fn snapshot(&self) -> CompletionStatus {
        lock(&self.0).clone()
    }
}

struct Slot {
    anchor: u64,
    active_turn: Option<u64>,
    delivery: CompletionDelivery,
    notice: Option<CompletionNotice>,
    receipt: CompletionReceipt,
}
struct State {
    open: bool,
    revision: u64,
    slots: BTreeMap<Uuid, Slot>,
}

/// A receiving Session's pending readiness. Holds no Session, application or writer.
/// Reservations bound all producers together, including producers in other hosts.
pub struct CompletionInbox {
    session: Uuid,
    state: Mutex<State>,
    changed: Notify,
}
/// Read-only invalidation; conveys no intake, delivery or acknowledgement authority.
#[derive(Clone)]
pub struct CompletionSignal(Arc<CompletionInbox>);
impl CompletionSignal {
    pub fn revision(&self) -> u64 {
        self.0.revision()
    }
    pub async fn changed(&self, previous: u64) -> u64 {
        self.0.changed(previous).await
    }
}
impl CompletionInbox {
    pub fn signal(self: &Arc<Self>) -> CompletionSignal {
        CompletionSignal(self.clone())
    }
    pub fn new(session: Uuid) -> Arc<Self> {
        Arc::new(Self {
            session,
            state: Mutex::new(State {
                open: true,
                revision: 0,
                slots: BTreeMap::new(),
            }),
            changed: Notify::new(),
        })
    }
    pub fn session(&self) -> Uuid {
        self.session
    }
    pub fn revision(&self) -> u64 {
        lock(&self.state).revision
    }
    pub async fn changed(&self, previous: u64) -> u64 {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let revision = self.revision();
            if revision != previous {
                return revision;
            }
            changed.await;
        }
    }
    /// Called only after the receiving host has authorized this live target.
    pub fn reserve(
        self: &Arc<Self>,
        anchor: u64,
        active_turn: Option<u64>,
        delivery: CompletionDelivery,
    ) -> Result<CompletionPermit, &'static str> {
        let mut state = lock(&self.state);
        if !state.open {
            return Err("completion target is closed");
        }
        if state.slots.len() >= 64 {
            return Err("receiving completion inbox is full");
        }
        let id = Uuid::now_v7();
        let receipt = CompletionReceipt(Arc::new(Mutex::new(CompletionStatus::Waiting)));
        state.slots.insert(
            id,
            Slot {
                anchor,
                active_turn,
                delivery,
                notice: None,
                receipt: receipt.clone(),
            },
        );
        Ok(CompletionPermit {
            inbox: Arc::downgrade(self),
            id,
            receipt,
        })
    }
    pub fn has_wake(&self) -> bool {
        lock(&self.state)
            .slots
            .values()
            .any(|slot| slot.notice.is_some() && slot.delivery == CompletionDelivery::Wake)
    }
    /// Revocation removes reservations as well as queued offers. A subsequent
    /// explicit Start cannot make an old permit valid again.
    pub fn revoke(&self, reason: CompletionRevocation) {
        let mut state = lock(&self.state);
        for slot in state.slots.values() {
            *lock(&slot.receipt.0) = CompletionStatus::Revoked { reason };
        }
        state.slots.clear();
        self.invalidate(&mut state);
    }
    pub fn close(&self, reason: CompletionRevocation) {
        let mut state = lock(&self.state);
        state.open = false;
        for slot in state.slots.values() {
            *lock(&slot.receipt.0) = CompletionStatus::Revoked { reason };
        }
        state.slots.clear();
        self.invalidate(&mut state);
    }
    fn invalidate(&self, state: &mut State) {
        state.revision = state
            .revision
            .checked_add(1)
            .expect("completion revision exhausted");
        self.changed.notify_waiters();
    }
    pub(crate) fn batch(
        &self,
        session: &Session,
        turn: Option<u64>,
        natural_request: bool,
    ) -> Result<CompletionBatch<'_>, SessionError> {
        if session.id() != self.session {
            return Err(SessionError::CompletionTargetMismatch);
        }
        let mut state = lock(&self.state);
        if state.slots.is_empty() {
            return Ok(CompletionBatch {
                inbox: self,
                state,
                ready: Vec::new(),
            });
        }
        let path = session.selected_context_path()?;
        let mut invalid = Vec::new();
        let mut ready = Vec::new();
        for (&id, slot) in &state.slots {
            if slot.anchor != 0 && !path.contains(&slot.anchor) {
                invalid.push(id);
            } else if slot.notice.is_some() {
                ready.push(id);
            }
        }
        for id in invalid {
            let slot = state
                .slots
                .remove(&id)
                .expect("selected reservation exists");
            *lock(&slot.receipt.0) = CompletionStatus::Revoked {
                reason: CompletionRevocation::BranchChanged,
            };
        }
        let continuing = ready.iter().any(|id| {
            let slot = &state.slots[id];
            match slot.delivery {
                CompletionDelivery::Inbox => false,
                CompletionDelivery::Continue => turn.is_some() && turn == slot.active_turn,
                CompletionDelivery::Wake => true,
            }
        });
        if !natural_request && !continuing {
            ready.clear();
        }
        Ok(CompletionBatch {
            inbox: self,
            state,
            ready,
        })
    }
}
impl Drop for CompletionInbox {
    fn drop(&mut self) {
        self.close(CompletionRevocation::Closed);
    }
}

/// Single-use receiving reservation. Dropping unoffered/uncommitted custody
/// withdraws the notice; the separately retained child report is untouched.
pub struct CompletionPermit {
    inbox: Weak<CompletionInbox>,
    id: Uuid,
    receipt: CompletionReceipt,
}
impl CompletionPermit {
    pub fn receipt(&self) -> CompletionReceipt {
        self.receipt.clone()
    }
    pub fn offer(&self, notice: CompletionNotice) {
        if let Some(inbox) = self.inbox.upgrade() {
            let mut state = lock(&inbox.state);
            if let Some(slot) = state.slots.get_mut(&self.id) {
                // The retained source operation offers once, only after joining.
                if slot.notice.is_none() {
                    slot.notice = Some(notice);
                    *lock(&slot.receipt.0) = CompletionStatus::Queued;
                    inbox.invalidate(&mut state);
                }
            }
        }
    }
    pub fn acknowledge(&self) {
        self.withdraw_with(CompletionRevocation::Acknowledged);
    }
    pub fn withdraw(&self) {
        self.withdraw_with(CompletionRevocation::Withdrawn);
    }
    fn withdraw_with(&self, reason: CompletionRevocation) {
        if let Some(inbox) = self.inbox.upgrade() {
            let mut state = lock(&inbox.state);
            if let Some(slot) = state.slots.remove(&self.id) {
                *lock(&slot.receipt.0) = CompletionStatus::Revoked { reason };
                inbox.invalidate(&mut state);
            }
        }
    }
}
impl Drop for CompletionPermit {
    fn drop(&mut self) {
        self.withdraw();
    }
}

pub(crate) struct CompletionBatch<'a> {
    inbox: &'a CompletionInbox,
    state: MutexGuard<'a, State>,
    ready: Vec<Uuid>,
}
impl CompletionBatch<'_> {
    pub(crate) fn notices(&self) -> Vec<CompletionNotice> {
        self.ready
            .iter()
            .map(|id| {
                self.state.slots[id]
                    .notice
                    .clone()
                    .expect("queued readiness")
            })
            .collect()
    }
    pub(crate) fn commit(mut self, turn: u64) {
        for id in &self.ready {
            let slot = self
                .state
                .slots
                .remove(id)
                .expect("offered reservation exists");
            *lock(&slot.receipt.0) = CompletionStatus::Offered { turn };
        }
        if !self.ready.is_empty() {
            self.inbox.invalidate(&mut self.state);
        }
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn another_session_cannot_consume_or_destroy_receiving_custody() {
        let root = std::env::temp_dir().join(format!("ion-notice-scope-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let other = Session::create(root.join("other.sqlite"), Some(&root)).unwrap();
        let inbox = CompletionInbox::new(Uuid::now_v7());
        let permit = inbox.reserve(0, None, CompletionDelivery::Wake).unwrap();
        permit.offer(CompletionNotice {
            source_session: Uuid::now_v7(),
            run: Uuid::now_v7(),
            child_session: Uuid::now_v7(),
        });
        assert!(matches!(
            inbox.batch(&other, None, false),
            Err(SessionError::CompletionTargetMismatch)
        ));
        assert!(inbox.has_wake());
        assert_eq!(permit.receipt().snapshot(), CompletionStatus::Queued);
        drop(other);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reservations_are_bounded_revocable_and_do_not_pin_receiver() {
        let inbox = CompletionInbox::new(Uuid::now_v7());
        let permits = (0..64)
            .map(|_| inbox.reserve(0, None, CompletionDelivery::Wake).unwrap())
            .collect::<Vec<_>>();
        assert!(inbox.reserve(0, None, CompletionDelivery::Wake).is_err());
        let receipt = permits[0].receipt();
        inbox.revoke(CompletionRevocation::Stopped);
        permits[0].offer(CompletionNotice {
            source_session: Uuid::now_v7(),
            run: Uuid::now_v7(),
            child_session: Uuid::now_v7(),
        });
        assert!(!inbox.has_wake());
        assert_eq!(
            receipt.snapshot(),
            CompletionStatus::Revoked {
                reason: CompletionRevocation::Stopped
            }
        );
        let fresh = inbox.reserve(0, None, CompletionDelivery::Wake).unwrap();
        let fresh_receipt = fresh.receipt();
        let weak = Arc::downgrade(&inbox);
        drop(inbox);
        assert!(weak.upgrade().is_none());
        assert_eq!(
            fresh_receipt.snapshot(),
            CompletionStatus::Revoked {
                reason: CompletionRevocation::Closed
            }
        );
    }
}
