mod memory;
pub(crate) mod sqlite;

pub(crate) use memory::MemoryStore;

use std::path::PathBuf;

use crate::session::SessionError;
use crate::session::transaction::MutationBatch;

/// Persistence accepts only a fully validated batch. It never decides readiness
/// or changes resident semantic state. Any error fences the owner conservatively.
pub(crate) trait Persistence: std::fmt::Debug + Send {
    fn commit(&mut self, batch: &MutationBatch) -> Result<(), StoreError>;

    /// Give up cross-process writable ownership, if this store holds any.
    ///
    /// Called once the owner has fenced canonical writes and joined its
    /// invocations, so another process may take the session over. Store backends
    /// without a cross-process ownership domain do nothing.
    fn release_ownership(&mut self) {}
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum StoreError {
    #[error("{0}")]
    Other(String),
    /// Another live process holds writable ownership of this session database.
    /// It is the one persistence failure a client can act on by waiting or
    /// pointing at the other process, so it stays distinguishable from a store
    /// that simply could not complete the operation.
    #[error("session database {} is owned by another live process", database.display())]
    OwnershipConflict { database: PathBuf },
}

impl StoreError {
    pub(crate) fn other(message: impl Into<String>) -> Self {
        Self::Other(message.into())
    }

    pub(crate) fn ownership_conflict(database: impl Into<PathBuf>) -> Self {
        Self::OwnershipConflict {
            database: database.into(),
        }
    }

    /// The public session error this failure becomes.
    pub(crate) fn into_session_error(self) -> SessionError {
        match self {
            Self::OwnershipConflict { database } => SessionError::SessionInUse(database),
            Self::Other(message) => SessionError::Persistence(message),
        }
    }
}
