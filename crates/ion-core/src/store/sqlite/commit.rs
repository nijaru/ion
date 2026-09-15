//! Application of one atomic semantic mutation batch.
//!
//! The whole batch, the metadata compare-and-set and the commit cursor advance
//! inside one SQLite transaction. Nothing here re-decides readiness: the batch
//! was validated against the resident draft, and every statement here is the
//! durable expression of a mutation that batch already committed to.

use rusqlite::Connection;

use super::{StoreError, conversation, entry, input, task};
use crate::CommitSeq;
use crate::session::transaction::{Mutation, MutationBatch};

/// Apply `batch` durably. Returns a fence error if the stored commit cursor is
/// no longer the base the batch was built against, which is how a stale writer
/// authority is rejected instead of silently interleaving.
pub(crate) fn apply(connection: &mut Connection, batch: &MutationBatch) -> Result<(), StoreError> {
    let transaction = connection.transaction()?;
    let advanced = transaction.execute(
        "UPDATE session_meta SET last_seq = ?1, last_commit = ?2 WHERE id = 1 AND last_commit IS ?3",
        rusqlite::params![
            batch.last_seq.get(),
            batch.commit_seq.get(),
            batch.base_commit.map(CommitSeq::get),
        ],
    )?;
    if advanced != 1 {
        return Err(StoreError::other(
            "commit cursor advanced by another writer; this authority is fenced".to_owned(),
        ));
    }

    let mut root = None;
    for mutation in &batch.writes {
        match mutation {
            Mutation::CreateRoot(root_conversation) => {
                conversation::insert(&transaction, root_conversation)?;
                root = Some(root_conversation.id);
            }
            Mutation::CreateConversation(conversation) => {
                conversation::insert(&transaction, conversation)?;
            }
            Mutation::AppendEntry(item) => entry::insert(&transaction, item)?,
            Mutation::AdmitInput(item) => input::insert(&transaction, item, batch.commit_seq)?,
            Mutation::SetInputDisposition {
                input_id,
                disposition,
            } => input::set_disposition(&transaction, *input_id, *disposition)?,
            Mutation::CreateTask(item) => task::insert(&transaction, item)?,
            Mutation::OpenForegroundTurn {
                conversation_id,
                task_id,
            } => conversation::set_foreground_turn(
                &transaction,
                *conversation_id,
                Some(*task_id),
                None,
            )?,
            Mutation::ReleaseForegroundTurn {
                conversation_id,
                task_id,
            } => conversation::set_foreground_turn(
                &transaction,
                *conversation_id,
                None,
                Some(*task_id),
            )?,
            Mutation::ReserveTask {
                task_id,
                generation,
                kind,
            } => task::reserve(&transaction, *task_id, *generation, *kind)?,
            Mutation::CheckpointTask {
                task_id,
                generation,
                checkpoint,
                output,
            } => task::checkpoint(
                &transaction,
                *task_id,
                *generation,
                checkpoint.as_ref(),
                output.as_ref(),
            )?,
            Mutation::MarkTaskCancellation(task_id) => {
                task::mark_cancellation(&transaction, *task_id)?;
            }
            Mutation::MarkTurnCancelled {
                conversation_id,
                root,
            } => conversation::set_turn_cancelled(&transaction, *conversation_id, *root)?,
            Mutation::SettleTask {
                task_id,
                generation,
                outcome,
                output,
            } => task::settle(
                &transaction,
                *task_id,
                *generation,
                outcome,
                output.as_ref(),
            )?,
            Mutation::AttachOwnedConversation {
                task_id,
                conversation_id,
            } => task::attach_owned_conversation(&transaction, *task_id, *conversation_id)?,
            Mutation::SetConversationConfig {
                conversation_id,
                config,
            } => {
                conversation::set_config(&transaction, *conversation_id, config, batch.commit_seq)?
            }
            Mutation::SetConversationRetired {
                conversation_id,
                retired,
            } => conversation::set_retired(&transaction, *conversation_id, *retired)?,
            Mutation::CloseTurn { root, closed_by } => {
                task::close_turn(&transaction, *root, *closed_by)?;
            }
        }
    }

    if let Some(root) = root {
        let updated = transaction.execute(
            "UPDATE session_meta SET root_conversation = COALESCE(root_conversation, ?1)",
            [root.get()],
        )?;
        if updated != 1 {
            return Err(StoreError::other(
                "root conversation was not recorded".to_owned(),
            ));
        }
    }

    transaction.commit()?;
    Ok(())
}
