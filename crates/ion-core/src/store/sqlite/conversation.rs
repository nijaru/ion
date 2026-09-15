//! Conversation rows. Conversations are small and fully normalized, so they are
//! reconstructed from columns rather than a payload blob.

use rusqlite::{Connection, params};

use super::{StoreError, id_from, json_from, json_to};
use crate::conversation::InstalledConfig;
use crate::{CommitSeq, Conversation, ConversationConfig, ConversationId, HistoryParent, TaskId};

pub(crate) fn insert(
    connection: &Connection,
    conversation: &Conversation,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO conversations
           (id, parent_id, parent_at, owner_task, foreground_turn, turn_cancelled, retired)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            conversation.id.get(),
            conversation
                .parent
                .map(|parent| parent.conversation_id.get()),
            conversation.parent.map(|parent| parent.at.get()),
            conversation.owner_task.map(TaskId::get),
            conversation.foreground_turn.map(TaskId::get),
            conversation.turn_cancelled,
            conversation.retired,
        ],
    )?;
    Ok(())
}

/// Set or clear the conversation's foreground slot.
///
/// `task_id` is the root that currently holds the slot: it is written when a
/// turn opens and cleared when the turn has no remaining non-terminal member.
/// Clearing the slot also clears the turn's cancellation barrier, because the
/// turn it belonged to is over. The conditional update is a safety net;
/// semantic validation already happened against the resident draft.
pub(crate) fn set_foreground_turn(
    connection: &Connection,
    conversation_id: ConversationId,
    task_id: Option<TaskId>,
    expected: Option<TaskId>,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE conversations
         SET foreground_turn = ?2,
             turn_cancelled = (CASE WHEN ?2 IS NULL THEN 0 ELSE turn_cancelled END)
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

/// Mark the turn holding this conversation's slot as cancelled.
pub(crate) fn set_turn_cancelled(
    connection: &Connection,
    conversation_id: ConversationId,
    root: TaskId,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE conversations SET turn_cancelled = 1
         WHERE id = ?1 AND foreground_turn = ?2",
        params![conversation_id.get(), root.get()],
    )?;
    if updated != 1 {
        return Err(StoreError::other(format!(
            "conversation {conversation_id} was not holding turn {root}"
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

/// Install a conversation's configuration and the commit that installed it.
///
/// Both columns move together: a configuration without its revision could not be
/// fenced by a replacement, and a revision without its configuration would claim
/// a change nobody can read. The revision is the batch's own commit sequence, so
/// the durable value cannot disagree with the commit that wrote it.
pub(crate) fn set_config(
    connection: &Connection,
    conversation_id: ConversationId,
    config: &ConversationConfig,
    revision: CommitSeq,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE conversations SET config = ?2, config_revision = ?3 WHERE id = ?1",
        params![conversation_id.get(), json_to(config)?, revision.get()],
    )?;
    if updated != 1 {
        return Err(StoreError::other(format!(
            "conversation {conversation_id} configuration did not match the write set"
        )));
    }
    Ok(())
}

/// Every conversation with its installed configuration, if it has one.
///
/// The two configuration columns are read together and must agree: a row that
/// carries only one of them was not written by a valid commit, so it is refused
/// rather than read as a half-configured conversation.
pub(crate) fn load(
    connection: &Connection,
) -> Result<Vec<(Conversation, Option<InstalledConfig>)>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT id, parent_id, parent_at, owner_task, foreground_turn, turn_cancelled, retired,
                config, config_revision
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
            row.get::<_, bool>(6)?,
            row.get::<_, Option<String>>(7)?,
            row.get::<_, Option<i64>>(8)?,
        ))
    })?;

    let mut conversations = Vec::new();
    for row in rows {
        let (
            id,
            parent_id,
            parent_at,
            owner_task,
            foreground_turn,
            turn_cancelled,
            retired,
            config,
            config_revision,
        ) = row?;
        let config = match (config, config_revision) {
            (Some(config), Some(revision)) => Some(InstalledConfig::new(
                id_from(revision)?,
                json_from::<ConversationConfig>(&config)?,
            )),
            (None, None) => None,
            _ => {
                return Err(StoreError::other(format!(
                    "conversation {id} has a partial configuration"
                )));
            }
        };
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
        conversations.push((
            Conversation {
                id: id_from(id)?,
                parent,
                owner_task: owner_task.map(id_from).transpose()?,
                foreground_turn: foreground_turn.map(id_from).transpose()?,
                turn_cancelled,
                retired,
            },
            config,
        ));
    }
    Ok(conversations)
}
