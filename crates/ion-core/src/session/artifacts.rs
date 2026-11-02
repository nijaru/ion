//! Session-owned binary captures. Chunk commits never span external work.
//! Unfinished/unclaimed captures are retained: absence of an observation is not
//! permission to delete evidence of an uncertain effect.
use std::{
    io::{self, Write},
    sync::Arc,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Session, SessionError};

const CHUNK_BYTES: usize = 256 * 1024;
pub(crate) const PAGE_BYTES: usize = 16 * 1024;

pub(super) fn initialize(connection: &Connection) -> rusqlite::Result<()> {
    connection.execute_batch(&format!(
        "CREATE TABLE artifacts (id TEXT PRIMARY KEY, label TEXT NOT NULL, source BLOB NOT NULL, receipt BLOB);
         CREATE TABLE artifact_chunks (artifact TEXT NOT NULL REFERENCES artifacts(id), byte_offset INTEGER NOT NULL CHECK(byte_offset>=0), body BLOB NOT NULL CHECK(length(body) BETWEEN 1 AND {CHUNK_BYTES}), checksum TEXT NOT NULL, PRIMARY KEY(artifact, byte_offset));"
    ))
}

/// A receipt for retained bytes, not authorization to read them. Readers must
/// establish the source observation and its visibility independently.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub id: uuid::Uuid,
    pub label: String,
    pub bytes: u64,
    pub sha256: String,
    pub complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactPage {
    pub offset: u64,
    pub bytes: Vec<u8>,
    pub next_offset: Option<u64>,
}

/// Narrow capture capability. Keeping it keeps the Session writer lease alive.
#[derive(Clone)]
pub struct ArtifactSink(Arc<Session>, Option<CaptureOrigin>);

#[derive(Clone, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
enum CaptureOrigin {
    Tool(crate::ToolOrigin),
    UserShell { admission_entry: u64 },
}

pub struct ArtifactWriter {
    sink: ArtifactSink,
    id: uuid::Uuid,
    label: String,
    pending: Vec<u8>,
    hash: Sha256,
    bytes: u64,
    committed: u64,
    failed: bool,
    io: Arc<crate::operation::OperationOwner>,
}

impl Session {
    pub fn artifact_sink(self: &Arc<Self>) -> ArtifactSink {
        ArtifactSink(self.clone(), None)
    }

    /// Operator-level bounded inspection. Model tools must first authorize a
    /// committed source coordinate; the capture sink deliberately has no reader.
    pub fn read_artifact(
        &self,
        reference: &ArtifactRef,
        offset: u64,
        limit: usize,
    ) -> Result<ArtifactPage, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        read_page(&store.connection, reference, offset, limit)
    }
}

fn io_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn chunk_checksum(id: uuid::Uuid, offset: u64, bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(id.as_bytes());
    hash.update(offset.to_be_bytes());
    hash.update(bytes);
    format!("{:x}", hash.finalize())
}

impl ArtifactSink {
    pub(crate) fn for_tool(mut self, origin: crate::ToolOrigin) -> Self {
        self.1 = Some(CaptureOrigin::Tool(origin));
        self
    }

    pub(super) fn for_shell(mut self, admission_entry: u64) -> Self {
        self.1 = Some(CaptureOrigin::UserShell { admission_entry });
        self
    }

    fn check_origin(&self) -> io::Result<()> {
        if matches!(&self.1, Some(CaptureOrigin::Tool(origin)) if origin.session() != self.0.id()) {
            return Err(io::Error::other(
                "artifact sink belongs to a different Session",
            ));
        }
        Ok(())
    }

    pub fn start(&self, label: impl Into<String>) -> io::Result<ArtifactWriter> {
        self.check_origin()?;
        let owner = self.0.storage.register().map_err(io_error)?;
        Ok(ArtifactWriter {
            sink: self.clone(),
            id: uuid::Uuid::new_v4(),
            label: label.into(),
            pending: Vec::with_capacity(CHUNK_BYTES),
            hash: Sha256::new(),
            bytes: 0,
            committed: 0,
            failed: false,
            io: owner,
        })
    }

