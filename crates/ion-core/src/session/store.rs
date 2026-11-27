//! The Session's durable SQLite store: prepare a candidate, commit, then publish it.
//! The Store holds derived in-memory projections beside the authoritative journal.

use std::{path::PathBuf, time::Duration};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::{
    HistoryProjection, SessionEntry, SessionError, artifacts,
    journal::{self, CommittedAppend, JournalIdentity, PreparedAppend, WorkingJournal},
    projection::ProjectionMode,
};
#[cfg(any(unix, test))]
use super::{journal::encode_entry, projection::project};

pub(super) const FORMAT_VERSION: u32 = 19;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Header {
    pub(super) version: u32,
    pub(super) id: uuid::Uuid,
    pub(super) provider_session_id: uuid::Uuid,
    pub(super) cwd: Option<PathBuf>,
    #[serde(default)]
    pub(super) name: Option<String>,
}

impl Header {
    pub(super) fn decode(encoded: &[u8]) -> Result<Self, SessionError> {
        // Reject obsolete formats before interpreting the current payload.
        // Never invent an affinity identity when opening old data.
        #[derive(Deserialize)]
        struct Format {
            version: u32,
        }
        let format: Format = serde_json::from_slice(encoded)?;
        if format.version != FORMAT_VERSION {
            return Err(SessionError::UnsupportedFormat(format.version));
        }
        Ok(serde_json::from_slice(encoded)?)
    }

    pub(super) fn identity(&self) -> JournalIdentity {
        JournalIdentity {
            session: self.id,
            provider_session: self.provider_session_id,
        }
    }
}

/// A checked journal prefix, ready to install in a newly acquired SQLite connection.
/// File creation, leases and WAL policy belong to the connection owner.
#[cfg(any(unix, test))]
pub(super) struct StoreSeed<'a> {
    entries: &'a [SessionEntry],
    projection: HistoryProjection,
    encoded: Vec<Vec<u8>>,
}

#[cfg(any(unix, test))]
impl<'a> StoreSeed<'a> {
    pub fn prepare(entries: &'a [SessionEntry]) -> Result<Self, SessionError> {
        let projection = project(entries.iter().map(Ok), ProjectionMode::Working, &|entry| {
            Ok(entries[(entry - 1) as usize].clone())
        })?;
        let encoded = entries.iter().map(encode_entry).collect::<Result<_, _>>()?;
        Ok(Self {
            entries,
            projection,
            encoded,
        })
    }

    pub fn initialize(
        self,
        mut connection: Connection,
        header: &Header,
        install_artifacts: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<(), SessionError>,
    ) -> Result<Store, SessionError> {
        write_transaction(&mut connection, |tx| {
            initialize_schema(tx)?;
            install_artifacts(tx)?;
            for reference in self.entries.iter().flat_map(artifacts::references) {
                artifacts::validate(tx, reference)?;
            }
            tx.execute(
                "INSERT INTO session(id, header) VALUES (1, ?1)",
                params![serde_json::to_vec(header)?],
            )?;
            for body in self.encoded {
                tx.execute("INSERT INTO entries(body) VALUES (?1)", params![body])?;
            }
            Ok(())
        })?;
        Ok(Store::from_projection(
            connection,
            header.identity(),
            self.projection,
        ))
    }
}

pub(super) struct Store {
    pub(super) connection: Connection,
    faulted: bool,
    pub(super) working: WorkingJournal,
}

impl Store {
    /// Rebuild from one pinned read of an already acquired, configured connection.
    /// This neither acquires ownership nor repairs unfinished work.
    #[cfg(any(unix, test))]
    pub(super) fn reopen(connection: Connection) -> Result<(Self, Header), SessionError> {
        let (header, projection) = {
            let checkpoint = ReadCheckpoint::pin(&connection)?;
            let projection = checkpoint.project(ProjectionMode::Working)?;
            (checkpoint.header, projection)
        };
        Ok((
            Self::from_projection(connection, header.identity(), projection),
            header,
        ))
    }

    #[cfg(any(unix, test))]
    fn from_projection(
        connection: Connection,
        identity: JournalIdentity,
        projection: HistoryProjection,
    ) -> Self {
        Self {
            connection,
            faulted: false,
            working: WorkingJournal::open(identity, projection),
        }
    }

    pub(super) fn ensure_healthy(&self) -> Result<(), SessionError> {
        if self.faulted {
            Err(SessionError::StoreFaulted)
        } else {
            Ok(())
        }
    }

