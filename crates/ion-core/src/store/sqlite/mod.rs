//! SQLite ownership and schema for one Ion Session.

mod connection;
mod ownership;
pub(crate) mod schema;

use std::path::Path;

use rusqlite::Connection;

use super::StoreError;
use crate::SessionId;

pub(crate) struct SqliteStore {
    pub(crate) session_id: SessionId,
    _connection: Connection,
    _ownership: ownership::Ownership,
}

impl SqliteStore {
    pub(crate) fn create(path: &Path, session_id: SessionId) -> Result<Self, StoreError> {
        if path.exists() {
            return Err(StoreError::AlreadyExists(path.to_path_buf()));
        }
        let ownership = ownership::Ownership::acquire(path)?;
        let connection = connection::create(path)?;
        schema::initialize(&connection, session_id)?;
        Ok(Self {
            session_id,
            _connection: connection,
            _ownership: ownership,
        })
    }

    pub(crate) fn open(path: &Path) -> Result<Self, StoreError> {
        if !path.exists() {
            return Err(StoreError::Unknown(path.to_path_buf()));
        }
        let ownership = ownership::Ownership::acquire(path)?;
        let connection = connection::open(path)?;
        schema::verify(&connection)?;
        let session_id = schema::read_session_id(&connection)?;
        Ok(Self {
            session_id,
            connection,
            _ownership: ownership,
        })
    }
}