    /// For an already-owned blocking worker. Async producers use write_bytes
    /// and finish_async instead; neither API detaches fallible storage work.
    pub fn capture(&self, label: &str, bytes: &[u8]) -> io::Result<ArtifactRef> {
        let mut writer = self.start(label)?;
        writer.write_all(bytes)?;
        writer.finish(true)
    }

    fn append(&self, id: uuid::Uuid, label: &str, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let mut store = self.0.store.lock().map_err(io_error)?;
        let tx = store.connection.transaction().map_err(io_error)?;
        tx.execute(
            "INSERT OR IGNORE INTO artifacts(id,label,source) VALUES (?1,?2,?3)",
            params![
                id.to_string(),
                label,
                serde_json::to_vec(&self.1).map_err(io_error)?
            ],
        )
        .map_err(io_error)?;
        let unfinished: bool = tx
            .query_row(
                "SELECT receipt IS NULL FROM artifacts WHERE id=?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .map_err(io_error)?;
        if !unfinished {
            return Err(io::Error::other("artifact is already finalized"));
        }
        tx.execute(
            "INSERT INTO artifact_chunks(artifact, byte_offset, body, checksum) VALUES (?1,?2,?3,?4)",
            params![
                id.to_string(),
                i64::try_from(offset).map_err(io_error)?,
                bytes,
                chunk_checksum(id, offset, bytes)
            ],
        )
        .map_err(io_error)?;
        tx.commit().map_err(io_error)
    }
}

impl ArtifactWriter {
    fn check(&self) -> io::Result<()> {
        self.sink.check_origin()?;
        if self.failed {
            Err(io::Error::other(
                "artifact capture failed or its write wait was cancelled",
            ))
        } else {
            Ok(())
        }
    }

    fn record(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::other("artifact length overflow"))?;
        self.hash.update(bytes);
        self.pending.extend_from_slice(bytes);
        Ok(())
    }

    /// Buffered async ingestion. Cancellation poisons an in-flight flush so a
    /// later finish cannot publish a success-shaped receipt for uncertain bytes.
    pub async fn write_bytes(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        self.check()?;
        while !bytes.is_empty() {
            let n = bytes.len().min(CHUNK_BYTES - self.pending.len());
            self.record(&bytes[..n])?;
            bytes = &bytes[n..];
            if self.pending.len() == CHUNK_BYTES {
                self.failed = true;
                let data = std::mem::replace(&mut self.pending, Vec::with_capacity(CHUNK_BYTES));
                let sink = self.sink.clone();
                let id = self.id;
                let offset = self.committed;
                let written = data.len() as u64;
                let label = self.label.clone();
                self.sink
                    .0
                    .storage
                    .run(&self.io, move || {
                        sink.append(id, &label, offset, &data)
                            .map_err(SessionError::from)
                    })
                    .await
                    .map_err(io_error)?;
                self.committed += written;
                self.failed = false;
            }
        }
        Ok(())
    }

    pub fn finish(self, complete: bool) -> io::Result<ArtifactRef> {
        let owner = self.io.clone();
        let _idle = self.sink.0.storage.reserve_sync(&owner).map_err(io_error)?;
        self.finish_storage(complete)
    }

