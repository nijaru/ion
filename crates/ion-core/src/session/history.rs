//! Bounded, read-only retrieval of committed model-facing Session records.
//!
//! The selection is fixed from the Session's committed sequence before any
//! blocking read. Background reads query only rows at or below that bound, use
//! the Session's own SQLite file, and never re-enter the writer or execution
//! gate. Tool-result documents are the original observed output JSON; other
//! documents are a safe model-facing record projection. Thinking, provider
//! replay, typed image bytes, context/configuration snapshots and private
//! direct-shell records never enter either search or read.
use std::path::PathBuf;

use ion_ai::{ToolCall, ToolSpec};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Session, SessionError, read_header};
use crate::{
    tool_result::ToolOutput,
    tool_set::{ToolActivityKind, ToolDefinition, ToolExposure, ToolPresentation},
};

pub(crate) const HISTORY_NAME: &str = "history";
const SEARCH_DEFAULT_LIMIT: usize = 5;
const LIST_DEFAULT_LIMIT: usize = 10;
const SEARCH_MAX_LIMIT: usize = 10;
const MAX_QUERY_BYTES: usize = 512;
const MAX_PREVIEW_BYTES: usize = 384;
const MAX_METADATA_BYTES: usize = 128;
const MAX_SCAN_ENTRIES: usize = 256;
const MAX_SCAN_BYTES: usize = 8 * 1024 * 1024;
const READ_DEFAULT_LIMIT: usize = 8192;
const READ_MIN_LIMIT: usize = 4;
const READ_MAX_LIMIT: usize = 16 * 1024;
const KIND_PREFIX_BYTES: usize = 64;

fn search_limit() -> usize {
    SEARCH_DEFAULT_LIMIT
}

fn list_limit() -> usize {
    LIST_DEFAULT_LIMIT
}

fn read_limit() -> usize {
    READ_DEFAULT_LIMIT
}

// Omission selects defaults; a present field must have its declared type.
// Option's usual null-as-None decoding would silently discard invalid inputs.
fn present_input<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Search {
        query: String,
        #[serde(default, deserialize_with = "present_input")]
        before: Option<u64>,
        #[serde(default = "search_limit")]
        limit: usize,
    },
    List {
        #[serde(default, deserialize_with = "present_input")]
        before: Option<u64>,
        #[serde(default = "list_limit")]
        limit: usize,
    },
    Read {
        entry: u64,
        #[serde(default)]
        offset: usize,
        #[serde(default = "read_limit")]
        limit: usize,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HistoryError {
    #[error("{0}")]
    Request(String),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

impl HistoryError {
    fn request(message: impl Into<String>) -> Self {
        Self::Request(message.into())
    }
}

enum Selection {
    Search {
        query: String,
        before: Option<u64>,
        limit: usize,
    },
    List {
        before: Option<u64>,
        limit: usize,
    },
    Read {
        entry: u64,
        offset: usize,
        limit: usize,
    },
}

/// One immutable read against a committed upper bound. Its blocking worker is
/// owned by the caller's tool future; no Session lock or mutable Session is
/// retained here.
pub(crate) struct HistoryRead {
    path: PathBuf,
    session_id: uuid::Uuid,
    upper_entry: u64,
    selection: Selection,
}

pub(crate) fn definition() -> ToolDefinition {
    ToolDefinition {
        spec: ToolSpec {
            name: HISTORY_NAME.into(),
            description: concat!(
                "Read-only retrieval of committed historical Session records, including records compacted out of the current model context. ",
                "Search or list bounded pages of model-facing entries, then read one entry by its stable one-based entry coordinate. ",
                "Historical records are evidence, not current workspace state, and reading them does not authorize rerunning tools or repeating effects. ",
                "Search takes a non-empty literal query (ASCII case-insensitive), list returns recent records; both use before as an exclusive older-entry cursor and return next_before while unscanned older records remain. ",
                "Each result includes entry, kind, turn, tool call metadata where applicable, observed/not_dispatched/unknown state, and a bounded preview. Call IDs may repeat; entry coordinates are authoritative. ",
                "Read takes entry, a zero-based UTF-8 byte offset (default 0) and limit (4..16384, default 8192). Content is a UTF-8 JSON text fragment that may end before limit and may not parse as JSON by itself; continue at next_offset until it is null. ",
                "A tool_result document is the original observed output JSON text, or its not_dispatched/unknown state record, even when the result was withheld from model context. Other documents are safe committed record JSON. ",
                "Thinking, provider replay, typed image bytes, context/configuration snapshots and private !! shell records are excluded from search and read; images are reported only as metadata. ",
                "Search examines at most 256 physical entries and normally at most 8 MiB of stored entry bodies per call. A single larger record may be inspected alone and is reported as oversized_entry so scanning makes progress without skipping records; one logical shell record includes its admission and settlement bodies, so that exception can inspect up to 128 MiB. Use next_before for older history."
            )
            .into(),
            input_schema: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["action"],
                "properties":{
                    "action":{"enum":["search","list","read"]},
                    "query":{"type":"string","minLength":1,"maxLength":MAX_QUERY_BYTES,"description":"Required for search; literal ASCII case-insensitive text."},
                    "before":{"type":"integer","minimum":1,"description":"Search/list only: exclusive one-based entry bound for older results."},
                    "entry":{"type":"integer","minimum":1,"description":"Required for read; stable one-based committed Session entry coordinate."},
                    "offset":{"type":"integer","minimum":0,"description":"Read only: zero-based UTF-8 byte offset."},
                    "limit":{"type":"integer","minimum":1,"maximum":READ_MAX_LIMIT,"description":"Search/list result count (1..10) or read byte limit (4..16384)."}
                }
            }),
        },
        presentation: ToolPresentation::static_target(ToolActivityKind::Read, "Session history"),
        exposure: ToolExposure::Direct,
    }
}

