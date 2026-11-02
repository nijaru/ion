//! Preparation, retained-byte custody and commit receipts for Session inputs.
use std::sync::{Arc, Mutex};

use ion_ai::{Content, Message};
use serde::Serialize;
use tokio::sync::watch;
use uuid::Uuid;

use crate::{AgentError, AgentLimits, json_size::encoded_len, session::valid_user_message};

/// Shared by steering and follow-ups, not a process-RSS or model-context quota.
/// The default preserves the existing RPC retained-input allowance.
#[derive(Clone)]
pub struct InputBudget {
    limit: usize,
    used: Arc<Mutex<usize>>,
}

impl Default for InputBudget {
    fn default() -> Self {
        Self::new(32 * 1024 * 1024)
    }
}

impl InputBudget {
    pub fn new(max_encoded_bytes: usize) -> Self {
        Self {
            limit: max_encoded_bytes,
            used: Arc::new(Mutex::new(0)),
        }
    }

    /// Prepare bounded input custody, not durable admission. Metadata stays
    /// outside model content and the Session journal.
    pub fn prepare<M: Serialize>(
        &self,
        message: Message,
        metadata: M,
        limits: AgentLimits,
    ) -> Result<PreparedInput<M>, AgentError> {
        let reservation = self.reserve(&message, &metadata, limits)?;
        let (disposition, _) = watch::channel(InputStatus::Pending);
        Ok(PreparedInput {
            disposition,
            message,
            metadata,
            reservation,
        })
    }

    /// Reserve for a host-owned editor input without retaining a duplicate
    /// message. Keep the returned owner alongside the unchanged prepared input.
    pub fn reserve(
        &self,
        message: &Message,
        metadata: &impl Serialize,
        limits: AgentLimits,
    ) -> Result<InputReservation, AgentError> {
        validate_input(message, limits)?;
        let bytes = encoded_len(&(message, metadata))?;
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if bytes > self.limit.saturating_sub(*used) {
            return Err(AgentError::InputQueueFull {
                max_encoded_bytes: self.limit,
            });
        }
        *used += bytes;
        drop(used);
        Ok(InputReservation {
            used: self.used.clone(),
            bytes,
        })
    }
}

/// The durable coordinates of one locally tracked input. This is an
/// acknowledgement, not a durable request key or permission to replay input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct InputCommit {
    pub session: Uuid,
    pub turn: u64,
    pub entry: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum InputStatus {
    Pending,
    Committing,
    Committed(InputCommit),
    NotAccepted,
    /// The commit owner disappeared after beginning work. Reconcile the Session;
    /// neither requesting stop nor losing this acknowledgement proves rollback.
    Unresolved,
}

/// Read-only, process-local input identity and commit disposition. Cloning a
/// receipt does not acquire execution, withdrawal or replay authority.
#[derive(Debug, Clone)]
pub struct InputReceipt(watch::Receiver<InputStatus>);

impl InputReceipt {
    pub fn status(&self) -> InputStatus {
        // Keep the state borrow through the closure check. Otherwise a commit
        // followed by sender drop could turn a stale Pending into NotAccepted.
        let status = self.0.borrow();
        match (*status, self.0.has_changed().is_err()) {
            (InputStatus::Pending, true) => InputStatus::NotAccepted,
            (InputStatus::Committing, true) => InputStatus::Unresolved,
            (status, _) => status,
        }
    }

    pub fn same_input(&self, other: &Self) -> bool {
        self.0.same_channel(&other.0)
    }

    pub async fn changed(&mut self) -> Result<(), watch::error::RecvError> {
        self.0.changed().await
    }
}

/// Once-owned prepared input, including the sole authority to publish its
/// commit. Dropping pending custody proves nonacceptance; dropping committing
/// custody leaves an unresolved outcome instead.
pub struct PreparedInput<M = ()> {
    // Close disposition before any arbitrary metadata destructor runs.
    disposition: watch::Sender<InputStatus>,
    message: Message,
    metadata: M,
    reservation: InputReservation,
}

impl<M> PreparedInput<M> {
    pub fn message(&self) -> &Message {
        &self.message
    }

    pub fn receipt(&self) -> InputReceipt {
        InputReceipt(self.disposition.subscribe())
    }

