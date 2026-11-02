//! Scoped custody of native Session resource destruction.
use std::sync::{Arc, Mutex};

use super::{Session, SessionError, SessionRetirement};

/// Own this scope outside Session control and any outstanding reader futures.
/// After stopping/joining control and releasing all Session references, join the
/// scope before releasing its runtime. Binding changes never wait for external
/// Session references. No Session is kept alive by this owner.
#[derive(Clone, Default)]
pub struct SessionRetirements(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    closed: bool,
    receipts: Vec<SessionRetirement>,
}

impl SessionRetirements {
    pub(super) fn try_join(&self) -> Option<Result<(), SessionError>> {
        let receipts = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .receipts
            .clone();
        for receipt in receipts {
            match receipt.try_join() {
                Some(Ok(())) => {}
                other => return other,
            }
        }
        Some(Ok(()))
    }

    pub fn released(&self) -> Vec<SessionRetirement> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .receipts
            .iter()
            .filter(|receipt| receipt.is_released())
            .cloned()
            .collect()
    }
    /// Register before releasing any application-owned Session reference.
    pub fn register(&self, session: &Session) -> Result<(), SessionError> {
        // Reap completed success, but preserve unread native/IO failures. Keep
        // this snapshot until after unlocking, including removed receipts.
        let snapshot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .receipts
            .clone();
        let done: Vec<_> = snapshot
            .iter()
            .filter(|receipt| matches!(receipt.try_join(), Some(Ok(()))))
            .collect();
        {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.closed {
                return Err(SessionError::RetirementClosed);
            }
            state
                .receipts
                .retain(|receipt| !done.iter().any(|done| done.same_session(receipt)));
            let receipt = session.retirement();
            if !state
                .receipts
                .iter()
                .any(|existing| existing.same_session(&receipt))
            {
                state.receipts.push(receipt);
            }
        }
        Ok(())
    }

    /// Close resource admission and join actual Store teardown. This borrowed
    /// wait can be abandoned and rejoined; it neither revokes external readers
    /// nor discards native failures. All Session references must first be freed.
    pub async fn join(&self) -> Result<(), SessionError> {
        let receipts = {
            let mut state = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.closed = true;
            state.receipts.clone()
        };
        let mut errors = Vec::new();
        for receipt in receipts {
            if let Err(error) = receipt.join().await {
                errors.push(error.to_string());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(SessionError::Storage(errors.join("; ")))
        }
    }
}
