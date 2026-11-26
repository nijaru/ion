//! The Session's durable SQLite store: prepare a candidate, commit, then publish it.
//! The Store holds derived in-memory projections beside the authoritative journal.

use super::journal::encode_entry;
use super::*;

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

/// Owns both the physical database and the exclusive writer lease. Field
/// order closes SQLite before releasing the lease, including during retirement.
pub(super) struct NativeStore {
    pub store: Mutex<Store>,
    _lock: SessionLock,
}

#[cfg(test)]
impl NativeStore {
    pub fn duplicate_lease_descriptor(&self) -> std::io::Result<File> {
        self._lock.0.try_clone()
    }
}

pub(super) struct OpenedStore {
    pub native: NativeStore,
    pub header: Header,
    pub path: PathBuf,
}

impl OpenedStore {
    pub fn create(
        path: &Path,
        cwd: Option<&Path>,
        id: uuid::Uuid,
        entries: &[SessionEntry],
        artifact_source: Option<(&Path, uuid::Uuid)>,
    ) -> Result<Self, SessionError> {
        let projection = project(entries.iter().map(Ok), ProjectionMode::Working, &|entry| {
            Ok(entries[(entry - 1) as usize].clone())
        })?;
        let encoded = entries
            .iter()
            .map(encode_entry)
            .collect::<Result<Vec<_>, _>>()?;
        let cwd = cwd.map(Path::canonicalize).transpose()?;
        if cwd.as_ref().is_some_and(|cwd| !cwd.is_dir()) {
            return Err(SessionError::InvalidWorkingDirectory);
        }
        let filename = path.file_name().ok_or(SessionError::InvalidPath)?;
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let path = parent.canonicalize()?.join(filename);
        // Hold the canonical lease through creation and failed-initialization
        // cleanup; another opener must never own a database we are removing.
        let lock = lock(&path)?;
        let mut creation = create_database_file(&path)?;
        let path = database_path(&path)?;
        let mut connection = Connection::open(&path)?;
        initialize_new(&connection)?;
        let header = Header {
            version: FORMAT_VERSION,
            // Random prefixes remain useful as catalog selectors and short UI labels.
            id,
            provider_session_id: uuid::Uuid::now_v7(),
            cwd,
            name: None,
        };
        write_transaction(&mut connection, |tx| {
            if let Some((path, id)) = artifact_source {
                let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                configure_connection(&source)?;
                let snapshot = source.unchecked_transaction()?;
                if read_header(&snapshot)?.provider_session_id != id {
                    return Err(SessionError::InvalidDatabase);
                }
                artifacts::copy(&snapshot, tx, entries)?;
            }
            for reference in entries.iter().flat_map(artifacts::references) {
                artifacts::validate(tx, reference)?;
            }
            tx.execute(
                "INSERT INTO session(id, header) VALUES (1, ?1)",
                params![serde_json::to_vec(&header)?],
            )?;
            for body in encoded {
                tx.execute("INSERT INTO entries(body) VALUES (?1)", params![body])?;
            }
            Ok(())
        })?;
        creation.publish();
        Ok(Self {
            native: NativeStore {
                store: Mutex::new(Store::from_projection(connection, projection)),
                _lock: lock,
            },
            header,
            path,
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let path = database_path(path.as_ref())?;
        let lock = lock(&path)?;
        // A disappearing resume target must fail, not create a new empty file
        // between path validation and SQLite opening it.
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::default().difference(OpenFlags::SQLITE_OPEN_CREATE),
        )?;
        configure_connection(&connection)?;
        let (header, projection) = {
            let checkpoint = ReadCheckpoint::pin(&connection)?;
            let projection = checkpoint.project(ProjectionMode::Working)?;
            (checkpoint.header, projection)
        };
        prepare_writer(&connection)?;
        Ok(Self {
            native: NativeStore {
                store: Mutex::new(Store::from_projection(connection, projection)),
                _lock: lock,
            },
            header,
            path,
        })
    }
}

pub(super) struct Store {
    pub(super) connection: Connection,
    faulted: bool,
    pub(super) state: State,
    pub(super) settled: BTreeSet<u64>,
    pub(super) messages: Vec<ProjectedMessage>,
    // Sparse, rebuildable projection of committed context boundaries. It does
    // not retain or reproject historical tool payloads on each model request.
    pub(super) context_changes: Vec<ModelContextChange>,
    pub(super) child_records: ChildRecordIndex,
    pub(super) context_index: ContextIndex,
    // Process-local capacity evidence. Reopen, fork and clone start cold; this
    // is never serialized into Session history.
    pub(super) calibration: Option<RequestCalibration>,
    // Process-local task custody only; absent after reopen. Not replay grants.
    pub(super) live_backgrounds: BTreeMap<uuid::Uuid, std::sync::Weak<()>>,
}

struct SessionLock(File);

struct CreatedDatabase {
    path: PathBuf,
    identity: (u64, u64),
    keep: bool,
}
impl CreatedDatabase {
    fn publish(&mut self) {
        self.keep = true;
    }
}
impl Drop for CreatedDatabase {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && (metadata.dev(), metadata.ino()) == self.identity
        {
            let _ = fs::remove_file(&self.path);
            for suffix in ["-wal", "-shm"] {
                let mut path = self.path.as_os_str().to_os_string();
                path.push(suffix);
                let _ = fs::remove_file(PathBuf::from(path));
            }
        }
        // The lock file's stable inode belongs to future attempts as well.
    }
}

