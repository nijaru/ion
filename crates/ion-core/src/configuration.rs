//! Admission exclusion for one process-owned host configuration.
//!
//! Read leases belong to complete operation/admission lifetimes, not individual
//! model calls. Reload never waits inside a session writer: a busy host rejects
//! the update, leaving cancellation and observation available.

use crate::CommandError;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

#[derive(Clone, Debug, Default)]
pub struct HostConfiguration {
    state: Arc<ConfigurationState>,
}

#[derive(Debug, Default)]
struct ConfigurationState {
    barrier: Arc<RwLock<()>>,
    failed: AtomicBool,
}

#[derive(Debug)]
pub struct ConfigurationLease {
    _guard: OwnedRwLockReadGuard<()>,
}

#[must_use = "finish a successful or unchanged update; dropping fails admission closed"]
pub struct ConfigurationUpdate {
    state: Arc<ConfigurationState>,
    _guard: OwnedRwLockWriteGuard<()>,
    finished: bool,
    was_failed: bool,
}

impl HostConfiguration {
    pub fn try_enter(&self) -> Result<ConfigurationLease, CommandError> {
        let guard = Arc::clone(&self.state.barrier)
            .try_read_owned()
            .map_err(|_| CommandError::ConfigurationBusy)?;
        if self.state.failed.load(Ordering::Acquire) {
            return Err(CommandError::ConfigurationFailed);
        }
        Ok(ConfigurationLease { _guard: guard })
    }

    pub fn try_update(&self) -> Result<ConfigurationUpdate, CommandError> {
        let guard = Arc::clone(&self.state.barrier)
            .try_write_owned()
            .map_err(|_| CommandError::ConfigurationBusy)?;
        Ok(ConfigurationUpdate {
            state: Arc::clone(&self.state),
            _guard: guard,
            finished: false,
            was_failed: self.state.failed.load(Ordering::Acquire),
        })
    }
}

impl ConfigurationUpdate {
    pub(crate) fn belongs_to(&self, configuration: &HostConfiguration) -> bool {
        Arc::ptr_eq(&self.state, &configuration.state)
    }

    /// Release an update that made no durable or external changes, preserving
    /// whether a prior failed reconciliation already fenced this host.
    pub fn unchanged(mut self) {
        self.state.failed.store(self.was_failed, Ordering::Release);
        self.finished = true;
    }

    /// Publish successful reconciliation and permit new work.
    pub fn finish(mut self) {
        self.state.failed.store(false, Ordering::Release);
        self.finished = true;
    }
}

impl Drop for ConfigurationUpdate {
    fn drop(&mut self) {
        if !self.finished {
            // Fields (including the write guard) drop after this method.
            self.state.failed.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_excludes_admission_and_existing_work_excludes_update() {
        let configuration = HostConfiguration::default();
        let lease = configuration.try_enter().unwrap();
        assert!(matches!(
            configuration.try_update(),
            Err(CommandError::ConfigurationBusy)
        ));
        drop(lease);
        let update = configuration.try_update().unwrap();
        assert!(matches!(
            configuration.try_enter(),
            Err(CommandError::ConfigurationBusy)
        ));
        update.finish();
        assert!(configuration.try_enter().is_ok());
    }

    #[test]
    fn unfinished_update_fails_closed_until_successful_reconciliation() {
        let configuration = HostConfiguration::default();
        drop(configuration.try_update().unwrap());
        assert!(matches!(
            configuration.try_enter(),
            Err(CommandError::ConfigurationFailed)
        ));
        configuration.try_update().unwrap().finish();
        assert!(configuration.try_enter().is_ok());
    }
    #[test]
    fn unchanged_preserves_previous_failure_and_owner_identity() {
        let configuration = HostConfiguration::default();
        let other = HostConfiguration::default();
        let update = configuration.try_update().unwrap();
        assert!(update.belongs_to(&configuration));
        assert!(!update.belongs_to(&other));
        update.unchanged();
        assert!(configuration.try_enter().is_ok());
        drop(configuration.try_update().unwrap());
        configuration.try_update().unwrap().unchanged();
        assert!(matches!(
            configuration.try_enter(),
            Err(CommandError::ConfigurationFailed)
        ));
    }
}
