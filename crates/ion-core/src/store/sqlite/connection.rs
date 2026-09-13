//! Open policy, pragmas and session identity for the per-session database.
//!
//! Durability floor: WAL journalling with `synchronous = FULL`. A committed
//! transaction is therefore durable across process death and OS failure to the
//! extent the filesystem honours `fsync`; `NORMAL` would trade that for fewer
//! syncs and is deliberately not used.

use std::path::Path;

use rusqlite::{Connection, OpenFlags};

use super::StoreError;
use crate::SessionId;

fn configure(connection: &Connection) -> Result<(), StoreError> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "busy_timeout", 5_000)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

/// Create a new database file. Fails if the file already holds a schema.
pub(crate) fn create(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )?;
    configure(&connection)?;
    Ok(connection)
}

/// Open an existing database file. Never creates one.
pub(crate) fn open(path: &Path) -> Result<Connection, StoreError> {
    if !path.exists() {
        return Err(StoreError(format!(
            "session database {} does not exist",
            path.display()
        )));
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    configure(&connection)?;
    Ok(connection)
}

/// Record the owning session identity on a freshly created database.
pub(crate) fn record_session_id(
    connection: &Connection,
    session_id: SessionId,
) -> Result<(), StoreError> {
    let inserted = connection.execute(
        "INSERT INTO session_meta (id, session_id) VALUES (1, ?1)",
        [session_id.as_uuid().to_string()],
    )?;
    if inserted != 1 {
        return Err(StoreError("session metadata was not created".to_owned()));
    }
    Ok(())
}

pub(crate) fn read_session_id(connection: &Connection) -> Result<SessionId, StoreError> {
    let raw: String = connection.query_row(
        "SELECT session_id FROM session_meta WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    raw.parse()
        .map_err(|error| StoreError(format!("invalid session id in database: {error}")))
}

/// Metadata read during reconstruction.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Meta {
    pub(crate) last_seq: Option<i64>,
    pub(crate) last_commit: Option<i64>,
    pub(crate) root_conversation: Option<i64>,
}

pub(crate) fn read_meta(connection: &Connection) -> Result<Meta, StoreError> {
    let meta = connection.query_row(
        "SELECT last_seq, last_commit, root_conversation FROM session_meta WHERE id = 1",
        [],
        |row| {
            Ok(Meta {
                last_seq: row.get(0)?,
                last_commit: row.get(1)?,
                root_conversation: row.get(2)?,
            })
        },
    )?;
    Ok(meta)
}