    pub fn into_message(self) -> Message {
        self.into_parts().0
    }

    pub fn into_parts(self) -> (Message, M, InputReservation) {
        drop(self.disposition);
        (self.message, self.metadata, self.reservation)
    }

    pub(crate) fn begin(&self) {
        assert_eq!(*self.disposition.borrow(), InputStatus::Pending);
        self.disposition.send_replace(InputStatus::Committing);
    }

    /// Only a known precommit failure can make this custody pending again.
    pub(crate) fn not_committed(&self) {
        self.disposition.send_replace(InputStatus::Pending);
    }

    pub(crate) fn committed(self, commit: InputCommit) -> (Message, M, Option<CommittedInput>) {
        self.disposition
            .send_replace(InputStatus::Committed(commit));
        let acknowledgement = if self.disposition.receiver_count() > 0 {
            Some(CommittedInput {
                receipt: self.receipt(),
                _reservation: self.reservation,
            })
        } else {
            drop(self.reservation);
            None
        };
        (self.message, self.metadata, acknowledgement)
    }
}

/// Ordered steering acknowledgement. Unread delivery keeps the input's bounded
/// reservation, but not its arbitrary metadata or a duplicate message.
pub struct CommittedInput {
    receipt: InputReceipt,
    _reservation: InputReservation,
}

impl CommittedInput {
    pub fn receipt(&self) -> &InputReceipt {
        &self.receipt
    }
}

/// Retained accounting ownership; moving it never releases capacity.
pub struct InputReservation {
    used: Arc<Mutex<usize>>,
    bytes: usize,
}

impl Drop for InputReservation {
    fn drop(&mut self) {
        // No user code runs with the accounting lock held. Recovering a poisoned
        // lock during cleanup still releases this private, once-owned reservation.
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *used -= self.bytes;
    }
}

