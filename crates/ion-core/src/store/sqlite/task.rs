//! Task rows plus their dependency and owned-conversation child tables.
//!
//! Task identity, lifecycle state and the indexed discriminators live in
//! columns; checkpoint/output payloads stay as JSON. Reconstructing a task never
//! needs another table's payloads, and loading a session reads the child tables
//! once rather than per task.

use std::collections::HashMap;

use rusqlite::{Connection, params};
use serde_json::Value;

use super::{StoreError, id_from, json_from, json_to, sql_int};
use crate::{
    ConversationId, InvocationKind, TaskId, TaskInvocation, TaskOutcome, TaskOutput, TaskRecord,
    TaskStatus,
};

fn state_of(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Running => "running",
        TaskStatus::Terminal(_) => "terminal",
    }
}

pub(crate) fn insert(connection: &Connection, task: &TaskRecord) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO tasks (id, conversation_id, kind, schema_version, input, checkpoint, turn,
                            generation, invocation, cancel_requested, state, outcome, output)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            task.id.get(),
            task.conversation_id.get(),
            task.kind.as_str(),
            task.schema_version,
            json_to(&task.input)?,
            task.checkpoint.as_ref().map(json_to).transpose()?,
            task.turn.map(TaskId::get),
            sql_int(task.generation)?,
            task.invocation.as_ref().map(json_to).transpose()?,
            task.cancel_requested,
            state_of(&task.status),
            match &task.status {
                TaskStatus::Terminal(outcome) => Some(json_to(outcome)?),
                TaskStatus::Pending | TaskStatus::Running => None,
            },
            task.output.as_ref().map(json_to).transpose()?,
        ],
    )?;
    for (position, dependency) in task.dependencies.iter().enumerate() {
        connection.execute(
            "INSERT INTO task_dependencies (task_id, position, depends_on) VALUES (?1, ?2, ?3)",
            params![task.id.get(), position as i64, dependency.get()],
        )?;
    }
    for (position, conversation_id) in task.owned_conversations.iter().enumerate() {
        connection.execute(
            "INSERT INTO task_ownership (task_id, position, conversation_id) VALUES (?1, ?2, ?3)",
            params![task.id.get(), position as i64, conversation_id.get()],
        )?;
    }
    Ok(())
}

/// Append one owned conversation to a task's list. The conversation's own
/// `owner_task` column was written by the conversation insert in the same batch.
pub(crate) fn attach_owned_conversation(
    connection: &Connection,
    task_id: TaskId,
    conversation_id: ConversationId,
) -> Result<(), StoreError> {
    let position: i64 = connection.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM task_ownership WHERE task_id = ?1",
        [task_id.get()],
        |row| row.get(0),
    )?;
    connection.execute(
        "INSERT INTO task_ownership (task_id, position, conversation_id) VALUES (?1, ?2, ?3)",
        params![task_id.get(), position, conversation_id.get()],
    )?;
    Ok(())
}

/// Record the member whose settlement closed a turn on the turn's root row.
pub(crate) fn close_turn(
    connection: &Connection,
    root: TaskId,
    closed_by: TaskId,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE tasks SET turn_closed_by = ?2 WHERE id = ?1 AND turn = id AND turn_closed_by IS NULL",
        params![root.get(), closed_by.get()],
    )?;
    if updated != 1 {
        return Err(StoreError::other(format!(
            "task {root} was not an open turn root"
        )));
    }
    Ok(())
}

/// Record a reserved invocation generation.
pub(crate) fn reserve(
    connection: &Connection,
    task_id: TaskId,
    generation: u64,
    kind: InvocationKind,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE tasks SET generation = ?2, invocation = ?3, state = 'running'
         WHERE id = ?1",
        params![
            task_id.get(),
            sql_int(generation)?,
            json_to(&TaskInvocation { generation, kind })?,
        ],
    )?;
    if updated != 1 {
        return Err(StoreError::other(format!(
            "task {task_id} was not reserved"
        )));
    }
    Ok(())
}

/// Replace the checkpoint/output pair of a running invocation. `None` clears the
/// stored payload, matching the resident apply.
pub(crate) fn checkpoint(
    connection: &Connection,
    task_id: TaskId,
    generation: u64,
    checkpoint: Option<&Value>,
    output: Option<&TaskOutput>,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE tasks SET checkpoint = ?3, output = ?4 WHERE id = ?1 AND generation = ?2",
        params![
            task_id.get(),
            sql_int(generation)?,
            checkpoint.map(json_to).transpose()?,
            output.map(json_to).transpose()?,
        ],
    )?;
    if updated != 1 {
        return Err(StoreError::other(format!(
            "task {task_id} generation {generation} is not the stored invocation"
        )));
    }
    Ok(())
}

