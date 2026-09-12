use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvocationMode {
    Execute,
    Recover,
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invocation {
    pub task_id: TaskId,
    pub generation: i64,
    pub mode: InvocationMode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Terminal { kind: String, value: Value },
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredTask {
    pub id: TaskId,
    pub kind: String,
    pub input: Value,
    pub checkpoint: Option<Value>,
    pub status: TaskStatus,
    pub generation: i64,
    pub cancel_requested: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("task {0:?} was not found")]
    Missing(TaskId),
    #[error("task {0:?} is not pending")]
    NotPending(TaskId),
    #[error("task {0:?} is not a recoverable running task")]
    NotRecoverable(TaskId),
    #[error("task {0:?} is not ready for abort cleanup")]
    NotAbortable(TaskId),
    #[error("stale task invocation")]
    StaleInvocation,
    #[error("task cancellation revoked normal mutation authority")]
    Cancelled,
    #[error("task is already terminal")]
    Terminal,
    #[error("abort mutation attempted without a durable cancellation mark")]
    AbortWithoutMark,
    #[error("invalid stored json: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct PrototypeTaskStore {
    path: PathBuf,
    connection: Connection,
}

impl PrototypeTaskStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open(&path)?;
        connection.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = FULL;

            CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                input TEXT NOT NULL,
                checkpoint TEXT,
                status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'terminal')),
                generation INTEGER NOT NULL,
                cancel_requested INTEGER NOT NULL CHECK (cancel_requested IN (0, 1)),
                outcome_kind TEXT,
                outcome TEXT,
                CHECK ((status = 'terminal') = (outcome_kind IS NOT NULL AND outcome IS NOT NULL))
            );
            "#,
        )?;
        Ok(Self { path, connection })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create_task(&mut self, kind: &str, input: &Value) -> Result<TaskId, StoreError> {
        self.connection.execute(
            "INSERT INTO tasks
             (kind, input, checkpoint, status, generation, cancel_requested, outcome_kind, outcome)
             VALUES (?1, ?2, NULL, 'pending', 0, 0, NULL, NULL)",
            params![kind, serde_json::to_string(input)?],
        )?;
        Ok(TaskId(self.connection.last_insert_rowid()))
    }

    pub fn task(&self, task_id: TaskId) -> Result<StoredTask, StoreError> {
        let row = self
            .connection
            .query_row(
                "SELECT kind, input, checkpoint, status, generation, cancel_requested,
                        outcome_kind, outcome
                 FROM tasks WHERE id = ?1",
                [task_id.0],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, bool>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::Missing(task_id))?;
        let (kind, input, checkpoint, status, generation, cancel_requested, outcome_kind, outcome) =
            row;
        let status = match status.as_str() {
            "pending" => TaskStatus::Pending,
            "running" => TaskStatus::Running,
            "terminal" => TaskStatus::Terminal {
                kind: outcome_kind.expect("terminal row has outcome kind"),
                value: serde_json::from_str(&outcome.expect("terminal row has outcome"))?,
            },
            other => unreachable!("schema constrained task status: {other}"),
        };
        Ok(StoredTask {
            id: task_id,
            kind,
            input: serde_json::from_str(&input)?,
            checkpoint: checkpoint
                .as_deref()
                .map(serde_json::from_str)
                .transpose()?,
            status,
            generation,
            cancel_requested,
        })
    }

    pub fn reserve_execute(&mut self, task_id: TaskId) -> Result<Invocation, StoreError> {
        let task = self.task(task_id)?;
        if task.status != TaskStatus::Pending || task.cancel_requested {
            return Err(StoreError::NotPending(task_id));
        }
        let generation = task.generation + 1;
        let changed = self.connection.execute(
            "UPDATE tasks
             SET status = 'running', generation = ?2
             WHERE id = ?1 AND status = 'pending' AND generation = ?3 AND cancel_requested = 0",
            params![task_id.0, generation, task.generation],
        )?;
        if changed != 1 {
            return Err(StoreError::StaleInvocation);
        }
        Ok(Invocation {
            task_id,
            generation,
            mode: InvocationMode::Execute,
        })
    }

    pub fn reserve_recover(&mut self, task_id: TaskId) -> Result<Invocation, StoreError> {
        let task = self.task(task_id)?;
        if task.status != TaskStatus::Running || task.cancel_requested {
            return Err(StoreError::NotRecoverable(task_id));
        }
        let generation = task.generation + 1;
        let changed = self.connection.execute(
            "UPDATE tasks SET generation = ?2
             WHERE id = ?1 AND status = 'running' AND generation = ?3 AND cancel_requested = 0",
            params![task_id.0, generation, task.generation],
        )?;
        if changed != 1 {
            return Err(StoreError::StaleInvocation);
        }
        Ok(Invocation {
            task_id,
            generation,
            mode: InvocationMode::Recover,
        })
    }

    pub fn mark_cancel(&mut self, task_id: TaskId) -> Result<bool, StoreError> {
        let task = self.task(task_id)?;
        if matches!(task.status, TaskStatus::Terminal { .. }) {
            return Ok(false);
        }
        let changed = self.connection.execute(
            "UPDATE tasks SET cancel_requested = 1
             WHERE id = ?1 AND status != 'terminal' AND cancel_requested = 0",
            [task_id.0],
        )?;
        Ok(changed == 1)
    }

    pub fn reserve_abort(&mut self, task_id: TaskId) -> Result<Invocation, StoreError> {
        let task = self.task(task_id)?;
        if task.status != TaskStatus::Running || !task.cancel_requested {
            return Err(StoreError::NotAbortable(task_id));
        }
        let generation = task.generation + 1;
        let changed = self.connection.execute(
            "UPDATE tasks SET generation = ?2
             WHERE id = ?1 AND status = 'running' AND generation = ?3 AND cancel_requested = 1",
            params![task_id.0, generation, task.generation],
        )?;
        if changed != 1 {
            return Err(StoreError::StaleInvocation);
        }
        Ok(Invocation {
            task_id,
            generation,
            mode: InvocationMode::Abort,
        })
    }

    pub fn commit_checkpoint(
        &mut self,
        invocation: Invocation,
        checkpoint: &Value,
    ) -> Result<(), StoreError> {
        self.validate_invocation(invocation)?;
        let changed = self.connection.execute(
            "UPDATE tasks SET checkpoint = ?3
             WHERE id = ?1 AND status = 'running' AND generation = ?2",
            params![
                invocation.task_id.0,
                invocation.generation,
                serde_json::to_string(checkpoint)?
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::StaleInvocation);
        }
        Ok(())
    }

    pub fn apply_terminal(
        &mut self,
        invocation: Invocation,
        final_checkpoint: Option<&Value>,
        outcome_kind: &str,
        outcome: &Value,
    ) -> Result<(), StoreError> {
        self.validate_invocation(invocation)?;
        let checkpoint = match final_checkpoint {
            Some(value) => Some(serde_json::to_string(value)?),
            None => self.connection.query_row(
                "SELECT checkpoint FROM tasks WHERE id = ?1",
                [invocation.task_id.0],
                |row| row.get::<_, Option<String>>(0),
            )?,
        };
        let changed = self.connection.execute(
            "UPDATE tasks
             SET checkpoint = ?3, status = 'terminal', outcome_kind = ?4, outcome = ?5
             WHERE id = ?1 AND status = 'running' AND generation = ?2",
            params![
                invocation.task_id.0,
                invocation.generation,
                checkpoint,
                outcome_kind,
                serde_json::to_string(outcome)?,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::StaleInvocation);
        }
        Ok(())
    }

    fn validate_invocation(&self, invocation: Invocation) -> Result<(), StoreError> {
        let task = self.task(invocation.task_id)?;
        if matches!(task.status, TaskStatus::Terminal { .. }) {
            return Err(StoreError::Terminal);
        }
        if task.status != TaskStatus::Running || task.generation != invocation.generation {
            return Err(StoreError::StaleInvocation);
        }
        match invocation.mode {
            InvocationMode::Execute | InvocationMode::Recover if task.cancel_requested => {
                Err(StoreError::Cancelled)
            }
            InvocationMode::Abort if !task.cancel_requested => Err(StoreError::AbortWithoutMark),
            InvocationMode::Execute | InvocationMode::Recover | InvocationMode::Abort => Ok(()),
        }
    }
}