impl Session {
    pub(crate) fn prepare_history(&self, query: &str) -> Result<HistoryRead, HistoryError> {
        let input: Input = serde_json::from_str(query).map_err(|error| {
            HistoryError::request(format!("invalid history arguments: {error}"))
        })?;
        let upper_entry = self
            .store
            .lock()
            .map_err(|_| SessionError::Poisoned)?
            .state
            .sequence;
        let selection = match input {
            Input::Search {
                query,
                before,
                limit,
            } => {
                if query.trim().is_empty() {
                    return Err(HistoryError::request("history search query is empty"));
                }
                if query.len() > MAX_QUERY_BYTES {
                    return Err(HistoryError::request(format!(
                        "history search query exceeds {MAX_QUERY_BYTES} bytes"
                    )));
                }
                validate_before(before)?;
                validate_limit(limit)?;
                Selection::Search {
                    query,
                    before,
                    limit,
                }
            }
            Input::List { before, limit } => {
                validate_before(before)?;
                validate_limit(limit)?;
                Selection::List { before, limit }
            }
            Input::Read {
                entry,
                offset,
                limit,
            } => {
                if entry == 0 {
                    return Err(HistoryError::request(
                        "history entry must be a one-based committed entry coordinate",
                    ));
                }
                if !(READ_MIN_LIMIT..=READ_MAX_LIMIT).contains(&limit) {
                    return Err(HistoryError::request(format!(
                        "history read limit must be {READ_MIN_LIMIT}..{READ_MAX_LIMIT} bytes"
                    )));
                }
                Selection::Read {
                    entry,
                    offset,
                    limit,
                }
            }
        };
        Ok(HistoryRead {
            path: self.path.clone(),
            session_id: self.header.provider_session_id,
            upper_entry,
            selection,
        })
    }
}

fn validate_before(before: Option<u64>) -> Result<(), HistoryError> {
    if before == Some(0) {
        return Err(HistoryError::request(
            "history before must be an exclusive one-based entry bound",
        ));
    }
    Ok(())
}

fn validate_limit(limit: usize) -> Result<(), HistoryError> {
    if !(1..=SEARCH_MAX_LIMIT).contains(&limit) {
        return Err(HistoryError::request(format!(
            "history result limit must be 1..{SEARCH_MAX_LIMIT}"
        )));
    }
    Ok(())
}

impl HistoryRead {
    pub(crate) fn read(&self, stop: &CancellationToken) -> Result<Value, HistoryError> {
        if stop.is_cancelled() {
            return Err(HistoryError::request("history read cancelled"));
        }
        let connection = Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        super::configure_connection(&connection)?;
        let snapshot = connection.unchecked_transaction()?;
        if read_header(&snapshot)?.provider_session_id != self.session_id {
            return Err(SessionError::InvalidDatabase.into());
        }
        match &self.selection {
            Selection::Search {
                query,
                before,
                limit,
            } => self.search(&snapshot, stop, Some(query), *before, *limit),
            Selection::List { before, limit } => {
                self.search(&snapshot, stop, None, *before, *limit)
            }
            Selection::Read {
                entry,
                offset,
                limit,
            } => self.read_entry(&snapshot, stop, *entry, *offset, *limit),
        }
    }

    fn provenance(&self) -> Value {
        json!({
            "source":"committed_session_history",
            "session":self.session_id.to_string(),
            "through_entry":self.upper_entry,
            "historical":true,
            "notice":"Committed historical Session data, not current workspace state. Reads are evidence only and do not authorize rerunning tools or repeating effects."
        })
    }