fn create_database_file(path: &Path) -> Result<CreatedDatabase, SessionError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            let _ = fs::remove_file(path);
            return Err(error.into());
        }
    };
    Ok(CreatedDatabase {
        path: path.to_path_buf(),
        identity: (metadata.dev(), metadata.ino()),
        keep: false,
    })
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        // A child may briefly inherit a duplicate file description. Release
        // this writer's lease explicitly before closing our descriptor.
        let _ = flock(&self.0, FlockOperation::Unlock);
    }
}

impl Store {
    fn from_projection(connection: Connection, projection: HistoryProjection) -> Self {
        let mut store = Self {
            connection,
            faulted: false,
            state: projection.state,
            settled: projection.settled,
            messages: projection.messages,
            context_changes: projection.context_changes,
            child_records: projection.child_records,
            context_index: projection.context_index,
            calibration: None,
            live_backgrounds: BTreeMap::new(),
        };
        store.prune_context();
        store
    }

    pub(super) fn ensure_healthy(&self) -> Result<(), SessionError> {
        if self.faulted {
            Err(SessionError::StoreFaulted)
        } else {
            Ok(())
        }
    }

    /// Once a transaction begins, a failed statement is known uncommitted only
    /// after an explicitly successful rollback. COMMIT errors remain uncertain
    /// even if SQLite's drop cleanup
    /// succeeds: neither rollback nor visible bytes prove durable acceptance.
    pub(super) fn transaction<T>(
        &mut self,
        write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, SessionError>,
    ) -> Result<T, SessionError> {
        self.transaction_with(rusqlite::TransactionBehavior::Deferred, write)
    }

    fn transaction_with<T>(
        &mut self,
        behavior: rusqlite::TransactionBehavior,
        write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, SessionError>,
    ) -> Result<T, SessionError> {
        self.ensure_healthy()?;
        let result = write_transaction_with(&mut self.connection, behavior, write);
        if result
            .as_ref()
            .is_err_and(|error| error.is_commit_uncertain())
        {
            self.faulted = true;
        }
        result
    }

    fn prune_context(&mut self) {
        if let Some((through, _)) = &self.state.compaction {
            self.messages.retain(|record| {
                record.entry > *through
                    || self
                        .state
                        .compaction_inputs
                        .binary_search(&record.entry)
                        .is_ok()
                    || self
                        .state
                        .active_inputs
                        .binary_search(&record.entry)
                        .is_ok()
            });
            self.settled.retain(|entry| entry > through);
        }
        if self.state.compaction.is_some() || self.state.replay_epoch_start != 0 {
            self.context_changes.clear();
        }
    }

    pub(super) fn orphaned_backgrounds(&self, session: uuid::Uuid) -> Vec<SessionEntry> {
        self.state
            .backgrounds
            .iter()
            .filter(|(run, link)| {
                link.site.session == session
                    && self
                        .live_backgrounds
                        .get(run)
                        .is_none_or(|lease| lease.strong_count() == 0)
            })
            .map(|(run, _)| SessionEntry::DelegationSettled {
                run: *run,
                outcome: crate::DelegationOutcome::Unknown,
            })
            .collect()
    }

