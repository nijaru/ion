//! Explicit, read-only access to composed child facts, never general history.
use std::{
    collections::BTreeMap,
    ops::Bound::{Excluded, Unbounded},
    path::PathBuf,
};

use rusqlite::{Connection, OpenFlags, params};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Session, SessionEntry, SessionError, Store};
use crate::ToolOccurrence;

#[derive(Default)]
pub(super) struct ChildRecordIndex(BTreeMap<ToolOccurrence, BTreeMap<usize, ChildRecords>>);
#[derive(Clone, Copy, Default)]
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
    Parents {
        before: Option<ToolOccurrence>,
    },
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

/// A bounded set of immutable, committed row coordinates. The gateway owns
/// its read task; no Session lock or execution authority enters that task.
pub(crate) struct CodeInspection {
    path: PathBuf,
    session_id: uuid::Uuid,
    selection: Selection,
}
enum Selection {
    Parents {
        rows: Vec<(ToolOccurrence, usize)>,
        next_before: Option<ToolOccurrence>,
    },
    Children {
        parent: ToolOccurrence,
        rows: Vec<(usize, ChildRecords)>,
        next_after: Option<usize>,
    },
    Output {
        child: usize,
        records: ChildRecords,
        pointer: String,
        offset: usize,
        limit: usize,
    },
}

impl Session {
    pub(crate) fn prepare_code_inspection(
        &self,
        query: &str,
    ) -> Result<CodeInspection, InspectionError> {
        let query: Query = serde_json::from_str(query).map_err(|error| {
            InspectionError::Request(format!("invalid inspection query: {error}"))
        })?;
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let selection = match query {
            Query::Parents { before } => {
                let upper = before.map_or(Unbounded, Excluded);
                let mut parents = store
                    .child_records
                    .0
                    .range((Unbounded, upper))
                    .rev()
                    .filter(|(parent, _)| {
                        !store
                            .state
                            .pending
                            .iter()
                            .any(|pending| pending.occurrence == **parent)
                    });
                let rows = parents
                    .by_ref()
                    .take(10)
                    .map(|(&parent, children)| (parent, children.len()))
                    .collect::<Vec<_>>();
                let next_before = parents.next().and_then(|_| rows.last().map(|row| row.0));
                Selection::Parents { rows, next_before }
            }
            Query::Children { parent, after } => {
                let children = settled_children(&store, parent)?;
                let lower = after.map_or(Unbounded, Excluded);
                let mut children = children.range((lower, Unbounded));
                let rows = children
                    .by_ref()
                    .take(10)
                    .map(|(&child, &records)| (child, records))
                    .collect::<Vec<_>>();
                let next_after = children.next().and_then(|_| rows.last().map(|row| row.0));
                Selection::Children {
                    parent,
                    rows,
                    next_after,
                }
            }
            Query::Output {
                parent,
                child,
                pointer,
                offset,
                limit,
            } => {
                if !(4..=65536).contains(&limit)
                    || !(pointer.is_empty() || pointer.starts_with('/'))
                {
                    return Err(InspectionError::Request(
                        "output limit must be 4..65536 bytes and pointer a JSON Pointer".into(),
                    ));
                }
                let records = *settled_children(&store, parent)?
                    .get(&child)
                    .ok_or_else(|| InspectionError::Request("unknown child ordinal".into()))?;
                Selection::Output {
                    child,
                    records,
                    pointer,
                    offset,
                    limit,
                }
            }
        };
        Ok(CodeInspection {
            path: self.path.clone(),
            session_id: self.header.provider_session_id,
            selection,
        })
    }

    #[cfg(test)]
    pub(crate) fn inspect_code(&self, query: &str) -> Result<Value, InspectionError> {
        self.prepare_code_inspection(query)?.read()
    }
}

fn settled_children(
    store: &Store,
    parent: ToolOccurrence,
) -> Result<&BTreeMap<usize, ChildRecords>, InspectionError> {
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
    store
        .child_records
        .0
        .get(&parent)
        .ok_or_else(|| InspectionError::Request("parent has no saved Code Mode child calls".into()))
}

impl CodeInspection {
    pub(crate) fn read(self) -> Result<Value, InspectionError> {
        // WAL readers do not hold the writer/state mutex through filesystem
        // waits or maximum-record decoding. Pin one read snapshot and reject a
        // replaced database rather than consulting another Session's rows.
        let connection = Connection::open_with_flags(self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(SessionError::from)?;
        super::configure_connection(&connection)?;
        let snapshot = connection
            .unchecked_transaction()
            .map_err(SessionError::from)?;
        if super::read_header(&snapshot)?.provider_session_id != self.session_id {
            return Err(SessionError::InvalidDatabase.into());
        }
        match self.selection {
            Selection::Parents { rows, next_before } => {
                let parents = rows
                    .into_iter()
                    .map(|(parent, child_count)| json!({"parent":parent,"child_count":child_count}))
                    .collect::<Vec<_>>();
                Ok(json!({"parents":parents,"next_before":next_before}))
            }
            Selection::Children {
                parent,
                rows,
                next_after,
            } => {
                let summaries = rows
                    .into_iter()
                    .map(|(child, records)| summary(&snapshot, child, &records, true))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(json!({"parent":parent,"children":summaries,"next_after":next_after}))
            }
            Selection::Output {
                child,
                records,
                pointer,
                offset,
                limit,
            } => {
                let mut result = summary(&snapshot, child, &records, false)?;
                if result["state"] != "observed" {
                    return Ok(result);
                }
                // Select only the native JSON value. Image bytes, assistant/provider
                // replay, direct-shell facts and arbitrary Session entries cannot
                // cross this interface. Each lookup reads one indexed fact, not a
                // whole history clone. The existing entry bound limits decoding.
                let encoded: String = snapshot.query_row(
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
