//! Record eligibility and SQL projection for bounded Session history reads.
//!
//! Committed entry kinds are classified here, including private direct-shell
//! exclusion. Search matches and read documents are projected directly from the
//! Session database without retaining a writer or execution lock.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{HistoryError, HistoryRead, SessionError};

const MAX_PREVIEW_BYTES: usize = 384;
pub(super) const MAX_METADATA_BYTES: usize = 128;
const KIND_PREFIX_BYTES: usize = 64;

impl HistoryRead {
    pub(super) fn read_entry(
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

pub(super) fn entry_prefix(connection: &Connection, entry: u64) -> Result<String, SessionError> {
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

pub(super) fn entry_body_bytes(
    connection: &Connection,
    entry: u64,
) -> Result<Option<usize>, SessionError> {
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

pub(super) fn private_shell(connection: &Connection, entry: u64) -> Result<bool, SessionError> {
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

pub(super) fn kind_from_prefix(prefix: &str) -> Option<&str> {
    prefix
        .strip_prefix("{\"kind\":\"")?
        .split_once('"')
        .map(|(kind, _)| kind)
}

pub(super) fn hit(
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
