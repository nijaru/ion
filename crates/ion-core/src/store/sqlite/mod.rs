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
    T::try_from(raw).map_err(|error| StoreError(format!("invalid local sequence {raw}: {error}")))
}

/// Keep SQLite error text inside this module's error surface; no caller outside
/// `store::sqlite` depends on the SQLite error type.
impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self(format!("sqlite: {error}"))
    }
}

/// SQLite integers are signed 64-bit; reject an out-of-range generation rather
/// than letting it wrap into a different invocation identity.
fn sql_int(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_| StoreError(format!("value {value} does not fit in a sqlite integer")))
}

fn json_to<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|error| StoreError(format!("encode failed: {error}")))
}

fn json_from<T: DeserializeOwned>(raw: &str) -> Result<T, StoreError> {
    serde_json::from_str(raw).map_err(|error| StoreError(format!("decode failed: {error}")))
}

#[derive(Debug)]
pub(crate) struct SqliteStore {
    connection: Connection,
}

impl SqliteStore {
    /// Create a new session database. Refuses a database that already has a
    /// schema instead of overwriting it.
    pub(crate) fn create(path: &Path, session_id: SessionId) -> Result<Self, StoreError> {
        let connection = connection::create(path)?;
        schema::initialize(&connection)?;
        connection::record_session_id(&connection, session_id)?;
        Ok(Self { connection })
    }

    /// Open an existing session database and reconstruct resident state.
    pub(crate) fn open(path: &Path) -> Result<(Self, SessionState), StoreError> {
        let connection = connection::open(path)?;
        schema::verify(&connection)?;
        let session_id = connection::read_session_id(&connection)?;
        let state = load(&connection, session_id)?;
        Ok((Self { connection }, state))
    }
}

impl Persistence for SqliteStore {
    fn commit(&mut self, batch: &MutationBatch) -> Result<(), StoreError> {
        commit::apply(&mut self.connection, batch)
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
        return Err(StoreError(
            "session database has no root conversation; it was never initialized".to_owned(),
        ));
    }

    for conversation in conversation::load(connection)? {
        state
            .conversations
            .insert(conversation.id, std::sync::Arc::new(conversation));
    }
    for entry in entry::load(connection)? {
        state.entries.insert(entry.id, std::sync::Arc::new(entry));
    }
    for (stored_input, admitted_at) in input::load(connection)? {
        if let Some(key) = &stored_input.request_key {
            state.request_keys.insert(key.clone(), stored_input.id);
        }
        if let Some(commit_seq) = admitted_at {
            state.input_commits.insert(stored_input.id, commit_seq);
        }
        state
            .inputs
            .insert(stored_input.id, std::sync::Arc::new(stored_input));
    }
    for stored_task in task::load(connection)? {
        state
            .tasks
            .insert(stored_task.id, std::sync::Arc::new(stored_task));
    }

    Ok(state)
}