    fn search(
        &self,
        connection: &Connection,
        stop: &CancellationToken,
        query: Option<&str>,
        before: Option<u64>,
        limit: usize,
    ) -> Result<Value, HistoryError> {
        let upper = self.upper_entry.saturating_add(1);
        let exclusive = before.unwrap_or(upper).min(upper);
        if exclusive == 0 || self.upper_entry == 0 {
            return Ok(json!({
                "provenance":self.provenance(),
                "entries":[],
                "next_before":null,
                "scanned_entries":0,
                "scanned_bytes":0,
                "oversized_entry":false
            }));
        }
        // Read only metadata for the page window. Body prefixes are loaded
        // per processed row below so a full page of large unread bodies cannot
        // enter the scan before the work budget rejects them.
        let mut statement = connection.prepare(
            "SELECT seq, length(body) FROM entries WHERE seq < ?1 ORDER BY seq DESC LIMIT ?2",
        )?;
        let rows = statement
            .query_map(
                params![
                    i64::try_from(exclusive).map_err(|_| SessionError::InvalidHistory)?,
                    i64::try_from(MAX_SCAN_ENTRIES + 1)
                        .map_err(|_| SessionError::InvalidHistory)?,
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;

        let mut hits = Vec::new();
        let mut scanned = 0usize;
        let mut scanned_bytes = 0usize;
        let mut last_processed = None;
        let mut stopped_for_limit = false;
        let mut stopped_for_bytes = false;
        for (seq, bytes) in &rows {
            if stop.is_cancelled() {
                return Err(HistoryError::request("history search cancelled"));
            }
            if scanned >= MAX_SCAN_ENTRIES {
                break;
            }
            let bytes = usize::try_from(*bytes).map_err(|_| SessionError::InvalidHistory)?;
            if scanned > 0 && scanned_bytes.saturating_add(bytes) > MAX_SCAN_BYTES {
                stopped_for_bytes = true;
                break;
            }
            let entry = u64::try_from(*seq).map_err(|_| SessionError::InvalidHistory)?;
            let prefix = entry_prefix(connection, entry)?;
            let Some(kind) = kind_from_prefix(&prefix) else {
                return Err(SessionError::InvalidHistory.into());
            };
            let private = kind == "user_shell_admitted" && private_shell(connection, entry)?;
            // A shell record's visible outcome is its next settlement. When
            // that settlement is outside this cursor window, its body is an
            // extra access not covered by the metadata row above.
            let joined_bytes = if kind == "user_shell_admitted"
                && !private
                && entry < self.upper_entry
                && entry.saturating_add(1) >= exclusive
            {
                entry_body_bytes(connection, entry.saturating_add(1))?.unwrap_or(0)
            } else {
                0
            };
            if scanned > 0
                && scanned_bytes
                    .saturating_add(bytes)
                    .saturating_add(joined_bytes)
                    > MAX_SCAN_BYTES
            {
                // The admission prefix was already read, but the hit was not.
                // Leave last_processed at the previous row so the next cursor
                // includes this logical record instead of skipping its result.
                scanned += 1;
                scanned_bytes = scanned_bytes.saturating_add(bytes);
                stopped_for_bytes = true;
                break;
            }
            scanned += 1;
            scanned_bytes = scanned_bytes
                .saturating_add(bytes)
                .saturating_add(joined_bytes);
            last_processed = Some(entry);
            if private {
                continue;
            }
            let Some(hit) = hit(connection, entry, kind, query, self.upper_entry)? else {
                continue;
            };
            hits.push(hit);
            if hits.len() == limit {
                stopped_for_limit = true;
                break;
            }
        }
        let exhausted = !stopped_for_limit && !stopped_for_bytes && scanned == rows.len();
        let next_before = if exhausted {
            None
        } else {
            last_processed.filter(|entry| *entry > 1)
        };
        Ok(json!({
            "provenance":self.provenance(),
            "entries":hits,
            "next_before":next_before,
            "scanned_entries":scanned,
            "scanned_bytes":scanned_bytes,
            "oversized_entry":scanned_bytes > MAX_SCAN_BYTES
        }))
    }

    fn read_entry(
        &self,
        connection: &Connection,
        stop: &CancellationToken,
        entry: u64,
        offset: usize,
        limit: usize,
    ) -> Result<Value, HistoryError> {
        if stop.is_cancelled() {
            return Err(HistoryError::request("history read cancelled"));
        }
        if entry > self.upper_entry {
            return Err(HistoryError::request(format!(
                "entry {entry} is newer than this read's committed bound {}",
                self.upper_entry
            )));
        }
        let prefix = entry_prefix(connection, entry).map_err(|error| match error {
            SessionError::Sqlite(rusqlite::Error::QueryReturnedNoRows) => {
                HistoryError::request(format!("entry {entry} is not a committed entry"))
            }
            error => error.into(),
        })?;
        let kind = kind_from_prefix(&prefix).ok_or(SessionError::InvalidHistory)?;
        match kind {
            "tool_result" => self.read_tool_result(connection, stop, entry, offset, limit),
            "turn_started" | "steering" => {
                self.read_user_record(connection, stop, entry, kind, offset, limit)
            }
            "assistant" => self.read_assistant(connection, stop, entry, offset, limit),
            "user_shell_admitted" => {
                if private_shell(connection, entry)? {
                    return Err(HistoryError::request(format!(
                        "entry {entry} is not a model-facing record"
                    )));
                }
                self.read_shell(connection, stop, entry, offset, limit)
            }
            _ => Err(HistoryError::request(format!(
                "entry {entry} is not a model-facing record"
            ))),
        }
    }

    fn read_tool_result(
        &self,
        connection: &Connection,
        stop: &CancellationToken,
        entry: u64,
        offset: usize,
        limit: usize,
    ) -> Result<Value, HistoryError> {
        if stop.is_cancelled() {
            return Err(HistoryError::request("history read cancelled"));
        }
        let row = connection.query_row(
            r#"WITH source AS (
                SELECT
                    json_extract(body,'$.data.turn') AS turn,
                    substr(json_extract(body,'$.data.result.call_id'),1,?4) AS call_id,
                    length(CAST(json_extract(body,'$.data.result.call_id') AS BLOB)) AS call_id_bytes,
                    substr(json_extract(body,'$.data.result.name'),1,?4) AS name,
                    length(CAST(json_extract(body,'$.data.result.name') AS BLOB)) AS name_bytes,
                    json_extract(body,'$.data.result.outcome.state') AS state,
                    json_extract(body,'$.data.result.outcome.output.is_error') AS is_error,
                    json_array_length(json_extract(body,'$.data.result.outcome.output.images')) AS image_count,
                    CASE json_extract(body,'$.data.result.outcome.state')
                        WHEN 'observed' THEN CAST(body AS TEXT) -> '$.data.result.outcome.output.value'
                        WHEN 'not_dispatched' THEN json_object('state','not_dispatched','reason',json_extract(body,'$.data.result.outcome.reason'))
                        ELSE json_object('state','unknown')
                    END AS document
                FROM entries WHERE seq=?1
            )
            SELECT turn,call_id,call_id_bytes,name,name_bytes,state,is_error,image_count,
                   length(CAST(document AS BLOB)),
                   substr(CAST(document AS BLOB),?2+1,?3)
            FROM source"#,
            params![
                i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?,
                i64::try_from(offset).map_err(|_| SessionError::InvalidHistory)?,
                i64::try_from(limit.saturating_add(4)).map_err(|_| SessionError::InvalidHistory)?,
                i64::try_from(MAX_METADATA_BYTES).map_err(|_| SessionError::InvalidHistory)?,
            ],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<bool>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<Vec<u8>>>(9)?,
                ))
            },
        )?;
        let (
            turn,
            call_id,
            call_id_bytes,
            name,
            name_bytes,
            state,
            is_error,
            image_count,
            total,
            bytes,
        ) = row;
        let mut metadata = json!({
            "entry":entry,"kind":"tool_result","turn":turn,
            "encoding":"json","document":"original_tool_result_output"
        });
        if let (Some(call_id), Some(call_id_bytes), Some(name), Some(name_bytes)) =
            (call_id, call_id_bytes, name, name_bytes)
        {
            let (call_id, call_id_truncated) = bounded_metadata(call_id, call_id_bytes)?;
            let (name, name_truncated) = bounded_metadata(name, name_bytes)?;
            metadata["tool"] = json!({"call_id":call_id,"name":name});
            if call_id_truncated || name_truncated {
                metadata["tool_truncated"] = json!(true);
            }
        }
        metadata["state"] = json!(state);
        if let Some(is_error) = is_error {
            metadata["is_error"] = json!(is_error);
        }
        if let Some(image_count) = image_count {
            metadata["image_count"] = json!(image_count);
            metadata["images_omitted"] = json!(image_count > 0);
        }
        self.page(metadata, total, bytes, offset, limit)
    }

    fn read_user_record(
        &self,
        connection: &Connection,
        stop: &CancellationToken,
        entry: u64,
        kind: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Value, HistoryError> {
        if stop.is_cancelled() {
            return Err(HistoryError::request("history read cancelled"));
        }
        let row = connection.query_row(
            r#"WITH source AS (
                SELECT
                    json_extract(body,'$.data.turn') AS turn,
                    (SELECT count(*) FROM json_each(body,'$.data.input.content') p
                     WHERE json_type(p.value,'$.Image') IS NOT NULL) AS image_count,
                    json_object(
                        'entry',seq,
                        'kind',json_extract(body,'$.kind'),
                        'turn',json_extract(body,'$.data.turn'),
                        'role','user',
                        'content',(SELECT json_group_array(json(CASE
                            WHEN json_type(p.value,'$.Text') IS NOT NULL THEN json_object('type','text','text',json_extract(p.value,'$.Text'))
                            WHEN json_type(p.value,'$.Image') IS NOT NULL THEN json_object('type','image','mime_type',json_extract(p.value,'$.Image.mime_type'),'data_omitted',1)
                            ELSE json_object('type','omitted')
                        END)) FROM json_each(body,'$.data.input.content') p)
                    ) AS document
                FROM entries WHERE seq=?1 AND json_extract(body,'$.kind') IN ('turn_started','steering')
            )
            SELECT turn,image_count,length(CAST(document AS BLOB)),
                   substr(CAST(document AS BLOB),?2+1,?3)
            FROM source"#,
            params![
                i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?,
                i64::try_from(offset).map_err(|_| SessionError::InvalidHistory)?,
                i64::try_from(limit.saturating_add(4)).map_err(|_| SessionError::InvalidHistory)?,
            ],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                ))
            },
        )?;
        let (turn, image_count, total, bytes) = row;
        let metadata = json!({
            "entry":entry,"kind":kind,"turn":turn,"role":"user",
            "image_count":image_count,"images_omitted":image_count.is_some_and(|count| count > 0),
            "encoding":"json","document":"committed_record"
        });
        self.page(metadata, total, bytes, offset, limit)
    }

    fn read_assistant(
        &self,
        connection: &Connection,
        stop: &CancellationToken,
        entry: u64,
        offset: usize,
        limit: usize,
    ) -> Result<Value, HistoryError> {
        if stop.is_cancelled() {
            return Err(HistoryError::request("history read cancelled"));
        }
        let row = connection.query_row(
            r#"WITH source AS (
                SELECT
                    json_extract(body,'$.data.turn') AS turn,
                    (SELECT count(*) FROM json_each(body,'$.data.message.content') p
                     WHERE json_type(p.value,'$.Image') IS NOT NULL) AS image_count,
                    json_object(
                        'entry',seq,
                        'kind','assistant',
                        'turn',json_extract(body,'$.data.turn'),
                        'role','assistant',
                        'content',(SELECT json_group_array(json(CASE
                            WHEN json_type(p.value,'$.Text') IS NOT NULL THEN json_object('type','text','text',json_extract(p.value,'$.Text'))
                            WHEN json_type(p.value,'$.ToolCall') IS NOT NULL THEN json_object(
                                'type','tool_call',
                                'id',json_extract(p.value,'$.ToolCall.id'),
                                'name',json_extract(p.value,'$.ToolCall.name'),
                                'arguments',json_extract(p.value,'$.ToolCall.arguments')
                            )
                            WHEN json_type(p.value,'$.Image') IS NOT NULL THEN json_object('type','image','mime_type',json_extract(p.value,'$.Image.mime_type'),'data_omitted',1)
                            ELSE json_object('type','omitted')
                        END)) FILTER (WHERE json_type(p.value,'$.Thinking') IS NULL)
                        FROM json_each(body,'$.data.message.content') p)
                    ) AS document
                FROM entries WHERE seq=?1
            )
            SELECT turn,image_count,length(CAST(document AS BLOB)),
                   substr(CAST(document AS BLOB),?2+1,?3)
            FROM source"#,
            params![
                i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?,
                i64::try_from(offset).map_err(|_| SessionError::InvalidHistory)?,
                i64::try_from(limit.saturating_add(4)).map_err(|_| SessionError::InvalidHistory)?,
            ],
            |row| {
                Ok((
                    row.get::<_, Option<i64>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                ))
            },
        )?;
        let (turn, image_count, total, bytes) = row;
        let metadata = json!({
            "entry":entry,"kind":"assistant","turn":turn,"role":"assistant",
            "image_count":image_count,"images_omitted":image_count.is_some_and(|count| count > 0),
            "encoding":"json","document":"committed_record"
        });
        self.page(metadata, total, bytes, offset, limit)
    }

    fn read_shell(
        &self,
        connection: &Connection,
        stop: &CancellationToken,
        entry: u64,
        offset: usize,
        limit: usize,
    ) -> Result<Value, HistoryError> {
        if stop.is_cancelled() {
            return Err(HistoryError::request("history read cancelled"));
        }
        let row = connection.query_row(
            r#"WITH source AS (
                SELECT
                    COALESCE(json_extract(s.body,'$.data.outcome.kind'),'unknown') AS state,
                    json_extract(s.body,'$.data.outcome.is_error') AS is_error,
                    json_object(
                        'entry',e.seq,
                        'kind','user_shell_admitted',
                        'command',json_extract(e.body,'$.data.command'),
                        'outcome',CASE json_extract(s.body,'$.data.outcome.kind')
                            WHEN 'observed' THEN json_object(
                                'state','observed',
                                'is_error',json_extract(s.body,'$.data.outcome.is_error'),
                                'output',json_extract(s.body,'$.data.outcome.output')
                            )
                            WHEN 'unknown' THEN json_object('state','unknown')
                            ELSE json_object('state','unknown')
                        END
                    ) AS document
                FROM entries e
                LEFT JOIN entries s ON s.seq=e.seq+1
                    AND s.seq<=?4
                    AND json_extract(s.body,'$.kind')='user_shell_settled'
                WHERE e.seq=?1 AND json_extract(e.body,'$.data.exclude_from_context')=0
            )
            SELECT state,is_error,length(CAST(document AS BLOB)),
                   substr(CAST(document AS BLOB),?2+1,?3)
            FROM source"#,
            params![
                i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?,
                i64::try_from(offset).map_err(|_| SessionError::InvalidHistory)?,
                i64::try_from(limit.saturating_add(4)).map_err(|_| SessionError::InvalidHistory)?,
                i64::try_from(self.upper_entry).map_err(|_| SessionError::InvalidHistory)?,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<bool>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                ))
            },
        )?;
        let (state, is_error, total, bytes) = row;
        let mut metadata = json!({
            "entry":entry,"kind":"user_shell_admitted","role":"user",
            "state":state,"encoding":"json","document":"committed_record"
        });
        if let Some(is_error) = is_error {
            metadata["is_error"] = json!(is_error);
        }
        self.page(metadata, total, bytes, offset, limit)
    }

    fn page(
        &self,
        mut metadata: Value,
        total: i64,
        bytes: Option<Vec<u8>>,
        offset: usize,
        limit: usize,
    ) -> Result<Value, HistoryError> {
        let total = usize::try_from(total).map_err(|_| SessionError::InvalidHistory)?;
        if offset > total {
            return Err(HistoryError::request(format!(
                "history read offset {offset} exceeds document size {total}"
            )));
        }
        let raw = bytes.unwrap_or_default();
        let text = match std::str::from_utf8(&raw) {
            Ok(text) => text,
            Err(error) if error.error_len().is_some() => {
                return Err(HistoryError::request(
                    "history read offset is not a UTF-8 character boundary",
                ));
            }
            Err(error) => {
                std::str::from_utf8(&raw[..error.valid_up_to()]).expect("valid UTF-8 prefix")
            }
        };
        let mut end = limit.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let content = &text[..end];
        let next = offset + content.len();
        metadata["offset"] = json!(offset);
        metadata["content"] = json!(content);
        metadata["next_offset"] = json!((next < total).then_some(next));
        metadata["total_bytes"] = json!(total);
        Ok(json!({
            "provenance":self.provenance(),
            "entry":metadata
        }))
    }
}