    fn finish_storage(mut self, complete: bool) -> io::Result<ArtifactRef> {
        self.flush_storage()?;
        let reference = ArtifactRef {
            id: self.id,
            label: self.label,
            bytes: self.bytes,
            sha256: format!("{:x}", self.hash.finalize()),
            complete,
        };
        let store = self.sink.0.store.lock().map_err(io_error)?;
        let changed = store.connection.execute("INSERT INTO artifacts(id, receipt,label,source) VALUES (?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET receipt=excluded.receipt WHERE artifacts.receipt IS NULL AND artifacts.label=excluded.label AND artifacts.source=excluded.source",
            params![reference.id.to_string(), serde_json::to_vec(&reference).map_err(io_error)?, reference.label, serde_json::to_vec(&self.sink.1).map_err(io_error)?]).map_err(io_error)?;
        if changed != 1 {
            return Err(io::Error::other("artifact is already finalized"));
        }
        Ok(reference)
    }

    pub async fn finish_async(self, complete: bool) -> io::Result<ArtifactRef> {
        let owner = self.io.clone();
        let session = self.sink.0.clone();
        session
            .storage
            .run(&owner, move || {
                self.finish_storage(complete).map_err(SessionError::from)
            })
            .await
            .map_err(io_error)
    }

    fn flush_storage(&mut self) -> io::Result<()> {
        self.check()?;
        if !self.pending.is_empty() {
            self.failed = true;
            self.sink
                .append(self.id, &self.label, self.committed, &self.pending)?;
            self.committed += self.pending.len() as u64;
            self.pending.clear();
            self.failed = false;
        }
        Ok(())
    }
}

impl Write for ArtifactWriter {
    fn write(&mut self, mut bytes: &[u8]) -> io::Result<usize> {
        self.check()?;
        let len = bytes.len();
        while !bytes.is_empty() {
            let n = bytes.len().min(CHUNK_BYTES - self.pending.len());
            self.record(&bytes[..n])?;
            bytes = &bytes[n..];
            if self.pending.len() == CHUNK_BYTES {
                self.flush()?;
            }
        }
        Ok(len)
    }
    fn flush(&mut self) -> io::Result<()> {
        let owner = self.io.clone();
        let _idle = self.sink.0.storage.reserve_sync(&owner).map_err(io_error)?;
        self.flush_storage()
    }
}

/// Resolve only typed observations. Opaque JSON fields named artifacts are
/// never evidence custody, and private shell admissions cannot grant reads.
pub(super) fn history_reference(
    connection: &Connection,
    entry: u64,
    upper: u64,
    index: usize,
) -> Result<ArtifactRef, SessionError> {
    let (kind, private): (String, Option<bool>) = connection.query_row("SELECT json_extract(body,'$.kind'), json_extract(body,'$.data.exclude_from_context') FROM entries WHERE seq=?1", [i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?], |row| Ok((row.get(0)?,row.get(1)?)))?;
    let (coordinate, path) = match (kind.as_str(), private) {
        ("tool_result", _) => (entry, "$.data.result.outcome.output.artifacts"),
        ("user_shell_admitted", Some(false)) => {
            let (coordinate, _) = super::store::shell_settlement(connection, entry, upper)?
                .ok_or(SessionError::InvalidContextPoint)?;
            (coordinate, "$.data.outcome.output.artifacts")
        }
        _ => return Err(SessionError::InvalidContextPoint),
    };
    observation_reference(connection, coordinate, path, index)?
        .ok_or(SessionError::InvalidContextPoint)
}

pub(super) fn observation_reference(
    connection: &Connection,
    entry: u64,
    path: &str,
    index: usize,
) -> Result<Option<ArtifactRef>, SessionError> {
    let encoded: Option<String> = connection.query_row(
        "SELECT CAST(body AS TEXT) -> ?2 FROM entries WHERE seq=?1",
        params![
            i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?,
            format!("{path}[{index}]")
        ],
        |row| row.get(0),
    )?;
    encoded
        .map(|encoded| serde_json::from_str(&encoded).map_err(SessionError::from))
        .transpose()
}

