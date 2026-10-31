//! The Session's durable SQLite store: prepare a candidate, commit, then publish it.
//! The Store holds derived in-memory projections beside the authoritative journal.

use super::*;

pub(super) const FORMAT_VERSION: u32 = 18;
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

impl Drop for SessionLock {
    fn drop(&mut self) {
        // A child may briefly inherit a duplicate file description. Release
        // this writer's lease explicitly before closing our descriptor.
        let _ = flock(&self.0, FlockOperation::Unlock);
    }
}

impl Store {
    pub(super) fn from_projection(connection: Connection, projection: HistoryProjection) -> Self {
        Self {
            connection,
            state: projection.state,
            settled: projection.settled,
            messages: projection.messages,
            context_changes: projection.context_changes,
            child_records: projection.child_records,
            context_index: projection.context_index,
            calibration: None,
            live_backgrounds: BTreeMap::new(),
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
                    shell_message(&shell.command, &UserShellOutcome::Unknown, false),
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
                candidate.apply(entry, &mut new_messages, &self.settled, &mut new_settled)?;
                if let SessionEntry::ContextSelected { head_entry } = entry {
                    if replacement.is_some() || offset + 1 != entries.len() {
                        return Err(SessionError::InvalidHistory);
                    }
                    let navigation::RestoredContext {
                        state,
                        messages,
                        settled,
                        changes,
                    } = restore_context(
                        &self.context_index,
                        *head_entry,
                        ProjectionMode::Full,
                        &|entry| navigation::read_entry(&self.connection, entry),
                    )?;
                    candidate.restore_context(state);
                    replacement = Some((messages, settled, changes));
                }
                if let SessionEntry::ModelContextChanged { context, .. } = entry {
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
    let mut statement = connection.prepare("SELECT body FROM entries ORDER BY seq")?;
    let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    project(
        rows.map(|body| Ok(serde_json::from_slice::<SessionEntry>(&body?)?)),
        mode,
        &|entry| navigation::read_entry(connection, entry),
    )
}

pub(super) fn read_entries(connection: &Connection) -> Result<Vec<SessionEntry>, SessionError> {
    let mut statement = connection.prepare("SELECT body FROM entries ORDER BY seq")?;
    let rows = statement.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
    let mut entries = Vec::new();
    for body in rows {
        entries.push(serde_json::from_slice(&body?)?);
    }
    Ok(entries)
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
CREATE INDEX turn_ends ON entries(json_extract(body, '$.data.turn')) WHERE json_extract(body, '$.kind') = 'turn_ended';")?;
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