fn entry_prefix(connection: &Connection, entry: u64) -> Result<String, SessionError> {
    connection
        .query_row(
            "SELECT substr(CAST(body AS TEXT),1,?2) FROM entries WHERE seq=?1",
            params![
                i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?,
                KIND_PREFIX_BYTES as i64
            ],
            |row| row.get(0),
        )
        .map_err(Into::into)
}

fn entry_body_bytes(connection: &Connection, entry: u64) -> Result<Option<usize>, SessionError> {
    let bytes: Option<i64> = connection
        .query_row(
            "SELECT length(body) FROM entries WHERE seq=?1",
            [i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?],
            |row| row.get(0),
        )
        .optional()?;
    bytes
        .map(|bytes| usize::try_from(bytes).map_err(|_| SessionError::InvalidHistory))
        .transpose()
}

fn private_shell(connection: &Connection, entry: u64) -> Result<bool, SessionError> {
    let excluded: Option<bool> = connection
        .query_row(
            "SELECT json_extract(body,'$.data.exclude_from_context') FROM entries WHERE seq=?1 AND json_extract(body,'$.kind')='user_shell_admitted'",
            [i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(excluded.unwrap_or(false))
}

fn kind_from_prefix(prefix: &str) -> Option<&str> {
    prefix
        .strip_prefix("{\"kind\":\"")?
        .split_once('"')
        .map(|(kind, _)| kind)
}

fn hit(
    connection: &Connection,
    entry: u64,
    kind: &str,
    query: Option<&str>,
    upper_entry: u64,
) -> Result<Option<Value>, SessionError> {
    let seq = i64::try_from(entry).map_err(|_| SessionError::InvalidHistory)?;
    let preview_limit =
        i64::try_from(MAX_PREVIEW_BYTES + 4).map_err(|_| SessionError::InvalidHistory)?;
    match kind {
        "tool_result" => {
            let row = connection.query_row(
                r#"SELECT
                    json_extract(body,'$.data.turn'),
                    substr(json_extract(body,'$.data.result.call_id'),1,?4),
                    length(CAST(json_extract(body,'$.data.result.call_id') AS BLOB)),
                    substr(json_extract(body,'$.data.result.name'),1,?4),
                    length(CAST(json_extract(body,'$.data.result.name') AS BLOB)),
                    json_extract(body,'$.data.result.outcome.state'),
                    json_extract(body,'$.data.result.outcome.output.is_error'),
                    json_array_length(json_extract(body,'$.data.result.outcome.output.images')),
                    CASE WHEN ?1 IS NULL THEN 1 ELSE instr(lower(
                        COALESCE(json_extract(body,'$.data.result.call_id'),'') || char(10) ||
                        COALESCE(json_extract(body,'$.data.result.name'),'') || char(10) ||
                        COALESCE(CAST((CAST(body AS TEXT) -> '$.data.result.outcome.output.value') AS TEXT),'') || char(10) ||
                        COALESCE(json_extract(body,'$.data.result.outcome.reason'),'')
                    ),lower(?1)) END,
                    substr(CAST(COALESCE(
                        CAST((CAST(body AS TEXT) -> '$.data.result.outcome.output.value') AS TEXT),
                        json_extract(body,'$.data.result.outcome.reason'),''
                    ) AS BLOB),1,?2),
                    length(CAST(COALESCE(
                        CAST((CAST(body AS TEXT) -> '$.data.result.outcome.output.value') AS TEXT),
                        json_extract(body,'$.data.result.outcome.reason'),''
                    ) AS BLOB))
                FROM entries WHERE seq=?3"#,
                params![
                    query,
                    preview_limit,
                    seq,
                    i64::try_from(MAX_METADATA_BYTES).map_err(|_| SessionError::InvalidHistory)?
                ],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Option<bool>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, Option<Vec<u8>>>(9)?,
                        row.get::<_, Option<i64>>(10)?,
                    ))
                },
            )?;
            let (
                turn,
                call_id,
                call_id_bytes,
                name,
                name_bytes,
                state,
                is_error,
                image_count,
                matched,
                preview_bytes,
                total,
            ) = row;
            if matched == 0 {
                return Ok(None);
            }
            let (preview, truncated) = preview(preview_bytes, total);
            let mut value = json!({
                "entry":entry,"kind":"tool_result","turn":turn,"state":state,
                "preview":preview,"preview_truncated":truncated
            });
            if let (Some(call_id), Some(call_id_bytes), Some(name), Some(name_bytes)) =
                (call_id, call_id_bytes, name, name_bytes)
            {
                let (call_id, call_id_truncated) = bounded_metadata(call_id, call_id_bytes)?;
                let (name, name_truncated) = bounded_metadata(name, name_bytes)?;
                value["tool"] = json!({"call_id":call_id,"name":name});
                if call_id_truncated || name_truncated {
                    value["tool_truncated"] = json!(true);
                }
            }
            if let Some(is_error) = is_error {
                value["is_error"] = json!(is_error);
            }
            if let Some(image_count) = image_count {
                value["image_count"] = json!(image_count);
                value["images_omitted"] = json!(image_count > 0);
            }
            Ok(Some(value))
        }
        "assistant" => {
            let row = connection.query_row(
                r#"SELECT
                    json_extract(body,'$.data.turn'),
                    (SELECT count(*) FROM json_each(body,'$.data.message.content') p
                     WHERE json_type(p.value,'$.Image') IS NOT NULL),
                    CASE WHEN ?1 IS NULL THEN 1 ELSE EXISTS(
                        SELECT 1 FROM json_each(body,'$.data.message.content') p
                        WHERE (json_type(p.value,'$.Text') IS NOT NULL
                               AND instr(lower(json_extract(p.value,'$.Text')),lower(?1))>0)
                           OR (json_type(p.value,'$.ToolCall') IS NOT NULL
                               AND instr(lower(
                                   COALESCE(json_extract(p.value,'$.ToolCall.name'),'') || char(10) ||
                                   COALESCE(CAST(json_extract(p.value,'$.ToolCall.arguments') AS TEXT),'')
                               ),lower(?1))>0)
                    ) END,
                    substr(CAST(COALESCE(
                        (SELECT json_extract(p.value,'$.Text') FROM json_each(body,'$.data.message.content') p
                         WHERE json_type(p.value,'$.Text') IS NOT NULL ORDER BY p.key LIMIT 1),
                        (SELECT 'tool_call: ' || json_extract(p.value,'$.ToolCall.name')
                         FROM json_each(body,'$.data.message.content') p
                         WHERE json_type(p.value,'$.ToolCall') IS NOT NULL ORDER BY p.key LIMIT 1),
                        ''
                    ) AS BLOB),1,?2),
                    length(CAST(COALESCE(
                        (SELECT json_extract(p.value,'$.Text') FROM json_each(body,'$.data.message.content') p
                         WHERE json_type(p.value,'$.Text') IS NOT NULL ORDER BY p.key LIMIT 1),
                        (SELECT 'tool_call: ' || json_extract(p.value,'$.ToolCall.name')
                         FROM json_each(body,'$.data.message.content') p
                         WHERE json_type(p.value,'$.ToolCall') IS NOT NULL ORDER BY p.key LIMIT 1),
                        ''
                    ) AS BLOB))
                FROM entries WHERE seq=?3"#,
                params![query, preview_limit, seq],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            )?;
            let (turn, image_count, matched, preview_bytes, total) = row;
            if matched == 0 {
                return Ok(None);
            }
            let (preview, truncated) = preview(preview_bytes, total);
            Ok(Some(json!({
                "entry":entry,"kind":"assistant","role":"assistant","turn":turn,
                "image_count":image_count,"images_omitted":image_count.is_some_and(|count| count > 0),
                "preview":preview,"preview_truncated":truncated
            })))
        }
        "turn_started" | "steering" => {
            let row = connection.query_row(
                r#"SELECT
                    json_extract(body,'$.data.turn'),
                    (SELECT count(*) FROM json_each(body,'$.data.input.content') p
                     WHERE json_type(p.value,'$.Image') IS NOT NULL),
                    CASE WHEN ?1 IS NULL THEN 1 ELSE EXISTS(
                        SELECT 1 FROM json_each(body,'$.data.input.content') p
                        WHERE json_type(p.value,'$.Text') IS NOT NULL
                          AND instr(lower(json_extract(p.value,'$.Text')),lower(?1))>0
                    ) END,
                    substr(CAST(COALESCE(
                        (SELECT json_extract(p.value,'$.Text') FROM json_each(body,'$.data.input.content') p
                         WHERE json_type(p.value,'$.Text') IS NOT NULL ORDER BY p.key LIMIT 1),''
                    ) AS BLOB),1,?2),
                    length(CAST(COALESCE(
                        (SELECT json_extract(p.value,'$.Text') FROM json_each(body,'$.data.input.content') p
                         WHERE json_type(p.value,'$.Text') IS NOT NULL ORDER BY p.key LIMIT 1),''
                    ) AS BLOB))
                FROM entries WHERE seq=?3"#,
                params![query, preview_limit, seq],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<i64>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            )?;
            let (turn, image_count, matched, preview_bytes, total) = row;
            if matched == 0 {
                return Ok(None);
            }
            let (preview, truncated) = preview(preview_bytes, total);
            Ok(Some(json!({
                "entry":entry,"kind":kind,"role":"user","turn":turn,
                "image_count":image_count,"images_omitted":image_count.is_some_and(|count| count > 0),
                "preview":preview,"preview_truncated":truncated
            })))
        }
        "user_shell_admitted" => {
            let row = connection.query_row(
                r#"SELECT
                    COALESCE(json_extract(s.body,'$.data.outcome.kind'),'unknown'),
                    json_extract(s.body,'$.data.outcome.is_error'),
                    CASE WHEN ?1 IS NULL THEN 1 ELSE instr(lower(
                        COALESCE(json_extract(e.body,'$.data.command'),'') || char(10) ||
                        COALESCE(CAST(json_extract(s.body,'$.data.outcome.output') AS TEXT),'')
                    ),lower(?1)) END,
                    substr(CAST(json_extract(e.body,'$.data.command') AS BLOB),1,?2),
                    length(CAST(json_extract(e.body,'$.data.command') AS BLOB))
                FROM entries e
                LEFT JOIN entries s ON s.seq=e.seq+1
                    AND s.seq<=?4
                    AND json_extract(s.body,'$.kind')='user_shell_settled'
                WHERE e.seq=?3"#,
                params![
                    query,
                    preview_limit,
                    seq,
                    i64::try_from(upper_entry).map_err(|_| SessionError::InvalidHistory)?
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<bool>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                    ))
                },
            )?;
            let (state, is_error, matched, preview_bytes, total) = row;
            if matched == 0 {
                return Ok(None);
            }
            let (preview, truncated) = preview(preview_bytes, total);
            let mut value = json!({
                "entry":entry,"kind":"user_shell_admitted","role":"user","state":state,
                "preview":preview,"preview_truncated":truncated
            });
            if let Some(is_error) = is_error {
                value["is_error"] = json!(is_error);
            }
            Ok(Some(value))
        }
        _ => Ok(None),
    }
}

