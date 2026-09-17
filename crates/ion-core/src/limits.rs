//! Session-wide bounds.
//!
//! Per-turn ceilings are part of the frozen conversation configuration. These
//! are the bounds that belong to the session itself: how much durable content
//! it will hold, how much of that is held back for control and settlement, how
//! many inputs may wait, and how many commands may wait for the database.

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionLimits {
    /// Total durable content budget for entries and inputs.
    pub quota_bytes: u64,
    /// Capacity held back from ordinary growth so that a turn can always record
    /// a truthful outcome, a cancellation or a settlement.
    ///
    /// Without it, an accepted input could exhaust the quota and leave an
    /// already-performed external action with nowhere to report its result.
    pub reserved_bytes: u64,
    /// How many inputs may wait for a turn.
    pub max_queued_inputs: u32,
    /// Bounded command queue between clients and the database thread.
    pub command_capacity: usize,
    /// How long core waits for a stopped action to report before the supervisor
    /// keeps owning it.
    ///
    /// The grace bounds a cancelled turn's join and a close, and an action that
    /// outlasts it is not dropped: ownership and its unresolved outcome are
    /// retained, and close reports that it is still closing.
    pub execution_join_grace_ms: u64,
}

impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            quota_bytes: 512 * 1024 * 1024,
            reserved_bytes: 8 * 1024 * 1024,
            max_queued_inputs: 1_024,
            command_capacity: 256,
            execution_join_grace_ms: 2_000,
        }
    }
}

impl SessionLimits {
    pub fn validate(&self) -> Result<(), LimitsError> {
        if self.quota_bytes == 0 {
            return Err(LimitsError::NonPositive {
                setting: "quota_bytes",
            });
        }
        if self.reserved_bytes >= self.quota_bytes {
            return Err(LimitsError::ReserveNotBelowQuota {
                reserved_bytes: self.reserved_bytes,
                quota_bytes: self.quota_bytes,
            });
        }
        if self.max_queued_inputs == 0 {
            return Err(LimitsError::NonPositive {
                setting: "max_queued_inputs",
            });
        }
        if self.command_capacity == 0 {
            return Err(LimitsError::NonPositive {
                setting: "command_capacity",
            });
        }
        if self.execution_join_grace_ms == 0 {
            // A zero grace would make every stop unconfirmable, which reads as
            // "never join" rather than as a limit; that is not a bound.
            return Err(LimitsError::NonPositive {
                setting: "execution_join_grace_ms",
            });
        }
        Ok(())
    }

    /// The bounded wait for a stopped action to report.
    #[must_use]
    pub const fn execution_join_grace(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.execution_join_grace_ms)
    }

    /// How much ordinary growth may occupy before admission stops.
    #[must_use]
    pub const fn admission_ceiling(&self) -> u64 {
        self.quota_bytes - self.reserved_bytes
    }

    /// Whether one more payload of `size` fits in ordinary capacity.
    #[must_use]
    pub const fn admits(&self, used_bytes: u64, size: u64) -> bool {
        used_bytes.saturating_add(size) <= self.admission_ceiling()
    }

    /// Whether bounded control or settlement metadata of `size` fits in the
    /// reserve. Settlement never consults the admission ceiling.
    #[must_use]
    pub const fn admits_settlement(&self, used_bytes: u64, size: u64) -> bool {
        used_bytes.saturating_add(size) <= self.quota_bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LimitsError {
    #[error("{setting} must be positive")]
    NonPositive { setting: &'static str },
    #[error(
        "reserved_bytes ({reserved_bytes}) must be below quota_bytes ({quota_bytes}); a reserve that consumes the quota reserves nothing"
    )]
    ReserveNotBelowQuota {
        reserved_bytes: u64,
        quota_bytes: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserve_is_unreachable_by_ordinary_growth() {
        let limits = SessionLimits {
            quota_bytes: 1_000,
            reserved_bytes: 200,
            max_queued_inputs: 1,
            command_capacity: 1,
            ..SessionLimits::default()
        };
        assert!(limits.admits(799, 1));
        assert!(!limits.admits(800, 1));
        // The reserve still admits settlement at the same used total.
        assert!(limits.admits_settlement(800, 200));
        assert!(!limits.admits_settlement(800, 201));
    }

    #[test]
    fn a_reserve_above_the_quota_is_refused() {
        let limits = SessionLimits {
            quota_bytes: 100,
            reserved_bytes: 100,
            ..SessionLimits::default()
        };
        assert!(matches!(
            limits.validate(),
            Err(LimitsError::ReserveNotBelowQuota { .. })
        ));
    }
}