    /// Once a transaction begins, a failed statement is known uncommitted only
    /// after an explicitly successful rollback. COMMIT errors remain uncertain.
    pub(super) fn transaction<T>(
        &mut self,
        write: impl FnOnce(&rusqlite::Transaction<'_>, Header) -> Result<T, SessionError>,
    ) -> Result<T, SessionError> {
        let identity = self.working.identity();
        self.transaction_with_header(|tx, header| {
            identity.verify(header.identity())?;
            write(tx, header)
        })
    }

    fn transaction_with_header<T>(
        &mut self,
        write: impl FnOnce(&rusqlite::Transaction<'_>, Header) -> Result<T, SessionError>,
    ) -> Result<T, SessionError> {
        self.ensure_healthy()?;
        // Acquire write authority before sampling identity or tail. Upgrading a
        // deferred read transaction could fail immediately on a busy writer.
        let result = write_transaction_with(
            &mut self.connection,
            rusqlite::TransactionBehavior::Immediate,
            |tx| write(tx, read_header(tx)?),
        );
        if result.as_ref().is_err_and(|error| {
            error.is_commit_uncertain()
                || matches!(
                    error,
                    SessionError::JournalIdentityChanged
                        | SessionError::JournalFrontierChanged { .. }
                )
        }) {
            // A definite mismatch means no write occurred, but this owner can
            // no longer vouch for the projection from which it would prepare one.
            self.faulted = true;
        }
        result
    }

    pub(super) fn append(&mut self, entries: &[SessionEntry]) -> Result<(), SessionError> {
        self.ensure_healthy()?;
        let prepared = self.working.prepare(
            entries,
            |entry| read_entry(&self.connection, entry),
            |reference| artifacts::validate(&self.connection, reference),
        )?;
        let committed = self.commit_append(prepared)?;
        self.working.publish(committed);
        Ok(())
    }

    fn commit_append<'a>(
        &mut self,
        prepared: PreparedAppend<'a>,
    ) -> Result<CommittedAppend<'a>, SessionError> {
        let staged = self.transaction_with_header(|tx, header| {
            let tail = tx.query_row("SELECT MAX(seq) FROM entries", [], |row| row.get(0))?;
            let mut insert = tx.prepare("INSERT INTO entries(seq, body) VALUES (?1, ?2)")?;
            prepared.stage(header.identity(), tail, |sequence, body| {
                insert.execute(params![sequence, body])?;
                Ok(())
            })
        })?;
        Ok(staged.acknowledge())
    }
}

#[cfg(any(unix, test))]
fn write_transaction<T>(
    connection: &mut Connection,
    write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, SessionError>,
) -> Result<T, SessionError> {
    write_transaction_with(connection, rusqlite::TransactionBehavior::Deferred, write)
}

fn write_transaction_with<T>(
    connection: &mut Connection,
    behavior: rusqlite::TransactionBehavior,
    write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, SessionError>,
) -> Result<T, SessionError> {
    let tx = connection.transaction_with_behavior(behavior)?;
    match write(&tx) {
        Ok(value) => {
            tx.commit().map_err(|error| SessionError::CommitUncertain {
                error: Box::new(error.into()),
                rollback_error: None,
            })?;
            Ok(value)
        }
        Err(error) => match tx.rollback() {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(SessionError::CommitUncertain {
                error: Box::new(error),
                rollback_error: Some(rollback_error),
            }),
        },
    }
}

/// The SQLite read transaction owns the cut from the header SELECT through
/// replay and any point reads needed to materialize the working context.
pub(super) struct ReadCheckpoint<'a> {
    snapshot: rusqlite::Transaction<'a>,
    pub header: Header,
}

impl<'a> ReadCheckpoint<'a> {
    pub fn pin(connection: &'a Connection) -> Result<Self, SessionError> {
        let snapshot = connection.unchecked_transaction()?;
        let header = read_header(&snapshot)?;
        Ok(Self { snapshot, header })
    }

    pub fn project(&self, mode: ProjectionMode) -> Result<HistoryProjection, SessionError> {
        let mut statement = self.snapshot.prepare(SCAN_ENTRIES)?;
        journal::replay(SqliteRows(statement.query([])?), mode, |entry| {
            read_entry(&self.snapshot, entry)
        })
    }

    pub fn read_entries(&self) -> Result<Vec<SessionEntry>, SessionError> {
        read_entries(&self.snapshot)
    }
}

const SCAN_ENTRIES: &str = "SELECT seq, body FROM entries ORDER BY seq";

