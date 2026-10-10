//! Explicit, read-only access to composed child facts, never general history.
use std::{
    collections::{BTreeMap, HashMap},
    ops::Bound::{Excluded, Unbounded},
};

use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Session, SessionEntry, SessionError};
use crate::ToolOccurrence;

#[derive(Default)]
pub(super) struct ChildRecordIndex(HashMap<ToolOccurrence, BTreeMap<usize, ChildRecords>>);
#[derive(Default)]
struct ChildRecords {
    intent: u64,
    outcome: Option<u64>,
}
impl ChildRecordIndex {
    /// Rebuildable row coordinates only. Call after history validation/commit;
    /// no payload, execution state or new durable authority lives here.
    pub(super) fn observe(&mut self, sequence: u64, entry: &SessionEntry) {
        match entry {
            SessionEntry::ChildToolAdmitted { intent, .. } => {
                self.0.entry(intent.parent).or_default().insert(
                    intent.child,
                    ChildRecords {
                        intent: sequence,
                        outcome: None,
                    },
                );
            }
            SessionEntry::ChildToolResult { parent, child, .. } => {
                self.0
                    .get_mut(parent)
                    .and_then(|children| children.get_mut(child))
                    .expect("validated child result has an admitted intent")
                    .outcome = Some(sequence);
            }
            _ => {}
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InspectionError {
    #[error("{0}")]
    Request(String),
    #[error(transparent)]
    Session(#[from] SessionError),
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Query {
    Children {
        parent: ToolOccurrence,
        after: Option<usize>,
    },
    Output {
        parent: ToolOccurrence,
        child: usize,
        #[serde(default)]
        pointer: String,
        #[serde(default)]
        offset: usize,
        #[serde(default = "page_limit")]
        limit: usize,
    },
}
fn page_limit() -> usize {
    4096
}

impl Session {
    pub(crate) fn inspect_code(&self, query: &str) -> Result<Value, InspectionError> {
        let query: Query = serde_json::from_str(query).map_err(|error| {
            InspectionError::Request(format!("invalid inspection query: {error}"))
        })?;
        let parent = match &query {
            Query::Children { parent, .. } | Query::Output { parent, .. } => *parent,
        };
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        if store
            .state
            .pending
            .iter()
            .any(|pending| pending.occurrence == parent)
        {
            return Err(InspectionError::Request(
                "inspection requires a settled parent call".into(),
            ));
        }
        let children = store.child_records.0.get(&parent).ok_or_else(|| {
            InspectionError::Request("parent has no saved Code Mode child calls".into())
        })?;
        match query {
            Query::Children { after, .. } => {
                let lower = after.map_or(Unbounded, Excluded);
                let mut rows = children.range((lower, Unbounded));
                let mut summaries = Vec::new();
                for (child, records) in rows.by_ref().take(10) {
                    summaries.push(summary(&store.connection, *child, records, true)?);
                }
                let next_after = rows
                    .next()
                    .and_then(|_| summaries.last().map(|s| s["child"].clone()));
                Ok(json!({"parent":parent,"children":summaries,"next_after":next_after}))
            }
            Query::Output {
                child,
                pointer,
                offset,
                limit,
                ..
            } => {
                if !(4..=65536).contains(&limit)
                    || !(pointer.is_empty() || pointer.starts_with('/'))
                {
                    return Err(InspectionError::Request(
                        "output limit must be 4..65536 bytes and pointer a JSON Pointer".into(),
                    ));
                }
                let records = children
                    .get(&child)
                    .ok_or_else(|| InspectionError::Request("unknown child ordinal".into()))?;
                let mut result = summary(&store.connection, child, records, false)?;
                if result["state"] != "observed" {
                    return Ok(result);
                }
                // Select only the native JSON value. Image bytes, assistant/provider
                // replay, direct-shell facts and arbitrary Session entries cannot
                // cross this interface. Each lookup reads one indexed fact, not a
                // whole history clone. The existing entry bound limits decoding.
                let encoded: String = store.connection.query_row(
                    "SELECT CAST(body AS TEXT) -> '$.data.outcome.output.value' FROM entries WHERE seq=?1",
                    [i64::try_from(records.outcome.ok_or(SessionError::InvalidHistory)?).map_err(|_| SessionError::InvalidHistory)?], |row| row.get(0)
                ).map_err(SessionError::from)?;
                let value: Value = serde_json::from_str(&encoded).map_err(SessionError::from)?;
                let value = value.pointer(&pointer).ok_or_else(|| {
                    InspectionError::Request("JSON Pointer does not select a saved value".into())
                })?;
                let page = crate::json_size::encoded_window(value, offset, limit)
                    .map_err(SessionError::from)?
                    .ok_or_else(|| {
                        InspectionError::Request(
                            "offset must be a UTF-8 boundary within the encoded value".into(),
                        )
                    })?;
                result["offset"] = json!(offset);
                result["json"] = json!(page.text);
                result["next_offset"] = json!(page.next_offset);
                result["total_bytes"] = json!(page.total_bytes);
                Ok(result)
            }
        }
    }
}

struct OutcomeMetadata {
    state: String,
    is_error: Option<bool>,
    value_type: Option<String>,
    image_count: Option<i64>,
    reason: Option<String>,
    reason_bytes: Option<i64>,
}

fn summary(
    connection: &Connection,
    child: usize,
    records: &ChildRecords,
    discover_keys: bool,
) -> Result<Value, SessionError> {
    let (name, name_bytes): (String, i64) = connection.query_row(
        "SELECT substr(json_extract(CAST(body AS TEXT), '$.data.intent.call.name'),1,64), length(CAST(json_extract(CAST(body AS TEXT), '$.data.intent.call.name') AS BLOB)) FROM entries WHERE seq=?1",
        [i64::try_from(records.intent).map_err(|_| SessionError::InvalidHistory)?], |row| Ok((row.get(0)?,row.get(1)?))
    )?;
    let outcome = i64::try_from(records.outcome.ok_or(SessionError::InvalidHistory)?)
        .map_err(|_| SessionError::InvalidHistory)?;
    let metadata = connection.query_row(
        "SELECT json_extract(CAST(body AS TEXT), '$.data.outcome.state'),
                json_extract(CAST(body AS TEXT), '$.data.outcome.output.is_error'),
                json_type(CAST(body AS TEXT), '$.data.outcome.output.value'),
                json_array_length(CAST(body AS TEXT), '$.data.outcome.output.images'),
                substr(json_extract(CAST(body AS TEXT), '$.data.outcome.reason'),1,256),
                length(CAST(json_extract(CAST(body AS TEXT), '$.data.outcome.reason') AS BLOB)) FROM entries WHERE seq=?1",
        [outcome], |row| Ok(OutcomeMetadata {
            state: row.get(0)?,
            is_error: row.get(1)?,
            value_type: row.get(2)?,
            image_count: row.get(3)?,
            reason: row.get(4)?,
            reason_bytes: row.get(5)?,
        })
    )?;
    let name_truncated =
        usize::try_from(name_bytes).map_err(|_| SessionError::InvalidHistory)? > name.len();
    let mut result =
        json!({"child":child,"name":name,"name_truncated":name_truncated,"state":metadata.state});
    match metadata.state.as_str() {
        "observed" => {
            result["is_error"] = json!(metadata.is_error.ok_or(SessionError::InvalidHistory)?);
            result["image_count"] =
                json!(metadata.image_count.ok_or(SessionError::InvalidHistory)?);
            result["root_value_type"] = json!(
                metadata
                    .value_type
                    .as_ref()
                    .ok_or(SessionError::InvalidHistory)?
            );
            if discover_keys && metadata.value_type.as_deref() == Some("object") {
                let mut statement = connection.prepare("SELECT CASE WHEN length(CAST(key AS BLOB))<=64 THEN key ELSE NULL END FROM entries, json_each(CAST(body AS TEXT), '$.data.outcome.output.value') WHERE seq=?1 LIMIT 17")?;
                let keys = statement
                    .query_map(params![outcome], |row| row.get::<_, Option<String>>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                result["keys_truncated"] =
                    json!(keys.len() > 16 || keys.iter().any(Option::is_none));
                result["value_keys"] =
                    json!(keys.into_iter().take(16).flatten().collect::<Vec<_>>());
            }
        }
        "not_dispatched" => {
            let reason = metadata.reason.ok_or(SessionError::InvalidHistory)?;
            let bytes = usize::try_from(metadata.reason_bytes.ok_or(SessionError::InvalidHistory)?)
                .map_err(|_| SessionError::InvalidHistory)?;
            result["reason_truncated"] = json!(bytes > reason.len());
            result["reason"] = json!(reason);
        }
        "unknown" => {}
        _ => return Err(SessionError::InvalidHistory),
    }
    Ok(result)
}
