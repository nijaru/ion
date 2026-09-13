//! Conversation rows. Conversations are small and fully normalized, so they are
//! reconstructed from columns rather than a payload blob.

use rusqlite::{Connection, params};

use super::{StoreError, id_from};
use crate::{Conversation, ConversationId, HistoryParent, TaskId};

pub(crate) fn insert(
    connection: &Connection,
    conversation: &Conversation,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO conversations
           (id, parent_id, parent_at, owner_task, foreground_turn, retired)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            conversation.id.get(),
            conversation
                .parent
                .map(|parent| parent.conversation_id.get()),
            conversation.parent.map(|parent| parent.at.get()),
            conversation.owner_task.map(TaskId::get),
            conversation.foreground_turn.map(TaskId::get),
            conversation.retired,
        ],
    )?;
    Ok(())
}

/// Set or clear the conversation's foreground slot.
///
/// `task_id` is the root that currently holds the slot: it is written when a
/// turn opens and cleared when the turn has no remaining non-terminal member.
/// The conditional update is a safety net; semantic validation already happened
/// against the resident draft.
pub(crate) fn set_foreground_turn(
    connection: &Connection,
    conversation_id: ConversationId,
    task_id: Option<TaskId>,
    expected: Option<TaskId>,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE conversations SET foreground_turn = ?2
         WHERE id = ?1 AND foreground_turn IS ?3",
        params![
            conversation_id.get(),
            task_id.map(TaskId::get),
            expected.map(TaskId::get),
        ],
    )?;
    if updated != 1 {
        return Err(StoreError::other(format!(
            "conversation {conversation_id} foreground slot did not match the write set"
        )));
    }
    Ok(())
}

pub(crate) fn set_retired(
    connection: &Connection,
    conversation_id: ConversationId,
    retired: bool,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE conversations SET retired = ?2 WHERE id = ?1",
        params![conversation_id.get(), retired],
    )?;
    if updated != 1 {
        return Err(StoreError::other(format!(
            "conversation {conversation_id} retirement did not match the write set"
        )));
    }
    Ok(())
}

pub(crate) fn load(connection: &Connection) -> Result<Vec<Conversation>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, parent_id, parent_at, owner_task, foreground_turn, retired
         FROM conversations ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
            row.get::<_, bool>(5)?,
        ))
    })?;

    let mut conversations = Vec::new();
    for row in rows {
        let (id, parent_id, parent_at, owner_task, foreground_turn, retired) = row?;
        let parent = match (parent_id, parent_at) {
            (Some(conversation_id), Some(at)) => Some(HistoryParent {
                conversation_id: id_from(conversation_id)?,
                at: id_from(at)?,
            }),
            (None, None) => None,
            _ => {
                return Err(StoreError::other(format!(
                    "conversation {id} has a partial history parent"
                )));
            }
        };
        conversations.push(Conversation {
            id: id_from(id)?,
            parent,
            owner_task: owner_task.map(id_from).transpose()?,
            foreground_turn: foreground_turn.map(id_from).transpose()?,
            retired,
        });
    }
    Ok(conversations)
}
