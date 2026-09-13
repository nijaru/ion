//! Entry rows. Entries are immutable and append-only; `id` is the session-local
//! sequence, so it is also their ordering key.

use rusqlite::{Connection, params};

use super::{StoreError, id_from, json_from, json_to};
use crate::{Entry, EntryKind};

pub(crate) fn insert(connection: &Connection, entry: &Entry) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO entries (id, conversation_id, kind, data, projection, context)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            entry.id.get(),
            entry.conversation_id.get(),
            entry.kind.as_str(),
            json_to(&entry.data)?,
            json_to(&entry.projection)?,
            json_to(&entry.context)?,
        ],
    )?;
    Ok(())
}

pub(crate) fn load(connection: &Connection) -> Result<Vec<Entry>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, conversation_id, kind, data, projection, context FROM entries ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;

    let mut entries = Vec::new();
    for row in rows {
        let (id, conversation_id, kind, data, projection, context) = row?;
        let kind = EntryKind::new(kind).map_err(|error| {
            StoreError::other(format!("entry {id} has an invalid kind: {error}"))
        })?;
        entries.push(Entry {
            id: id_from(id)?,
            conversation_id: id_from(conversation_id)?,
            kind,
            data: json_from(&data)?,
            projection: json_from(&projection)?,
            context: json_from(&context)?,
        });
    }
    Ok(entries)
}
