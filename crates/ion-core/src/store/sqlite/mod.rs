//! Per-session SQLite persistence.
//!
//! One database holds one session. `Session` remains the only semantic writer:
//! this store accepts a validated write set, applies it atomically, and can
//! reconstruct equivalent resident state on open. No other module imports
//! `rusqlite` or holds a connection.
//!
//! Reads are fallible: [`SqliteStore::open`] returns a [`StoreError`] for a
//! missing file, an unsupported schema or a corrupt row, which is deliberately
//! distinct from "the record is absent".

mod commit;
mod connection;
mod conversation;
mod entry;
mod input;
mod ownership;
mod schema;
mod task;

use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{Persistence, StoreError};
use crate::session::state::SessionState;
use crate::session::transaction::MutationBatch;
use crate::{IdError, SessionId};

/// Convert a stored local sequence into a typed identity.
fn id_from<T: TryFrom<i64, Error = IdError>>(raw: i64) -> Result<T, StoreError> {
    T::try_from(raw)
        .map_err(|error| StoreError::other(format!("invalid local sequence {raw}: {error}")))
}

/// Keep SQLite error text inside this module's error surface; no caller outside
/// `store::sqlite` depends on the SQLite error type.
impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::other(format!("sqlite: {error}"))
    }
}

/// SQLite integers are signed 64-bit; reject an out-of-range generation rather
/// than letting it wrap into a different invocation identity.
fn sql_int(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_| StoreError::other(format!("value {value} does not fit in a sqlite integer")))
}

fn json_to<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value)
        .map_err(|error| StoreError::other(format!("encode failed: {error}")))
}

fn json_from<T: DeserializeOwned>(raw: &str) -> Result<T, StoreError> {
    serde_json::from_str(raw).map_err(|error| StoreError::other(format!("decode failed: {error}")))
}

#[derive(Debug)]
pub(crate) struct SqliteStore {
    connection: Connection,
    /// Writable ownership of the database file. `None` after an explicit
    /// release, which happens once the session has fenced writes and joined its
    /// invocations.
    ownership: Option<ownership::Ownership>,
}

impl SqliteStore {
    /// Create a new session database. Refuses a database that already has a
    /// schema instead of overwriting it.
    ///
    /// Ownership is taken before the database is opened, so no SQLite pragma,
    /// schema write or reconstruction can touch a session another live process
    /// owns.
    pub(crate) fn create(path: &Path, session_id: SessionId) -> Result<Self, StoreError> {
        let ownership = ownership::Ownership::acquire(path)?;
        let connection = connection::create(path)?;
        schema::initialize(&connection)?;
        connection::record_session_id(&connection, session_id)?;
        Ok(Self {
            connection,
            ownership: Some(ownership),
        })
    }

    /// Open an existing session database and reconstruct resident state.
    ///
    /// Reconstruction reads one explicit transaction, so it never observes a
    /// half-applied commit from a concurrent connection.
    pub(crate) fn open(path: &Path) -> Result<(Self, SessionState), StoreError> {
        if !path.exists() {
            return Err(StoreError::other(format!(
                "session database {} does not exist",
                path.display()
            )));
        }
        let ownership = ownership::Ownership::acquire(path)?;
        let connection = connection::open(path)?;
        schema::verify(&connection)?;
        let session_id = connection::read_session_id(&connection)?;
        let transaction = connection.unchecked_transaction()?;
        let state = load(&transaction, session_id)?;
        transaction.commit()?;
        Ok((
            Self {
                connection,
                ownership: Some(ownership),
            },
            state,
        ))
    }
}

impl Persistence for SqliteStore {
    fn commit(&mut self, batch: &MutationBatch) -> Result<(), StoreError> {
        commit::apply(&mut self.connection, batch)
    }

    fn release_ownership(&mut self) {
        // Dropping the handle releases the kernel-held lock.
        drop(self.ownership.take());
    }
}

/// Rebuild resident semantic state from durable records.
///
/// This is the open path: it only reads. A task that was running when the
/// process died stays `Running` here and is entered through an explicit
/// recovery drive, so opening a session never starts work.
fn load(connection: &Connection, session_id: SessionId) -> Result<SessionState, StoreError> {
    let meta = connection::read_meta(connection)?;
    let mut state = SessionState::empty(session_id);
    state.last_seq = meta.last_seq.map(id_from).transpose()?;
    state.last_commit = meta.last_commit.map(id_from).transpose()?;
    state.root_conversation = meta.root_conversation.map(id_from).transpose()?;
    if state.root_conversation.is_none() {
        return Err(StoreError::other(
            "session database has no root conversation; it was never initialized".to_owned(),
        ));
    }

    for conversation in conversation::load(connection)? {
        state
            .conversations
            .insert(conversation.id, std::sync::Arc::new(conversation));
    }
    // The index is written by the same owner a live append uses, so it cannot
    // drift from the records it indexes.
    for entry in entry::load(connection)? {
        state
            .insert_entry(entry)
            .map_err(|error| StoreError::other(format!("invalid stored entry: {error}")))?;
    }
    for (stored_input, admitted_at) in input::load(connection)? {
        if let Some(key) = &stored_input.request_key {
            state.request_keys.insert(key.clone(), stored_input.id);
        }
        if let Some(commit_seq) = admitted_at {
            state.input_commits.insert(stored_input.id, commit_seq);
        }
        state
            .insert_input(stored_input)
            .map_err(|error| StoreError::other(format!("invalid stored input: {error}")))?;
    }
    for stored_task in task::load(connection)? {
        state
            .insert_task(stored_task)
            .map_err(|error| StoreError::other(format!("invalid stored task: {error}")))?;
    }

    Ok(state)
}
