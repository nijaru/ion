//! Open policy, pragmas and session identity.
//!
//! Durability floor: WAL journalling with `synchronous = FULL`. A committed
//! transaction is therefore durable across process death and OS failure to the
//! extent the filesystem honours `fsync`; `NORMAL` would trade that for fewer
//! syncs and is deliberately not used.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use super::StoreError;
use crate::error::Error;
use crate::id::IdError;
use crate::store::SessionInfo;
use crate::{CommitSeq, ConversationId, SessionId};

fn configure(connection: &Connection) -> Result<(), StoreError> {
    // The journal mode is a persistent property of the database file, and
    // asking to change it takes an exclusive lock. Read it first and only ask
    // when it differs, so opening a database another connection is using stays
    // a read instead of a lock fight.
    let mode: String = connection.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        connection.pragma_update(None, "journal_mode", "WAL")?;
    }
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

/// Open an existing database file. Never creates one; the caller checks
/// existence before taking ownership so a missing database leaves no lock file.
pub(crate) fn open(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    configure(&connection)?;
    Ok(connection)
}

/// Session metadata as stored on the single `session_meta` row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Meta {
    pub(crate) session_id: SessionId,
    pub(crate) last_commit: Option<i64>,
    pub(crate) root: Option<i64>,
}

pub(crate) fn read_meta(connection: &Connection) -> Result<Meta, StoreError> {
    let row = connection
        .query_row(
            "SELECT session_id, last_commit, root_conversation \
             FROM session_meta WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((session_id, last_commit, root)) = row else {
        return Err(StoreError::Rejected(Error::Corrupt(
            "session metadata is missing; the database was never initialized".to_owned(),
        )));
    };
    let session_id = session_id.parse::<SessionId>().map_err(|error| {
        StoreError::Rejected(Error::Corrupt(format!("invalid session id: {error}")))
    })?;
    Ok(Meta {
        session_id,
        last_commit,
        root,
    })
}

pub(crate) fn read_info(connection: &Connection) -> Result<SessionInfo, StoreError> {
    let meta = read_meta(connection)?;
    let root = meta
        .root
        .ok_or_else(|| {
            StoreError::Rejected(Error::Corrupt(
                "session has no root conversation".to_owned(),
            ))
        })
        .and_then(|raw| {
            ConversationId::try_from(raw).map_err(|error: IdError| {
                StoreError::Rejected(Error::Corrupt(format!(
                    "invalid root conversation: {error}"
                )))
            })
        })?;
    let last_commit = meta
        .last_commit
        .map(|raw| {
            CommitSeq::try_from(raw).map_err(|error: IdError| {
                StoreError::Rejected(Error::Corrupt(format!("invalid commit cursor: {error}")))
            })
        })
        .transpose()?;
    Ok(SessionInfo {
        session_id: meta.session_id,
        root,
        last_commit,
    })
}