/// The SQL prefix is character-bounded to stay UTF-8 safe; this trims it to the
/// byte bound. The source field's full byte length reports truncation even when
/// the prefix was cut by characters, so the returned string never exceeds the
/// byte limit.
fn bounded_metadata(prefix: String, total_bytes: i64) -> Result<(String, bool), SessionError> {
    let total = usize::try_from(total_bytes).map_err(|_| SessionError::InvalidHistory)?;
    let mut end = prefix.len().min(MAX_METADATA_BYTES);
    while !prefix.is_char_boundary(end) {
        end -= 1;
    }
    Ok((prefix[..end].to_owned(), total > end))
}

fn preview(bytes: Option<Vec<u8>>, total: Option<i64>) -> (String, bool) {
    let Some(bytes) = bytes else {
        return (String::new(), false);
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).expect("valid UTF-8 prefix")
        }
    };
    let total = total.and_then(|total| usize::try_from(total).ok());
    (
        text.to_owned(),
        total.is_some_and(|total| total > text.len()),
    )
}

pub(crate) async fn execute(
    session: &std::sync::Arc<Session>,
    call: &ToolCall,
    stop: CancellationToken,
) -> ToolOutput {
    if stop.is_cancelled() {
        return failure("history read cancelled");
    }
    let query = match serde_json::to_string(&call.arguments) {
        Ok(query) => query,
        Err(error) => return failure(format!("invalid history arguments: {error}")),
    };
    if query.len() > super::MAX_ENTRY_BYTES {
        return failure("history arguments exceed the entry bound");
    }
    let read = match session.prepare_history(&query) {
        Ok(read) => read,
        Err(error) => return failure(error.to_string()),
    };
    let worker_stop = stop.clone();
    let joined = tokio::task::spawn_blocking(move || read.read(&worker_stop)).await;
    match joined {
        Ok(Ok(value)) => ToolOutput {
            value,
            images: Vec::new(),
            is_error: false,
        },
        Ok(Err(error)) => failure(error.to_string()),
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => failure(format!("history worker stopped: {error}")),
    }
}

fn failure(message: impl Into<String>) -> ToolOutput {
    ToolOutput {
        value: json!({"error":message.into()}),
        images: Vec::new(),
        is_error: true,
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