pub(super) fn observation_count(
    connection: &Connection,
    entry: u64,
    path: &str,
) -> Result<usize, SessionError> {
    let count: i64 = connection.query_row(
        "SELECT coalesce(json_array_length(CAST(body AS TEXT),?2),0) FROM entries WHERE seq=?1",
        params![
            i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?,
            path
        ],
        |row| row.get(0),
    )?;
    usize::try_from(count).map_err(|_| SessionError::InvalidHistory)
}

pub(super) fn references(entry: &super::SessionEntry) -> &[ArtifactRef] {
    use super::{SessionEntry, UserShellOutcome};
    match entry {
        SessionEntry::ToolResult {
            result:
                crate::ToolSettlement {
                    outcome: crate::ToolOutcome::Observed { output, .. },
                    ..
                },
            ..
        }
        | SessionEntry::ChildToolResult {
            outcome: crate::ChildOutcome::Observed { output },
            ..
        }
        | SessionEntry::UserShellSettled {
            outcome: UserShellOutcome::Observed { output },
            ..
        } => &output.artifacts,
        _ => &[],
    }
}

pub(super) fn copy(
    source: &Connection,
    target: &Connection,
    entries: &[super::SessionEntry],
) -> Result<(), SessionError> {
    let mut copied = std::collections::BTreeSet::new();
    for reference in entries.iter().flat_map(references) {
        if !copied.insert(reference.id) {
            continue;
        }
        validate(source, reference)?;
        let (label, origin): (String, Vec<u8>) = source.query_row(
            "SELECT label,source FROM artifacts WHERE id=?1",
            [reference.id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        target.execute(
            "INSERT INTO artifacts(id,receipt,label,source) VALUES (?1,?2,?3,?4)",
            params![
                reference.id.to_string(),
                serde_json::to_vec(reference)?,
                label,
                origin
            ],
        )?;
        let mut statement = source.prepare(
            "SELECT byte_offset,body,checksum FROM artifact_chunks WHERE artifact=?1 ORDER BY byte_offset",
        )?;
        let mut rows = statement.query([reference.id.to_string()])?;
        let mut bytes = 0u64;
        let mut digest = Sha256::new();
        while let Some(row) = rows.next()? {
            let body: Vec<u8> = row.get(1)?;
            let hash: String = row.get(2)?;
            if u64::try_from(row.get::<_, i64>(0)?).map_err(|_| SessionError::InvalidHistory)?
                != bytes
                || body.is_empty()
                || body.len() > CHUNK_BYTES
                || chunk_checksum(reference.id, bytes, &body) != hash
            {
                return Err(SessionError::InvalidHistory);
            }
            digest.update(&body);
            target.execute(
                "INSERT INTO artifact_chunks(artifact,byte_offset,body,checksum) VALUES (?1,?2,?3,?4)",
                params![reference.id.to_string(), i64::try_from(bytes).map_err(|_| SessionError::InvalidHistory)?, &body, hash],
            )?;
            bytes += body.len() as u64;
        }
        if bytes != reference.bytes || format!("{:x}", digest.finalize()) != reference.sha256 {
            return Err(SessionError::InvalidHistory);
        }
    }
    Ok(())
}

pub(super) fn validate(
    connection: &Connection,
    reference: &ArtifactRef,
) -> Result<(), SessionError> {
    if reference.id.is_nil()
        || reference.sha256.len() != 64
        || !reference
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SessionError::InvalidHistory);
    }
    let receipt: Option<Option<Vec<u8>>> = connection
        .query_row(
            "SELECT receipt FROM artifacts WHERE id=?1",
            [reference.id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let encoded = receipt.flatten().ok_or(SessionError::InvalidHistory)?;
    if serde_json::from_slice::<ArtifactRef>(&encoded)? != *reference {
        return Err(SessionError::InvalidHistory);
    }
    Ok(())
}

/// The caller has already authorized this exact source observation. Page checks
/// covered chunks, not the entire artifact per page. SHA receipts detect capture
/// and copy defects; SQLite plus hashes are not hostile-host tamper protection.
fn read_page(
    connection: &Connection,
    reference: &ArtifactRef,
    offset: u64,
    limit: usize,
) -> Result<ArtifactPage, SessionError> {
    validate(connection, reference)?;
    if offset > reference.bytes || !(1..=PAGE_BYTES).contains(&limit) {
        return Err(SessionError::InvalidArtifactRange);
    }
    let end = offset.saturating_add(limit as u64).min(reference.bytes);
    let mut data = Vec::with_capacity(limit);
    let mut cursor = offset;
    if end > offset {
        let mut statement = connection.prepare("SELECT byte_offset,body,checksum FROM artifact_chunks WHERE artifact=?1 AND byte_offset >= (SELECT max(byte_offset) FROM artifact_chunks WHERE artifact=?1 AND byte_offset<=?2) AND byte_offset<?3 ORDER BY byte_offset")?;
        let mut rows = statement.query(params![
            reference.id.to_string(),
            i64::try_from(offset).map_err(|_| SessionError::InvalidHistory)?,
            i64::try_from(end).map_err(|_| SessionError::InvalidHistory)?
        ])?;
        while let Some(row) = rows.next()? {
            let start =
                u64::try_from(row.get::<_, i64>(0)?).map_err(|_| SessionError::InvalidHistory)?;
            let body: Vec<u8> = row.get(1)?;
            let hash: String = row.get(2)?;
            let chunk_end = start
                .checked_add(body.len() as u64)
                .ok_or(SessionError::InvalidHistory)?;
            if body.is_empty()
                || body.len() > CHUNK_BYTES
                || (!data.is_empty() && start != cursor)
                || start > cursor
                || chunk_end <= cursor
                || chunk_end > reference.bytes
                || chunk_checksum(reference.id, start, &body) != hash
            {
                return Err(SessionError::InvalidHistory);
            }
            let next = end.min(chunk_end);
            data.extend_from_slice(&body[(cursor - start) as usize..(next - start) as usize]);
            cursor = next;
        }
    }
    if cursor != end {
        return Err(SessionError::InvalidHistory);
    }
    Ok(ArtifactPage {
        offset,
        bytes: data,
        next_offset: (end < reference.bytes).then_some(end),
    })
}

pub(super) fn page(
    connection: &Connection,
    reference: &ArtifactRef,
    offset: u64,
    limit: usize,
) -> Result<serde_json::Value, SessionError> {
    let page = read_page(connection, reference, offset, limit)?;
    // Defer only an incomplete character at a text page boundary, never malformed
    // bytes at EOF. Tiny, binary or unaligned requests keep the exact raw range.
    let text = match std::str::from_utf8(&page.bytes) {
        Ok(text) => Some(text),
        Err(error)
            if error.error_len().is_none()
                && error.valid_up_to() > 0
                && page.next_offset.is_some() =>
        {
            Some(
                std::str::from_utf8(&page.bytes[..error.valid_up_to()])
                    .expect("valid_up_to bounds a valid UTF-8 prefix"),
            )
        }
        Err(_) => None,
    }
    .filter(|text| {
        text.chars()
            .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
    });
    let (encoding, data, data_bytes) = match text {
        Some(text) => ("utf8", serde_json::json!(text), text.len()),
        None => (
            "base64",
            serde_json::json!(STANDARD.encode(&page.bytes)),
            page.bytes.len(),
        ),
    };
    let end = offset + data_bytes as u64;
    let end_label = reference.label.floor_char_boundary(128);
    Ok(serde_json::json!({
        "artifact_id": reference.id,
        "label": &reference.label[..end_label],
        "label_truncated": end_label < reference.label.len(),
        "total_bytes": reference.bytes,
        "sha256": reference.sha256,
        "complete": reference.complete,
        "offset": offset,
        "encoding": encoding,
        "data": data,
        "data_bytes": data_bytes,
        "next_offset": (end < reference.bytes).then_some(end),
    }))
}
