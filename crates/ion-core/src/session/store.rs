//! The Session's durable SQLite store: prepare a candidate, commit, then publish it.
//! The Store holds derived in-memory projections beside the authoritative journal.

use super::*;

pub(super) const FORMAT_VERSION: u32 = 19;
// An 8 MiB raw prompt or streamed response can grow up to sixfold when JSON
// escapes control characters. Keep the storage bound above that encoded size.
pub(crate) const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Header {
    pub(super) version: u32,
    pub(super) id: uuid::Uuid,
    pub(super) provider_session_id: uuid::Uuid,
    pub(super) cwd: Option<PathBuf>,
    #[serde(default)]
    pub(super) name: Option<String>,
}

pub(super) struct Store {
    pub(super) connection: Connection,
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

pub(super) struct SessionLock(pub(super) File);

pub(super) struct CreatedDatabase {
    path: PathBuf,
    identity: (u64, u64),
    keep: bool,
}
impl CreatedDatabase {
    pub(super) fn publish(&mut self) {
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

pub(super) fn create_database_file(path: &Path) -> Result<CreatedDatabase, SessionError> {
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
    pub(super) fn from_projection(connection: Connection, projection: HistoryProjection) -> Self {
        let mut store = Self {
            connection,
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
        let mut replacement = None;
        let mut candidate = self.state.clone();
        // A shared pending shell owns the final context message. Seed only that
        // slot so candidate settlement can replace it without rebuilding history.
        let pending_message = self
            .state
            .pending_shell
            .as_ref()
            .filter(|shell| !shell.exclude_from_context)
            .map(|shell| {
                ProjectedMessage::new(
                    shell.admission_entry,
                    shell_message(
                        &shell.command,
                        &UserShellOutcome::Unknown,
                        false,
                        shell.admission_entry,
                    ),
                )
            });
        let replace_pending_message = pending_message.is_some();
        let mut new_messages: Vec<_> = pending_message.into_iter().collect();
        let message_start = self.messages.len() - usize::from(replace_pending_message);
        let mut new_context_changes = Vec::new();
        let mut new_settled = Vec::new();
        let encoded = entries
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                for reference in artifacts::references(entry) {
                    artifacts::validate(&self.connection, reference)?;
                }
                let transition = candidate.apply(entry, &|coordinate| {
                    self.settled.contains(&coordinate) || new_settled.contains(&coordinate)
                })?;
                if transition.settled {
                    new_settled.push(candidate.sequence);
                }
                projection::append_message(
                    &mut new_messages,
                    candidate.sequence,
                    entry,
                    transition.closed_shell.as_ref(),
                );
                if let SessionEntry::ContextSelected { head_entry } = entry {
                    if replacement.is_some() || offset + 1 != entries.len() {
                        return Err(SessionError::InvalidHistory);
                    }
                    if !self.context_index.can_select(*head_entry) {
                        return Err(SessionError::InvalidContextPoint);
                    }
                    candidate.restore_context(
                        self.context_index.read_context(*head_entry, &|entry| {
                            read_entry(&self.connection, entry)
                        })?,
                    );
                    let context_index::WorkingContext {
                        messages,
                        settled,
                        changes,
                    } = working_context(&self.context_index, *head_entry, &candidate, &|entry| {
                        read_entry(&self.connection, entry)
                    })?;
                    replacement = Some((messages, settled, changes));
                }
                if let SessionEntry::ModelContextChanged { context, .. } = entry
                    && candidate.compaction.is_none()
                    && candidate.replay_epoch_start == 0
                {
                    new_context_changes.push(ModelContextChange {
                        after_message: message_start + new_messages.len(),
                        context: model_context_state(context),
                    });
                }
                encode_entry(entry)
            })
            .collect::<Result<Vec<_>, SessionError>>()?;
        let tx = self.connection.transaction()?;
        for body in encoded {
            tx.execute("INSERT INTO entries(body) VALUES (?1)", params![body])?;
        }
        tx.commit()?;
        let mut sequence = self.state.sequence;
        for entry in entries {
            sequence += 1;
            self.child_records.observe(sequence, entry);
            self.context_index.observe(entry);
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
        if let Some((messages, settled, changes)) = replacement {
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
}

pub(super) fn encode_entry(entry: &SessionEntry) -> Result<Vec<u8>, SessionError> {
    let bytes = serde_json::to_vec(entry)?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(SessionError::EntryTooLarge);
    }
    Ok(bytes)
}

pub(super) fn project_connection(
    connection: &Connection,
    mode: ProjectionMode,
) -> Result<HistoryProjection, SessionError> {
    // Validate one fact at a time without an owned raw-history export. The
    // projection mode determines which derived values must remain resident.
    let mut statement = connection.prepare("SELECT seq, body FROM entries ORDER BY seq")?;
    let mut rows = statement.query([])?;
    let mut sequence = 0;
    let entries = std::iter::from_fn(|| match rows.next() {
        Ok(Some(row)) => {
            sequence += 1;
            Some(decode_entry(row, sequence))
        }
        Ok(None) => None,
        Err(error) => Some(Err(error.into())),
    });
    project(entries, mode, &|entry| read_entry(connection, entry))
}

pub(super) fn read_entries(connection: &Connection) -> Result<Vec<SessionEntry>, SessionError> {
    let mut statement = connection.prepare("SELECT seq, body FROM entries ORDER BY seq")?;
    let mut rows = statement.query([])?;
    let mut entries = Vec::new();
    while let Some(row) = rows.next()? {
        entries.push(decode_entry(row, entries.len() as u64 + 1)?);
    }
    Ok(entries)
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
    decode_entry(row, entry)
}

fn decode_entry(row: &rusqlite::Row<'_>, expected: u64) -> Result<SessionEntry, SessionError> {
    // Coordinates identify effects, context branches and history reads. Never
    // substitute iteration order for a damaged physical key.
    let sequence: i64 = row.get(0)?;
    if u64::try_from(sequence).ok() != Some(expected) {
        return Err(SessionError::InvalidHistory);
    }
    let encoded = row.get_ref(1)?;
    let encoded = encoded
        .as_blob()
        .map_err(|_| SessionError::InvalidHistory)?;
    if encoded.len() > MAX_ENTRY_BYTES {
        return Err(SessionError::EntryTooLarge);
    }
    Ok(serde_json::from_slice(encoded)?)
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

pub(super) fn prepare_writer(connection: &Connection) -> Result<(), SessionError> {
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

pub(super) fn lock(path: &Path) -> Result<SessionLock, SessionError> {
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
