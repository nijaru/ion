//! Process-local effect admission for one Session.
//!
//! Durable intent is necessary but not sufficient to cross an external boundary.
//! Cancellation/close seal the affected gate before their durable transition so a
//! concurrently committed intent cannot start afterward.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, Weak};

use tokio_util::sync::CancellationToken;

use crate::TurnId;

#[derive(Debug, Default)]
pub(crate) struct EffectGates {
    inner: Mutex<HashMap<TurnId, Arc<EffectGate>>>,
}

impl EffectGates {
    pub(crate) fn gate(&self, turn: TurnId) -> Arc<EffectGate> {
        let mut gates = self.inner.lock().expect("effect-gate map poisoned");
        Arc::clone(gates.entry(turn).or_insert_with(|| Arc::new(EffectGate::new())))
    }

    pub(crate) fn begin_abort(&self, turn: TurnId) -> Vec<CancellationToken> {
        self.gate(turn).seal()
    }

    pub(crate) fn seal_all(&self) -> Vec<CancellationToken> {
        let gates = self.inner.lock().expect("effect-gate map poisoned");
        let mut tokens = Vec::new();
        for gate in gates.values() {
            tokens.extend(gate.seal());
        }
        tokens
    }
}

#[derive(Debug)]
pub(crate) struct EffectGate {
    state: Mutex<GateState>,
}

#[derive(Debug)]
struct GateState {
    sealed: bool,
    next_permit: u64,
    active: BTreeMap<u64, CancellationToken>,
}

impl EffectGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(GateState {
                sealed: false,
                next_permit: 0,
                active: BTreeMap::new(),
            }),
        }
    }

    pub(crate) fn admit(self: &Arc<Self>) -> Option<EffectPermit> {
        let mut state = self.state.lock().expect("effect gate poisoned");
        if state.sealed {
            return None;
        }
        state.next_permit = state.next_permit.checked_add(1)?;
        let id = state.next_permit;
        let stop = CancellationToken::new();
        state.active.insert(id, stop.clone());
        Some(EffectPermit {
            gate: Arc::downgrade(self),
            id,
            stop,
        })
    }

    fn seal(&self) -> Vec<CancellationToken> {
        let mut state = self.state.lock().expect("effect gate poisoned");
        state.sealed = true;
        state.active.values().cloned().collect()
    }

    fn release(&self, id: u64) {
        self.state
            .lock()
            .expect("effect gate poisoned")
            .active
            .remove(&id);
    }
}

#[derive(Debug)]
pub(crate) struct EffectPermit {
    gate: Weak<EffectGate>,
    id: u64,
    stop: CancellationToken,
}

impl EffectPermit {
    #[must_use]
    pub(crate) fn stop(&self) -> CancellationToken {
        self.stop.clone()
    }
}

impl Drop for EffectPermit {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.upgrade() {
            gate.release(self.id);
        }
    }
}

pub(crate) fn signal(tokens: impl IntoIterator<Item = CancellationToken>) {
    for token in tokens {
        token.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealing_rejects_new_effects_and_signals_only_when_requested() {
        let gate = Arc::new(EffectGate::new());
        let permit = gate.admit().expect("admitted");
        let stop = permit.stop();
        let tokens = gate.seal();
        assert!(!stop.is_cancelled());
        assert!(gate.admit().is_none());
        signal(tokens);
        assert!(stop.is_cancelled());
    }
}