    pub(super) fn append(&mut self, entries: &[SessionEntry]) -> Result<(), SessionError> {
        self.ensure_healthy()?;
        let journal::PreparedAppend {
            expected_tail,
            encoded,
            candidate,
            resolved_tools,
            replacement,
            replace_pending_message,
            new_messages,
            new_settled,
            new_context_changes,
        } = journal::AppendBasis {
            state: &self.state,
            context_index: &self.context_index,
            settled: &self.settled,
            message_count: self.messages.len(),
        }
        .prepare(
            entries,
            |entry| read_entry(&self.connection, entry),
            |reference| artifacts::validate(&self.connection, reference),
        )?;
        self.commit_append(expected_tail, encoded)?;
        let mut sequence = self.state.sequence;
        for (entry, resolved) in entries.iter().zip(resolved_tools) {
            sequence += 1;
            self.child_records.observe(sequence, entry);
            self.context_index.observe(entry);
            self.context_index.link_display(entry, resolved);
            if let SessionEntry::DelegationSettled { run, .. } = entry {
                self.live_backgrounds.remove(run);
            }
        }
        if entries.iter().any(|entry| {
            matches!(
                entry,
                SessionEntry::ContextSelected { .. }
                    | SessionEntry::Compacted { .. }
                    | SessionEntry::ProviderReplayRebased { .. }
                    | SessionEntry::EffectiveModelChanged { .. }
                    | SessionEntry::ModelSelected { .. }
                    | SessionEntry::ReasoningSelected { .. }
                    | SessionEntry::ModelContextChanged { .. }
            )
        }) {
            self.calibration = None;
        }
        self.state = candidate;
        if let Some(context_index::WorkingContext {
            messages,
            settled,
            changes,
        }) = replacement
        {
            self.messages = messages;
            self.settled = settled;
            self.context_changes = changes;
        } else {
            self.settled.extend(new_settled);
            if replace_pending_message {
                self.messages.pop();
            }
            self.messages.extend(new_messages);
            self.context_changes.extend(new_context_changes);
        }
        self.prune_context();
        Ok(())
    }

    fn commit_append(
        &mut self,
        expected_tail: u64,
        encoded: Vec<Vec<u8>>,
    ) -> Result<(), SessionError> {
        let expected = if expected_tail == 0 {
            None
        } else {
            Some(i64::try_from(expected_tail).map_err(|_| SessionError::InvalidHistory)?)
        };
        // Acquire the write slot before sampling the frontier. Upgrading a
        // deferred read transaction could fail immediately on a busy writer.
        let result = self.transaction_with(rusqlite::TransactionBehavior::Immediate, |tx| {
            let actual = tx.query_row("SELECT MAX(seq) FROM entries", [], |row| {
                row.get::<_, Option<i64>>(0)
            })?;
            if actual != expected {
                return Err(SessionError::JournalFrontierChanged {
                    expected_tail,
                    actual,
                });
            }
            let mut insert = tx.prepare("INSERT INTO entries(seq, body) VALUES (?1, ?2)")?;
            let mut sequence = expected.unwrap_or(0);
            for body in encoded {
                sequence = sequence
                    .checked_add(1)
                    .ok_or(SessionError::InvalidHistory)?;
                insert.execute(params![sequence, body])?;
            }
            Ok(())
        });
        if matches!(result, Err(SessionError::JournalFrontierChanged { .. })) {
            // This process no longer owns the working projection of the journal.
            self.faulted = true;
        }
        result
    }
}

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

/// The native read transaction owns the cut from the header SELECT through
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
    use rusqlite::OptionalExtension;
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
    // Reject obsolete formats before interpreting the current header payload.
    // In particular, never invent an affinity identity when opening old data.
    #[derive(Deserialize)]
    struct Format {
        version: u32,
    }
    let format: Format = serde_json::from_slice(&encoded)?;
    if format.version != FORMAT_VERSION {
        return Err(SessionError::UnsupportedFormat(format.version));
    }
    Ok(serde_json::from_slice(&encoded)?)
}

pub(super) fn configure_connection(connection: &Connection) -> Result<(), SessionError> {
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

fn prepare_writer(connection: &Connection) -> Result<(), SessionError> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

pub(super) fn initialize_new(connection: &Connection) -> Result<(), SessionError> {
    configure_connection(connection)?;
    prepare_writer(connection)?;
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

pub(super) fn database_path(path: &Path) -> Result<PathBuf, SessionError> {
    let path = path.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SessionError::NotFound
        } else {
            SessionError::Io(error)
        }
    })?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(SessionError::NotFound);
    }
    // SQLite resolves symlinks, but cannot unify hard-linked journal/WAL names.
    // Reject those aliases rather than claiming lock normalization makes them safe.
    // https://www.sqlite.org/howtocorrupt.html#multiple_links_to_the_same_file
    if metadata.nlink() != 1 {
        return Err(SessionError::HardLinkedDatabase);
    }
    Ok(path)
}

fn lock(path: &Path) -> Result<SessionLock, SessionError> {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(".lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(PathBuf::from(sidecar))?;
    flock(&file, FlockOperation::NonBlockingLockExclusive).map_err(|error| {
        if error == rustix::io::Errno::WOULDBLOCK {
            SessionError::AlreadyOpen
        } else {
            SessionError::Io(error.into())
        }
    })?;
    Ok(SessionLock(file))
}
