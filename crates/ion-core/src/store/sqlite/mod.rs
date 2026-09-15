//! The SQLite implementation of the session store.
//!
//! Every module here owns one group of tables and the transactions that write
//! them. Nothing outside this module imports `rusqlite`.
//!
//! Durability floor: WAL journalling with `synchronous = FULL`, foreign keys
//! on. A committed transaction is durable across process death and OS failure
//! to the extent the filesystem honours `fsync`.

mod codec;
mod connection;
pub(crate) mod conversation;
pub(crate) mod entry;
pub(crate) mod input;
mod ownership;
mod schema;
mod sequence;
pub(crate) mod turn;

use std::path::Path;

use rusqlite::Connection;

use super::{SessionInfo, StoreError};
use crate::error::Error;
use crate::id::IdError;

/// One open session database, owned by the database thread.
pub(crate) struct SqliteStore {
    connection: Connection,
    /// Writable ownership of the database file. Held, never read: dropping it
    /// releases the OS lock.
    _ownership: Option<ownership::Ownership>,
    /// Test-only: the next committing transaction fails before it publishes.
    #[cfg(test)]
    fault: std::cell::Cell<bool>,
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteStore")
            .finish_non_exhaustive()
    }
}

impl SqliteStore {
    /// Create a new session database. Refuses a database that already has a
    /// schema instead of overwriting one.
    ///
    /// Ownership is taken before the database is opened, so no pragma, schema
    /// write or read can touch a session another live process owns.
    pub(crate) fn create(path: &Path) -> Result<Self, StoreError> {
        let ownership = ownership::Ownership::acquire(path)?;
        let connection = connection::create(path)?;
        schema::initialize(&connection)?;
        Ok(Self {
            connection,
            _ownership: Some(ownership),
            #[cfg(test)]
            fault: std::cell::Cell::new(false),
        })
    }

    /// Open an existing session database.
    ///
    /// Opening reads identity and metadata only. It never reconstructs the
    /// transcript, and it never starts work.
    pub(crate) fn open(path: &Path) -> Result<Self, StoreError> {
        if !path.exists() {
            return Err(StoreError::Rejected(Error::UnknownSession(
                path.to_path_buf(),
            )));
        }
        let ownership = ownership::Ownership::acquire(path)?;
        let connection = connection::open(path)?;
        schema::verify(&connection)?;
        Ok(Self {
            connection,
            _ownership: Some(ownership),
            #[cfg(test)]
            fault: std::cell::Cell::new(false),
        })
    }

    /// Read the durable session identity, refusing a database that was never
    /// initialized or that cannot be decoded.
    pub(crate) fn check_readable(&self) -> Result<SessionInfo, StoreError> {
        connection::read_info(&self.connection)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Failed(format!("sqlite: {error}"))
    }
}

/// Test-only: make the next committing transaction fail before it publishes.
#[cfg(test)]
pub(crate) fn inject_commit_failure(store: &SqliteStore) {
    store.fault.set(true);
}

/// Test-only: arm the next committing transaction to fail.
#[cfg(test)]
pub(crate) struct InjectFault;

#[cfg(test)]
impl crate::store::Command for InjectFault {
    type Output = ();

    fn apply(self, store: &mut SqliteStore) -> Result<(), StoreError> {
        inject_commit_failure(store);
        Ok(())
    }
}

/// Fail a transaction that was asked to fail, before it publishes anything.
///
/// The flag is passed by field so a caller already holding a mutable borrow of
/// the connection can still reach it.
#[cfg(test)]
pub(crate) fn check_fault(fault: &std::cell::Cell<bool>) -> Result<(), StoreError> {
    if fault.replace(false) {
        return Err(StoreError::Failed("injected commit failure".to_owned()));
    }
    Ok(())
}

/// Map a local sequence into a typed identity.
pub(crate) fn id_from<T: TryFrom<i64, Error = IdError>>(raw: i64) -> Result<T, StoreError> {
    T::try_from(raw).map_err(|error| StoreError::Failed(format!("invalid local id {raw}: {error}")))
}