struct SqliteRows<'a>(rusqlite::Rows<'a>);

impl journal::EntryRows for SqliteRows<'_> {
    fn next_record(&mut self) -> Result<Option<journal::EncodedRecord<'_>>, SessionError> {
        self.0.next()?.map(encoded_record).transpose()
    }
}

pub(super) fn read_entries(connection: &Connection) -> Result<Vec<SessionEntry>, SessionError> {
    let mut statement = connection.prepare(SCAN_ENTRIES)?;
    journal::decode_rows(SqliteRows(statement.query([])?)).collect()
}

pub(super) fn shell_settlement(
    connection: &Connection,
    admission: u64,
    upper: u64,
) -> Result<Option<(u64, usize)>, SessionError> {
    let row: Option<(i64,i64)> = connection.query_row("SELECT seq,length(body) FROM entries WHERE json_extract(body,'$.kind')='user_shell_settled' AND json_extract(body,'$.data.admission_entry')=?1 AND seq<=?2", params![i64::try_from(admission).map_err(|_| SessionError::InvalidHistory)?, i64::try_from(upper).map_err(|_| SessionError::InvalidHistory)?], |row| Ok((row.get(0)?,row.get(1)?))).optional()?;
    row.map(|(coordinate, bytes)| {
        Ok((
            u64::try_from(coordinate).map_err(|_| SessionError::InvalidHistory)?,
            usize::try_from(bytes).map_err(|_| SessionError::InvalidHistory)?,
        ))
    })
    .transpose()
}

pub(super) fn read_entry(
    connection: &Connection,
    entry: u64,
) -> Result<SessionEntry, SessionError> {
    let mut statement =
        connection.prepare_cached("SELECT seq, body FROM entries WHERE seq = ?1")?;
    let mut rows = statement.query(params![
        i64::try_from(entry).map_err(|_| SessionError::InvalidContextPoint)?
    ])?;
    let row = rows.next()?.ok_or(SessionError::InvalidHistory)?;
    encoded_record(row)?.decode(entry)
}

fn encoded_record<'row>(
    row: &'row rusqlite::Row<'_>,
) -> Result<journal::EncodedRecord<'row>, SessionError> {
    Ok(journal::EncodedRecord {
        sequence: row.get(0)?,
        body: row
            .get_ref(1)?
            .as_blob()
            .map_err(|_| SessionError::InvalidHistory)?,
    })
}

pub(super) fn read_header(connection: &Connection) -> Result<Header, SessionError> {
    let encoded: Vec<u8> = connection
        .query_row("SELECT header FROM session WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()?
        .ok_or(SessionError::InvalidDatabase)?;
    Header::decode(&encoded)
}

pub(super) fn configure_connection(connection: &Connection) -> Result<(), SessionError> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

#[cfg(any(unix, test))]
pub(super) fn initialize_schema(connection: &Connection) -> Result<(), SessionError> {
    connection.execute_batch("CREATE TABLE session (id INTEGER PRIMARY KEY CHECK(id=1), header BLOB NOT NULL); CREATE TABLE entries (seq INTEGER PRIMARY KEY, body BLOB NOT NULL);
CREATE UNIQUE INDEX request_keys ON entries(json_extract(body, '$.data.request.scope'), json_extract(body, '$.data.request.id')) WHERE json_extract(body, '$.kind') = 'turn_started' AND json_extract(body, '$.data.request') IS NOT NULL;
CREATE UNIQUE INDEX delegation_runs ON entries(json_extract(body, '$.data.record.run')) WHERE json_extract(body, '$.kind') = 'delegation_admitted';
CREATE UNIQUE INDEX delegation_created_children ON entries(json_extract(body, '$.data.record.child')) WHERE json_extract(body, '$.kind') = 'delegation_admitted' AND json_extract(body, '$.data.record.spec.target.type') IN ('fresh', 'fork');
CREATE UNIQUE INDEX child_bindings ON entries(json_extract(body, '$.data.record.run')) WHERE json_extract(body, '$.kind') = 'child_session_of';
CREATE INDEX turn_ends ON entries(json_extract(body, '$.data.turn')) WHERE json_extract(body, '$.kind') = 'turn_ended';
CREATE UNIQUE INDEX shell_results ON entries(json_extract(body,'$.data.admission_entry')) WHERE json_extract(body,'$.kind')='user_shell_settled';")?;
    super::artifacts::initialize(connection)?;
    Ok(())
}