pub(crate) fn mark_cancellation(
    connection: &Connection,
    task_id: TaskId,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE tasks SET cancel_requested = 1 WHERE id = ?1",
        [task_id.get()],
    )?;
    if updated != 1 {
        return Err(StoreError::other(format!("task {task_id} was not found")));
    }
    Ok(())
}

pub(crate) fn settle(
    connection: &Connection,
    task_id: TaskId,
    generation: u64,
    outcome: &TaskOutcome,
    output: Option<&TaskOutput>,
) -> Result<(), StoreError> {
    let updated = connection.execute(
        "UPDATE tasks SET state = 'terminal', outcome = ?3, output = ?4, invocation = NULL
         WHERE id = ?1 AND generation = ?2",
        params![
            task_id.get(),
            sql_int(generation)?,
            json_to(outcome)?,
            output.map(json_to).transpose()?,
        ],
    )?;
    if updated != 1 {
        return Err(StoreError::other(format!(
            "task {task_id} generation {generation} is not the stored invocation"
        )));
    }
    Ok(())
}

/// Column order of the task `SELECT` below.
type LoadedTask = (
    i64,
    i64,
    String,
    u32,
    String,
    Option<String>,
    Option<i64>,
    Option<i64>,
    i64,
    Option<String>,
    bool,
    String,
    Option<String>,
    Option<String>,
);

pub(crate) fn load(connection: &Connection) -> Result<Vec<TaskRecord>, StoreError> {
    let dependencies = child_rows(
        connection,
        "SELECT task_id, depends_on FROM task_dependencies ORDER BY task_id, position",
    )?;
    let ownership = child_rows(
        connection,
        "SELECT task_id, conversation_id FROM task_ownership ORDER BY task_id, position",
    )?;

    let mut statement = connection.prepare(
        "SELECT id, conversation_id, kind, schema_version, input, checkpoint, turn,
                turn_closed_by, generation, invocation, cancel_requested, state, outcome, output
         FROM tasks ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| -> rusqlite::Result<LoadedTask> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
            row.get(9)?,
            row.get(10)?,
            row.get(11)?,
            row.get(12)?,
            row.get(13)?,
        ))
    })?;

    let mut tasks = Vec::new();
    for row in rows {
        let (
            id,
            conversation_id,
            kind,
            schema_version,
            input,
            checkpoint,
            turn,
            turn_closed_by,
            generation,
            invocation,
            cancel_requested,
            state,
            outcome,
            output,
        ) = row?;
        let task_id: TaskId = id_from(id)?;
        let status = match state.as_str() {
            "pending" => TaskStatus::Pending,
            "running" => TaskStatus::Running,
            "terminal" => {
                TaskStatus::Terminal(json_from(outcome.as_deref().ok_or_else(|| {
                    StoreError::other(format!("task {id} is terminal without an outcome"))
                })?)?)
            }
            other => {
                return Err(StoreError::other(format!(
                    "task {id} has unknown lifecycle state {other:?}"
                )));
            }
        };
        tasks.push(TaskRecord {
            id: task_id,
            conversation_id: id_from(conversation_id)?,
            kind: crate::TaskKindName::new(kind).map_err(|error| {
                StoreError::other(format!("task {id} has an invalid kind: {error}"))
            })?,
            schema_version,
            input: json_from(&input)?,
            checkpoint: checkpoint.as_deref().map(json_from).transpose()?,
            dependencies: dependencies
                .get(&task_id)
                .map(|values| values.iter().copied().map(id_from).collect())
                .transpose()?
                .unwrap_or_default(),
            owned_conversations: ownership
                .get(&task_id)
                .map(|values| values.iter().copied().map(id_from).collect())
                .transpose()?
                .unwrap_or_default(),
            turn: turn.map(id_from).transpose()?,
            turn_closed_by: turn_closed_by.map(id_from).transpose()?,
            generation: u64::try_from(generation)
                .map_err(|_| StoreError::other(format!("task {id} has a negative generation")))?,
            invocation: invocation.as_deref().map(json_from).transpose()?,
            cancel_requested,
            status,
            output: output.as_deref().map(json_from).transpose()?,
        });
    }
    Ok(tasks)
}

fn child_rows(connection: &Connection, sql: &str) -> Result<HashMap<TaskId, Vec<i64>>, StoreError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?;
    let mut grouped: HashMap<TaskId, Vec<i64>> = HashMap::new();
    for row in rows {
        let (owner, value) = row?;
        grouped.entry(id_from(owner)?).or_default().push(value);
    }
    Ok(grouped)
}
