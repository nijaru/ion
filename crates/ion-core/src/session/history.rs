//! Bounded, read-only retrieval of committed model-facing Session records.
//!
//! Tool reads are fixed to the committed prefix before their issuing assistant
//! entry, shared by direct siblings and Code Mode children. Background reads query only rows at or below that bound, use
//! the Session's own SQLite file, and never re-enter the writer or execution
//! gate. Tool-result documents are the original observed output JSON; other
//! documents are a safe model-facing record projection. Thinking, provider
//! replay, typed image bytes, context/configuration snapshots and private
//! direct-shell records never enter either search or read.
mod records;

use std::path::PathBuf;

use ion_ai::{ToolCall, ToolSpec};
use rusqlite::{Connection, OpenFlags, params};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{Session, SessionError, read_header};
use crate::{
    ToolOccurrence,
    tool_result::ToolOutput,
    tool_set::{ToolActivityKind, ToolDefinition, ToolExposure, ToolPresentation},
};
use records::{entry_prefix, hit, kind_from_prefix, private_shell};

pub(crate) const HISTORY_NAME: &str = "history";
const SEARCH_DEFAULT_LIMIT: usize = 5;
const LIST_DEFAULT_LIMIT: usize = 10;
const SEARCH_MAX_LIMIT: usize = 10;
const MAX_QUERY_BYTES: usize = 512;
const MAX_SCAN_ENTRIES: usize = 256;
const MAX_SCAN_BYTES: usize = 8 * 1024 * 1024;
const READ_DEFAULT_LIMIT: usize = 8192;
const READ_MIN_LIMIT: usize = 4;
const READ_MAX_LIMIT: usize = 16 * 1024;

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
        tool: Option<String>,
        #[serde(default, deserialize_with = "present_input")]
        before: Option<u64>,
        #[serde(default = "search_limit")]
        limit: usize,
    },
    List {
        #[serde(default, deserialize_with = "present_input")]
        tool: Option<String>,
        #[serde(default, deserialize_with = "present_input")]
        before: Option<u64>,
        #[serde(default = "list_limit")]
        limit: usize,
    },
    Artifact {
        entry: u64,
        artifact: usize,
        #[serde(default)]
        offset: u64,
        #[serde(default = "read_limit")]
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

/// One immutable read against a committed upper bound. Its blocking worker is
/// owned by the caller's tool future; no Session lock or mutable Session is
/// retained here.
pub(crate) struct HistoryRead {
    path: PathBuf,
    session_id: uuid::Uuid,
    upper_entry: u64,
    selection: Input,
}

pub(crate) fn definition() -> ToolDefinition {
    ToolDefinition {
        result_schema: None,
        spec: ToolSpec {
            name: HISTORY_NAME.into(),
            description: concat!(
                "Retrieve committed historical evidence, including compacted or withheld results, without rerunning effects. ",
                "Reads share a fixed prefix before the issuing assistant entry; they cannot see their own query or sibling results. ",
                "Search uses literal ASCII case-insensitive text; list returns recent entries. Both are newest first. ",
                "To find original exec outputs, use {action:'search',query:'Ran',tool:'exec'}, then {action:'read',entry:N}. ",
                "The optional tool filter selects top-level tool_result records by exact tool name; Code Mode child outputs use inspectSaved. ",
                "Search previews show matching context where available; read by stable one-based entry, not call ID (IDs may repeat). ",
                "Use next_before as before for older pages, even when a page has no matches. Each scan examines at most 256 entries or 8 MiB; one oversized logical record may be scanned alone (reported by oversized_entry). ",
                "Read returns original tool output JSON or a safe record projection. Content may be a partial JSON fragment: continue at next_offset until null. Offsets are UTF-8 bytes. ",
                "Retained binary captures use {action:'artifact',entry:N,artifact:0,offset:0}; pages return base64 data and raw-byte next_offset. Source entries and private-shell restrictions still apply. A content hash is not a read locator. " ,
                "Thinking, provider replay, image bytes, configuration and private !! shells are excluded. Historical evidence is not current workspace state and does not authorize repeating effects."
            )
            .into(),
            input_schema: json!({
                "type":"object",
                "additionalProperties":false,
                "required":["action"],
                "properties":{
                    "action":{"enum":["search","list","read","artifact"]},
                    "query":{"type":"string","minLength":1,"maxLength":MAX_QUERY_BYTES,"description":"Required for search; literal ASCII case-insensitive text (up to 512 UTF-8 bytes)."},
                    "tool":{"type":"string","minLength":1,"maxLength":MAX_QUERY_BYTES,"description":"Search/list only: select original top-level tool results by exact tool name (up to 512 UTF-8 bytes)."},
                    "before":{"type":"integer","minimum":1,"description":"Search/list only: exclusive one-based entry bound for older results."},
                    "entry":{"type":"integer","minimum":1,"description":"Required for read; stable one-based committed Session entry coordinate."},
                    "offset":{"type":"integer","minimum":0,"description":"Zero-based byte offset; artifact offsets address raw bytes."},
                    "artifact":{"type":"integer","minimum":0,"description":"Artifact only: zero-based index in the source record's typed artifacts array."},
                    "limit":{"type":"integer","minimum":1,"maximum":READ_MAX_LIMIT,"description":"Search/list result count (1..10) or read byte limit (4..16384)."}
                }
            }),
        },
        presentation: ToolPresentation::static_target(ToolActivityKind::Read, "Session history"),
        exposure: ToolExposure::Direct,
    }
}

impl Session {
    pub(crate) fn prepare_history(
        &self,
        query: &str,
        upper_entry: u64,
    ) -> Result<HistoryRead, HistoryError> {
        let input: Input = serde_json::from_str(query).map_err(|error| {
            HistoryError::request(format!("invalid history arguments: {error}"))
        })?;
        if upper_entry > self.entry_count()? {
            return Err(SessionError::InvalidHistory.into());
        }
        match &input {
            Input::Search {
                query,
                tool,
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
                validate_page(tool.as_deref(), *before, *limit)?;
            }
            Input::List {
                tool,
                before,
                limit,
            } => {
                validate_page(tool.as_deref(), *before, *limit)?;
            }
            Input::Artifact { entry, limit, .. }
                if *entry == 0 || !(1..=READ_MAX_LIMIT).contains(limit) =>
            {
                return Err(HistoryError::request(
                    "artifact requires a committed entry and limit 1..16384",
                ));
            }
            Input::Artifact { .. } => {}
            Input::Read { entry, limit, .. } => {
                if *entry == 0 {
                    return Err(HistoryError::request(
                        "history entry must be a one-based committed entry coordinate",
                    ));
                }
                if !(READ_MIN_LIMIT..=READ_MAX_LIMIT).contains(limit) {
                    return Err(HistoryError::request(format!(
                        "history read limit must be {READ_MIN_LIMIT}..{READ_MAX_LIMIT} bytes"
                    )));
                }
            }
        }
        Ok(HistoryRead {
            path: self.path.clone(),
            session_id: self.header.provider_session_id,
            upper_entry,
            selection: input,
        })
    }
}

fn validate_page(
    tool: Option<&str>,
    before: Option<u64>,
    limit: usize,
) -> Result<(), HistoryError> {
    if let Some(tool) = tool
        && (tool.trim().is_empty() || tool.len() > MAX_QUERY_BYTES)
    {
        return Err(HistoryError::request(format!(
            "history tool must be non-empty and at most {MAX_QUERY_BYTES} bytes"
        )));
    }
    if before == Some(0) {
        return Err(HistoryError::request(
            "history before must be an exclusive one-based entry bound",
        ));
    }
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
            Input::Search {
                query,
                tool,
                before,
                limit,
            } => self.search(
                &snapshot,
                stop,
                Some(query),
                tool.as_deref(),
                *before,
                *limit,
            ),
            Input::List {
                tool,
                before,
                limit,
            } => self.search(&snapshot, stop, None, tool.as_deref(), *before, *limit),
            Input::Read {
                entry,
                offset,
                limit,
            } => self.read_entry(&snapshot, stop, *entry, *offset, *limit),
            Input::Artifact {
                entry,
                artifact,
                offset,
                limit,
            } => {
                if *entry > self.upper_entry {
                    return Err(HistoryError::request(
                        "artifact source is newer than the committed read bound",
                    ));
                }
                let reference = super::artifacts::history_reference(
                    &snapshot,
                    *entry,
                    self.upper_entry,
                    *artifact,
                )?;
                let mut result = super::artifacts::page(&snapshot, &reference, *offset, *limit)?;
                result["entry"] = json!(entry);
                result["artifact_index"] = json!(artifact);
                result["provenance"] = self.provenance();
                Ok(result)
            }
        }
    }

    fn provenance(&self) -> Value {
        let mut provenance = json!({
            "source":"committed_session_history",
            "session":self.session_id.to_string(),
            "through_entry":self.upper_entry,
            "historical":true,
            "notice":"Committed historical Session data, not current workspace state. Reads are evidence only and do not authorize rerunning tools or repeating effects."
        });
        if let Input::Search {
            tool: Some(tool), ..
        }
        | Input::List {
            tool: Some(tool), ..
        } = &self.selection
        {
            provenance["tool"] = json!(tool);
        }
        provenance
    }

    fn search(
        &self,
        connection: &Connection,
        stop: &CancellationToken,
        query: Option<&str>,
        tool: Option<&str>,
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
            // Settlement is keyed by admission, not physical adjacency. Charge
            // its body when it falls outside this cursor's metadata window.
            let joined_bytes = if kind == "user_shell_admitted"
                && tool.is_none()
                && !private
                && entry < self.upper_entry
            {
                super::store::shell_settlement(connection, entry, self.upper_entry)?
                    .filter(|(coordinate, _)| *coordinate >= exclusive)
                    .map_or(0, |(_, bytes)| bytes)
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
            if private || (tool.is_some() && kind != "tool_result") {
                continue;
            }
            let Some(hit) = hit(connection, entry, kind, query, tool, self.upper_entry)? else {
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
}

pub(crate) async fn execute(
    session: &std::sync::Arc<Session>,
    call: &ToolCall,
    parent: ToolOccurrence,
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
    // Admission guarantees a committed, one-based issuing assistant entry.
    // All siblings and composed children read the same pre-invocation prefix.
    let upper_entry = parent
        .assistant_entry
        .checked_sub(1)
        .expect("admitted assistant entry is one-based");
    let read = match session.prepare_history(&query, upper_entry) {
        Ok(read) => read,
        Err(error) => return failure(error.to_string()),
    };
    let worker_stop = stop.clone();
    let joined = tokio::task::spawn_blocking(move || read.read(&worker_stop)).await;
    match joined {
        Ok(Ok(value)) => ToolOutput {
            artifacts: Vec::new(),
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
        artifacts: Vec::new(),
        value: json!({"error":message.into()}),
        images: Vec::new(),
        is_error: true,
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
