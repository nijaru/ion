//! Input rows, including the durable request-key and admission-commit mappings
//! used for idempotent replay after reopen.

use rusqlite::{Connection, params};

use super::{StoreError, id_from, json_from, json_to};
use crate::{CommitSeq, Input, InputDisposition, InputId, RequestKey};

/// Insert an admitted input together with the commit that admitted it. The
/// admission commit is not itself a mutation: a store derives it from the batch
/// being committed, which is why the write set plus its commit cursor is enough
/// to reconstruct duplicate-input receipts.
pub(crate) fn insert(
    connection: &Connection,
    input: &Input,
    commit_seq: CommitSeq,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO inputs
           (id, conversation_id, request_key, sender, mode, body, disposition, commit_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            input.id.get(),
            input.target.get(),
            input.request_key.as_ref().map(RequestKey::as_str),
            json_to(&input.sender)?,
            json_to(&input.mode)?,
            json_to(&input.body)?,
            json_to(&input.disposition)?,
            commit_seq.get(),
        ],
    )?;
    Ok(())
}

pub(crate) fn set_disposition(
    connection: &Connection,
    input_id: InputId,
    disposition: InputDisposition,
) -> Result<(), StoreError> {
    connection.execute(
        "UPDATE inputs SET disposition = ?2 WHERE id = ?1",
        params![input_id.get(), json_to(&disposition)?],
    )?;
    Ok(())
}

/// Load every input plus its admission commit, if it was admitted by a commit.
pub(crate) fn load(connection: &Connection) -> Result<Vec<(Input, Option<CommitSeq>)>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, conversation_id, request_key, sender, mode, body, disposition, commit_seq
         FROM inputs ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<i64>>(7)?,
        ))
    })?;

    let mut inputs = Vec::new();
    for row in rows {
        let (id, conversation_id, request_key, sender, mode, body, disposition, commit_seq) = row?;
        let request_key = request_key
            .map(|key| {
                RequestKey::new(key).map_err(|error| {
                    StoreError::other(format!("input {id} has an invalid key: {error}"))
                })
            })
            .transpose()?;
        inputs.push((
            Input {
                id: id_from(id)?,
                target: id_from(conversation_id)?,
                sender: json_from(&sender)?,
                mode: json_from(&mode)?,
                request_key,
                body: json_from(&body)?,
                disposition: json_from(&disposition)?,
            },
            commit_seq.map(id_from).transpose()?,
        ));
    }
    Ok(inputs)
}
