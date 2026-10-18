//! Independent bounds for retained response data and transport/framing work.
use super::{ProviderError, Value, invalid};

const MAX_DECODED_RESPONSE: usize = 8 * 1024 * 1024;
// Independent work allowance: the 40,000-frame regression carries only 80,000
// text bytes in more than 8 MiB of wire data. This is not a token-to-wire ratio.
const MAX_WIRE_RESPONSE: usize = 64 * 1024 * 1024;
const MAX_WITHOUT_PROGRESS: usize = 8 * 1024 * 1024;

/// Monotonic admission accounting for newly retained strings, values and nodes,
/// including opaque replay. Not an exact RSS or provider-token measurement.
#[derive(Default)]
pub(super) struct ResponseBudget {
    pub(super) used: usize,
}
impl ResponseBudget {
    pub(super) fn retain(&mut self, bytes: usize) -> Result<(), ProviderError> {
        let used = self.used.saturating_add(bytes);
        if used > MAX_DECODED_RESPONSE {
            return Err(invalid("provider decoded response exceeded byte limit"));
        }
        self.used = used;
        Ok(())
    }

    // Values originate in a bounded SSE frame. Serialize only newly inserted
    // values, never the growing response or a previously retained snapshot.
    pub(super) fn retain_value(&mut self, value: &Value) -> Result<(), ProviderError> {
        let bytes = serde_json::to_vec(value)
            .map_err(|_| invalid("cannot size provider response value"))?;
        self.retain(bytes.len())
    }
}

/// Work bounds are independent of output allowance. Even useful output can hit
/// the lifetime transport ceiling under extreme fragmentation; token counts do
/// not establish a universal wire-size bound. Comments and repeated metadata do
/// not refresh the smaller no-progress allowance.
#[derive(Default)]
pub(super) struct StreamBudget {
    pub(super) wire_bytes: usize,
    without_progress: usize,
}
impl StreamBudget {
    pub(super) fn byte(&mut self) -> Result<(), ProviderError> {
        if self.wire_bytes == MAX_WIRE_RESPONSE {
            return Err(invalid("provider stream exceeded transport byte limit"));
        }
        self.wire_bytes += 1;
        Ok(())
    }

    pub(super) fn frame(&mut self, bytes: usize, progress: bool) -> Result<(), ProviderError> {
        self.without_progress = if progress {
            0
        } else {
            self.without_progress.saturating_add(bytes)
        };
        if self.without_progress > MAX_WITHOUT_PROGRESS {
            return Err(invalid("provider stream exceeded no-progress byte limit"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_does_not_reset_the_lifetime_transport_bound() {
        let mut budget = StreamBudget {
            wire_bytes: MAX_WIRE_RESPONSE - 1,
            ..StreamBudget::default()
        };
        budget.frame(256, true).unwrap();
        budget.byte().unwrap();
        budget.frame(256, true).unwrap();
        assert!(
            budget
                .byte()
                .unwrap_err()
                .message
                .contains("transport byte limit")
        );
    }
}