pub(crate) fn validate_input(message: &Message, limits: AgentLimits) -> Result<(), AgentError> {
    if !valid_user_message(message) {
        return Err(AgentError::InvalidUserInput);
    }
    if !limits.image_input
        && message
            .content
            .iter()
            .any(|part| matches!(part, Content::Image(_)))
    {
        return Err(AgentError::ImagesUnsupported);
    }
    if encoded_len(message)? > limits.max_request_bytes {
        return Err(AgentError::InputTooLarge {
            max_encoded_bytes: limits.max_request_bytes,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_receipts_distinguish_abandonment_from_unresolved_commit() {
        let budget = InputBudget::default();
        let prepare = || {
            budget
                .prepare(
                    Message::user_input("same".into(), []),
                    (),
                    AgentLimits::default(),
                )
                .unwrap()
        };
        let pending = prepare();
        let receipt = pending.receipt();
        assert!(receipt.same_input(&receipt.clone()));
        assert_eq!(receipt.status(), InputStatus::Pending);
        let committing = prepare();
        let unresolved = committing.receipt();
        assert!(!receipt.same_input(&unresolved));
        committing.begin();
        assert_eq!(unresolved.status(), InputStatus::Committing);
        drop(pending);
        drop(committing);
        assert_eq!(receipt.status(), InputStatus::NotAccepted);
        assert_eq!(unresolved.status(), InputStatus::Unresolved);
    }

    #[test]
    fn committed_receipts_survive_owner_drop_and_bound_unread_delivery() {
        let message = Message::user_input("input".into(), []);
        let budget = InputBudget::new(encoded_len(&(&message, &())).unwrap());
        let input = budget
            .prepare(message.clone(), (), AgentLimits::default())
            .unwrap();
        let receipt = input.receipt();
        input.begin();
        let commit = InputCommit {
            session: Uuid::now_v7(),
            turn: 2,
            entry: 7,
        };
        let (_, (), acknowledgement) = input.committed(commit);
        assert_eq!(receipt.status(), InputStatus::Committed(commit));
        assert!(
            acknowledgement
                .as_ref()
                .unwrap()
                .receipt()
                .same_input(&receipt)
        );
        assert!(
            budget
                .prepare(message.clone(), (), AgentLimits::default())
                .is_err()
        );
        drop(acknowledgement);
        assert_eq!(receipt.status(), InputStatus::Committed(commit));
        assert!(budget.prepare(message, (), AgentLimits::default()).is_ok());
    }

    #[test]
    fn known_precommit_failure_preserves_pending_custody() {
        let input = InputBudget::default()
            .prepare(
                Message::user_input("input".into(), []),
                (),
                AgentLimits::default(),
            )
            .unwrap();
        let receipt = input.receipt();
        input.begin();
        input.not_committed();
        assert_eq!(receipt.status(), InputStatus::Pending);
        drop(input);
        assert_eq!(receipt.status(), InputStatus::NotAccepted);
    }

    #[test]
    fn shared_preparation_accounts_metadata_and_retains_transferred_inputs() {
        let message = Message::user_input("next".into(), []);
        let size = encoded_len(&(&message, &())).unwrap();
        let budget = InputBudget::new(size * 2);
        let steering = budget
            .prepare(message.clone(), &(), AgentLimits::default())
            .unwrap();
        let follow_up = budget
            .clone()
            .prepare(message.clone(), &(), AgentLimits::default())
            .unwrap();
        assert!(matches!(
            budget.prepare(message.clone(), &(), AgentLimits::default()),
            Err(AgentError::InputQueueFull { .. })
        ));
        // Moving prepared steering to another host queue must not free capacity.
        let restored = steering;
        assert!(
            budget
                .prepare(message.clone(), &(), AgentLimits::default())
                .is_err()
        );
        assert_eq!(restored.into_message(), message);
        assert!(
            budget
                .prepare(
                    message.clone(),
                    &"large metadata".repeat(size),
                    AgentLimits::default()
                )
                .is_err()
        );
        let next = budget
            .prepare(message.clone(), &(), AgentLimits::default())
            .unwrap();
        drop(follow_up);
        drop(next);
        assert_eq!(*budget.used.lock().unwrap(), 0);
        assert!(budget.prepare(message, &(), AgentLimits::default()).is_ok());
    }

    #[test]
    fn owned_metadata_and_reservation_survive_input_transfer() {
        let message = Message::user_input("prepared".into(), []);
        let metadata = ("  /check ARG 🦀\n".to_owned(), 3usize);
        let bytes = encoded_len(&(&message, &metadata)).unwrap();
        let budget = InputBudget::new(bytes);
        let input = budget
            .prepare(message.clone(), metadata.clone(), AgentLimits::default())
            .unwrap();
        let (prepared, original, reservation) = input.into_parts();
        assert_eq!(prepared, message);
        assert_eq!(original, metadata);
        assert!(matches!(
            budget.prepare(message.clone(), (), AgentLimits::default()),
            Err(AgentError::InputQueueFull { .. })
        ));
        drop(reservation);
        assert!(
            budget
                .prepare(message, metadata, AgentLimits::default())
                .is_ok()
        );
    }

    #[test]
    fn rejection_precedes_admission_for_route_size_and_invalid_content() {
        let budget = InputBudget::default();
        let input = Message::user_input("escaped\n".repeat(10), []);
        let limits = AgentLimits {
            max_request_bytes: 40,
            ..AgentLimits::default()
        };
        assert!(matches!(
            budget.prepare(input, &(), limits),
            Err(AgentError::InputTooLarge { .. })
        ));
        let image = ion_ai::ImageContent::from_bytes(&[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ])
        .unwrap();
        assert!(matches!(
            budget.prepare(
                Message::user_input(
                    "image".into(),
                    [ion_ai::LoadedImage {
                        content: image,
                        note: None
                    }]
                ),
                &(),
                AgentLimits::default()
            ),
            Err(AgentError::ImagesUnsupported)
        ));
        assert!(matches!(
            budget.prepare(
                Message::user_input("".into(), []),
                &(),
                AgentLimits::default()
            ),
            Err(AgentError::InvalidUserInput)
        ));
        let mut input = Message::user_input("question".into(), []);
        input
            .content
            .push(ion_ai::Content::Thinking("not user input".into()));
        assert!(matches!(
            budget.prepare(input, &(), AgentLimits::default()),
            Err(AgentError::InvalidUserInput)
        ));
        assert_eq!(*budget.used.lock().unwrap(), 0);
    }
}
