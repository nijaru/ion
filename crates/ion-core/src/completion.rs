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
    /// A storage attempt may have committed. Not eligible for automatic retry.
    Unresolved,
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
    claimed: bool,
    withdrawal: Option<CompletionRevocation>,
}
impl Slot {
    /// A claim retains custody until its storage outcome is known. In particular,
    /// a concurrent withdrawal must not turn a durable success into a revocation.
    fn withdraw(&mut self, reason: CompletionRevocation) -> bool {
        let reason = *self.withdrawal.get_or_insert(reason);
        if self.claimed {
            false
        } else {
            *lock(&self.receipt.0) = CompletionStatus::Revoked { reason };
            true
        }
    }
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
                claimed: false,
                withdrawal: None,
            },
        );
        Ok(CompletionPermit {
            inbox: Arc::downgrade(self),
            id,
            receipt,
        })
    }
    pub fn has_wake(&self) -> bool {
        lock(&self.state).slots.values().any(|slot| {
            !slot.claimed && slot.notice.is_some() && slot.delivery == CompletionDelivery::Wake
        })
    }
    /// Revocation removes unclaimed reservations and defers withdrawal of live
    /// claims until their storage outcome. A subsequent Start cannot revive them.
    pub fn revoke(&self, reason: CompletionRevocation) {
        let mut state = lock(&self.state);
        state.slots.retain(|_, slot| !slot.withdraw(reason));
        self.invalidate(state);
    }
    pub fn close(&self, reason: CompletionRevocation) {
        let mut state = lock(&self.state);
        state.open = false;
        state.slots.retain(|_, slot| !slot.withdraw(reason));
        self.invalidate(state);
    }
    fn invalidate(&self, mut state: MutexGuard<'_, State>) {
        state.revision = state
            .revision
            .checked_add(1)
            .expect("completion revision exhausted");
        // Wakers can run arbitrary receiving-host code, including reading us.
        drop(state);
        self.changed.notify_waiters();
    }
    pub(crate) fn batch(
        self: &Arc<Self>,
        session: &Session,
        turn: Option<u64>,
        natural_request: bool,
    ) -> Result<CompletionBatch, SessionError> {
        if session.id() != self.session {
            return Err(SessionError::CompletionTargetMismatch);
        }
        // Session storage may block. Never acquire it beneath the inbox lock.
        let path = session.selected_context_path()?;
        let mut state = lock(&self.state);
        let mut invalid = Vec::new();
        let mut ready = Vec::new();
        for (&id, slot) in &state.slots {
            if slot.claimed {
                continue;
            }
            if slot.anchor != 0 && !path.contains(&slot.anchor) {
                invalid.push(id);
            } else if slot.notice.is_some() {
                ready.push(id);
            }
        }
        let invalidated = !invalid.is_empty();
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
        let notices = ready
            .iter()
            .map(|id| {
                let slot = state
                    .slots
                    .get_mut(id)
                    .expect("selected reservation exists");
                slot.claimed = true;
                slot.notice.clone().expect("queued readiness")
            })
            .collect();
        let batch = CompletionBatch {
            inbox: self.clone(),
            ready,
            notices,
            begun: false,
        };
        if invalidated || !batch.ready.is_empty() {
            self.invalidate(state);
        }
        Ok(batch)
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
                    inbox.invalidate(state);
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
            if let Some(slot) = state.slots.get_mut(&self.id) {
                if slot.withdraw(reason) {
                    state.slots.remove(&self.id);
                }
                inbox.invalidate(state);
            }
        }
    }
}
impl Drop for CompletionPermit {
    fn drop(&mut self) {
        self.withdraw();
    }
}

/// Owned claim: no Session borrow or lock survives selection. Claimed slots
/// still count against capacity, including after revoke/close, until resolution.
/// Dropping before `begin` releases custody; dropping after it is uncertain.
#[must_use]
pub(crate) struct CompletionBatch {
    inbox: Arc<CompletionInbox>,
    ready: Vec<Uuid>,
    notices: Vec<CompletionNotice>,
    begun: bool,
}
impl CompletionBatch {
    pub(crate) fn notices(&self) -> Vec<CompletionNotice> {
        self.notices.clone()
    }
    /// Call immediately before storage may commit, including before dispatch to
    /// a worker that can outlive its waiter. This does not publish an offer.
    pub(crate) fn begin(&mut self) {
        self.begun = true;
    }
    /// Only a known durable success may publish Offered. Also accepts an
    /// unbegun claim, but callers must begin to protect their uncertain exits.
    pub(crate) fn commit(mut self, turn: u64) {
        self.begin();
        self.finish(Some(CompletionStatus::Offered { turn }));
    }
    /// Use only when storage is known not to have committed. Otherwise drop a
    /// begun batch to preserve uncertainty instead of silently retrying it.
    pub(crate) fn reject(mut self) {
        self.finish(None);
    }
    fn finish(&mut self, outcome: Option<CompletionStatus>) {
        if self.ready.is_empty() {
            return;
        }
        let mut state = lock(&self.inbox.state);
        for id in self.ready.drain(..) {
            let mut slot = state.slots.remove(&id).expect("claimed reservation exists");
            if let Some(status) = &outcome {
                *lock(&slot.receipt.0) = status.clone();
            } else if let Some(reason) = slot.withdrawal {
                *lock(&slot.receipt.0) = CompletionStatus::Revoked { reason };
            } else {
                slot.claimed = false;
                state.slots.insert(id, slot);
            }
        }
        self.notices.clear();
        self.inbox.invalidate(state);
    }
}
impl Drop for CompletionBatch {
    fn drop(&mut self) {
        if self.begun {
            self.finish(Some(CompletionStatus::Unresolved));
        } else {
            self.finish(None);
        }
    }
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
