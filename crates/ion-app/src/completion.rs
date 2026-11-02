//! Receiving-owned live completion grants. Coordinates never reconstruct grants.
use anyhow::{Result, ensure};
use ion_core::{
    CompletionDelivery, CompletionInbox, CompletionPermit, CompletionRevocation, ToolOrigin,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, MutexGuard, Weak},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct Binding {
    inbox: Arc<CompletionInbox>,
    policy: CompletionDelivery,
    epoch: CancellationToken,
    stopped: bool,
}
pub(crate) struct CompletionReceiver(Mutex<Binding>);
impl CompletionReceiver {
    pub(crate) fn new(session: Uuid) -> Arc<Self> {
        Arc::new(Self(Mutex::new(Binding {
            inbox: CompletionInbox::new(session),
            policy: CompletionDelivery::Inbox,
            epoch: CancellationToken::new(),
            stopped: false,
        })))
    }
    pub(crate) fn inbox(&self) -> Arc<CompletionInbox> {
        lock(&self.0).inbox.clone()
    }
    pub(crate) fn policy(&self) -> CompletionDelivery {
        lock(&self.0).policy
    }
    pub(crate) fn set_policy(&self, policy: CompletionDelivery) {
        let mut binding = lock(&self.0);
        if binding.policy != policy {
            binding.epoch.cancel();
            binding.inbox.revoke(CompletionRevocation::BindingChanged);
            binding.epoch = CancellationToken::new();
            binding.policy = policy;
        }
    }
    pub(crate) fn revoke(&self, reason: CompletionRevocation) {
        let mut binding = lock(&self.0);
        binding.epoch.cancel();
        binding.inbox.revoke(reason);
        binding.stopped = true;
    }
    pub(crate) fn invalidate_binding(&self) {
        let mut binding = lock(&self.0);
        binding.epoch.cancel();
        binding.inbox.revoke(CompletionRevocation::BindingChanged);
        binding.epoch = CancellationToken::new();
    }
    pub(crate) fn rearm(&self) {
        let mut binding = lock(&self.0);
        if binding.stopped {
            binding.epoch = CancellationToken::new();
            binding.stopped = false;
        }
    }
    pub(crate) fn rebind(&self, session: Uuid) {
        let mut binding = lock(&self.0);
        binding.epoch.cancel();
        binding.inbox.close(CompletionRevocation::BindingChanged);
        binding.inbox = CompletionInbox::new(session);
        binding.epoch = CancellationToken::new();
        binding.stopped = false;
    }
    pub(crate) fn close(&self) {
        let mut binding = lock(&self.0);
        binding.epoch.cancel();
        binding.inbox.close(CompletionRevocation::Closed);
        binding.stopped = true;
    }
    pub(crate) fn target(
        self: &Arc<Self>,
        anchor: u64,
        active_turn: Option<u64>,
        delivery: CompletionDelivery,
    ) -> Result<CompletionTarget> {
        let binding = lock(&self.0);
        ensure!(
            !binding.stopped && !binding.epoch.is_cancelled(),
            "receiving completion authority is stopped"
        );
        ensure!(
            rank(delivery) <= rank(binding.policy),
            "receiving completion policy does not authorize {delivery:?}"
        );
        Ok(CompletionTarget {
            receiver: Arc::downgrade(self),
            inbox: Arc::downgrade(&binding.inbox),
            epoch: binding.epoch.clone(),
            anchor,
            active_turn,
            delivery,
        })
    }
}

/// A receiving Host issued this opaque capability. Cloneable targets do not
/// retain Session writers and cannot survive Stop/rebinding by using a saved ID.
#[derive(Clone)]
pub struct CompletionTarget {
    receiver: Weak<CompletionReceiver>,
    inbox: Weak<CompletionInbox>,
    epoch: CancellationToken,
    anchor: u64,
    active_turn: Option<u64>,
    delivery: CompletionDelivery,
}
impl CompletionTarget {
    pub(crate) fn reserve(&self) -> Result<CompletionPermit> {
        let receiver = self
            .receiver
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("completion receiver is gone"))?;
        let binding = lock(&receiver.0);
        ensure!(
            !self.epoch.is_cancelled() && !binding.stopped,
            "completion grant was revoked"
        );
        let inbox = self
            .inbox
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("completion binding is gone"))?;
        inbox
            .reserve(self.anchor, self.active_turn, self.delivery)
            .map_err(anyhow::Error::msg)
    }
}

#[derive(Default)]
pub(crate) struct CompletionRegistry(Mutex<Registry>);
#[derive(Default)]
struct Registry {
    closed: bool,
    receivers: BTreeMap<Uuid, Weak<CompletionReceiver>>,
}
impl CompletionRegistry {
    pub(crate) fn register(&self, owner: Uuid, receiver: &Arc<CompletionReceiver>) {
        let mut state = lock(&self.0);
        state.receivers.retain(|_, entry| entry.strong_count() != 0);
        if state.closed {
            receiver.close();
        } else {
            state.receivers.insert(owner, Arc::downgrade(receiver));
        }
    }
    pub(crate) fn parent_target(&self, origin: ToolOrigin) -> Result<Option<CompletionTarget>> {
        let state = lock(&self.0);
        ensure!(!state.closed, "completion registration is closed");
        let mut matches = state
            .receivers
            .values()
            .filter_map(Weak::upgrade)
            .filter(|receiver| receiver.inbox().session() == origin.session());
        // A raw Core caller may use coding tools without installing a receiving
        // Host control. It retains ordinary reports, but grants no automatic work.
        let Some(receiver) = matches.next() else {
            return Ok(None);
        };
        ensure!(
            matches.next().is_none(),
            "originating Session has ambiguous completion receivers; supply an explicit target"
        );
        receiver
            .target(
                origin.occurrence().assistant_entry,
                Some(origin.turn()),
                receiver.policy(),
            )
            .map(Some)
    }
    pub(crate) fn close(&self) {
        let mut state = lock(&self.0);
        state.closed = true;
        for receiver in state.receivers.values().filter_map(Weak::upgrade) {
            receiver.close();
        }
        state.receivers.clear();
    }
}
fn rank(mode: CompletionDelivery) -> u8 {
    match mode {
        CompletionDelivery::Inbox => 0,
        CompletionDelivery::Continue => 1,
        CompletionDelivery::Wake => 2,
    }
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
