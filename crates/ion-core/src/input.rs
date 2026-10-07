//! Admission and retained-byte ownership for host inputs awaiting Session commit.
use std::{
    io::{self, Write},
    sync::{Arc, Mutex},
};

use ion_ai::{Content, Message};
use serde::Serialize;

use crate::{AgentLimits, CodingAgentError, session::valid_user_message};

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

    /// Validate before accepting ownership. Metadata is retained host data (for
    /// example a correlation ID or image notes), never model-visible content.
    pub fn admit(
        &self,
        message: Message,
        metadata: &impl Serialize,
        limits: AgentLimits,
    ) -> Result<AcceptedInput, CodingAgentError> {
        let reservation = self.reserve(&message, metadata, limits)?;
        Ok(AcceptedInput {
            message,
            reservation,
        })
    }

    /// Reserve for a host-owned editor input without retaining a duplicate
    /// message. Keep the returned owner alongside the unchanged accepted input.
    pub fn reserve(
        &self,
        message: &Message,
        metadata: &impl Serialize,
        limits: AgentLimits,
    ) -> Result<InputReservation, CodingAgentError> {
        validate_input(message, limits)?;
        let bytes = encoded_bytes(&(message, metadata))?;
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if bytes > self.limit.saturating_sub(*used) {
            return Err(CodingAgentError::InputQueueFull {
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

/// An accepted input retains its budget while moving between host queues.
/// Releasing it into a Session/editor/response transfers ownership, not replay.
pub struct AcceptedInput {
    message: Message,
    reservation: InputReservation,
}

impl AcceptedInput {
    pub fn message(&self) -> &Message {
        &self.message
    }

    pub fn into_message(self) -> Message {
        self.message
    }

    pub fn into_parts(self) -> (Message, InputReservation) {
        (self.message, self.reservation)
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

pub(crate) fn validate_input(
    message: &Message,
    limits: AgentLimits,
) -> Result<(), CodingAgentError> {
    if !valid_user_message(message) {
        return Err(CodingAgentError::InvalidUserInput);
    }
    if !limits.image_input
        && message
            .content
            .iter()
            .any(|part| matches!(part, Content::Image(_)))
    {
        return Err(CodingAgentError::ImagesUnsupported);
    }
    if encoded_bytes(message)? > limits.max_request_bytes {
        return Err(CodingAgentError::InputTooLarge {
            max_encoded_bytes: limits.max_request_bytes,
        });
    }
    Ok(())
}

fn encoded_bytes(value: &impl Serialize) -> Result<usize, serde_json::Error> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("input size overflow"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)?;
    Ok(counter.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_admission_accounts_metadata_and_retains_transferred_inputs() {
        let message = Message::user_input("next".into(), []);
        let size = encoded_bytes(&(&message, &())).unwrap();
        let budget = InputBudget::new(size * 2);
        let steering = budget
            .admit(message.clone(), &(), AgentLimits::default())
            .unwrap();
        let follow_up = budget
            .clone()
            .admit(message.clone(), &(), AgentLimits::default())
            .unwrap();
        assert!(matches!(
            budget.admit(message.clone(), &(), AgentLimits::default()),
            Err(CodingAgentError::InputQueueFull { .. })
        ));
        // Moving accepted steering to another host queue must not free capacity.
        let restored = steering;
        assert!(
            budget
                .admit(message.clone(), &(), AgentLimits::default())
                .is_err()
        );
        assert_eq!(restored.into_message(), message);
        assert!(
            budget
                .admit(
                    message.clone(),
                    &"large metadata".repeat(size),
                    AgentLimits::default()
                )
                .is_err()
        );
        let next = budget
            .admit(message.clone(), &(), AgentLimits::default())
            .unwrap();
        drop(follow_up);
        drop(next);
        assert_eq!(*budget.used.lock().unwrap(), 0);
        assert!(budget.admit(message, &(), AgentLimits::default()).is_ok());
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
            budget.admit(input, &(), limits),
            Err(CodingAgentError::InputTooLarge { .. })
        ));
        let image = ion_ai::ImageContent::from_bytes(&[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ])
        .unwrap();
        assert!(matches!(
            budget.admit(
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
            Err(CodingAgentError::ImagesUnsupported)
        ));
        assert!(matches!(
            budget.admit(
                Message::user_input("".into(), []),
                &(),
                AgentLimits::default()
            ),
            Err(CodingAgentError::InvalidUserInput)
        ));
        assert_eq!(*budget.used.lock().unwrap(), 0);
    }
}
