use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InputId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EffectId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionMode {
    Prompt,
    SpawnWorker,
}

impl AdmissionMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::SpawnWorker => "spawn_worker",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub input_id: InputId,
    pub task_id: TaskId,
    pub created_agent: Option<AgentId>,
    pub worker_task: Option<TaskId>,
}

#[derive(Debug)]
pub enum AdmissionError {
    Conflict,
    Sql(rusqlite::Error),
}

impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict => write!(f, "request key conflicts with the original admission"),
            Self::Sql(error) => write!(f, "sqlite: {error}"),
        }
    }
}

impl std::error::Error for AdmissionError {}

impl From<rusqlite::Error> for AdmissionError {
    fn from(value: rusqlite::Error) -> Self {
        Self::Sql(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recovery {
    ReplaySafe,
    NeverReplay,
}

impl Recovery {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ReplaySafe => "replay_safe",
            Self::NeverReplay => "never_replay",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectSpec {
    pub ordinal: i64,
    pub name: &'static str,
    pub recovery: Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDecision {
    Retry { effect_id: EffectId, attempt: i64 },
    Indeterminate { effect_id: EffectId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    Idle,
    Running,
    Finished(String),
}

pub struct PrototypeStore {
    path: PathBuf,
    connection: Connection,
}

impl PrototypeStore {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open(&path)?;
        connection.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = FULL;

            CREATE TABLE IF NOT EXISTS meta (
                key TEXT PRIMARY KEY,
                value INTEGER NOT NULL
            );
            INSERT OR IGNORE INTO meta (key, value) VALUES ('next_id', 2);
            INSERT OR IGNORE INTO meta (key, value) VALUES ('commit_seq', 0);

            CREATE TABLE IF NOT EXISTS agents (
                id INTEGER PRIMARY KEY,
                parent_id INTEGER REFERENCES agents(id),
                retained INTEGER NOT NULL CHECK (retained IN (0, 1))
            );
            INSERT OR IGNORE INTO agents (id, parent_id, retained) VALUES (1, NULL, 1);

            CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY,
                agent_id INTEGER NOT NULL REFERENCES agents(id),
                state TEXT NOT NULL,
                generation INTEGER NOT NULL,
                cancel_requested INTEGER NOT NULL CHECK (cancel_requested IN (0, 1)),
                terminal TEXT,
                checkpoint TEXT NOT NULL DEFAULT 'null'
            );

            CREATE TABLE IF NOT EXISTS inputs (
                request_key TEXT PRIMARY KEY,
                target_agent INTEGER NOT NULL REFERENCES agents(id),
                content TEXT NOT NULL,
                mode TEXT NOT NULL,
                input_id INTEGER NOT NULL UNIQUE,
                task_id INTEGER NOT NULL REFERENCES tasks(id),
                created_agent INTEGER REFERENCES agents(id),
                worker_task INTEGER REFERENCES tasks(id)
            );

            CREATE TABLE IF NOT EXISTS effects (
                id INTEGER PRIMARY KEY,
                task_id INTEGER NOT NULL REFERENCES tasks(id),
                ordinal INTEGER NOT NULL,
                name TEXT NOT NULL,
                recovery TEXT NOT NULL,
                status TEXT NOT NULL,
                attempt INTEGER NOT NULL,
                generation INTEGER NOT NULL,
                result TEXT,
                settled_seq INTEGER,
                UNIQUE (task_id, ordinal)
            );

            CREATE TABLE IF NOT EXISTS outputs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id INTEGER NOT NULL REFERENCES tasks(id),
                generation INTEGER NOT NULL,
                text TEXT NOT NULL
            );
            "#,
        )?;
        Ok(Self { path, connection })
    }

    pub const fn root(&self) -> AgentId {
        AgentId(1)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn admit(
        &mut self,
        request_key: &str,
        target: AgentId,
        content: &str,
        mode: AdmissionMode,
    ) -> Result<Receipt, AdmissionError> {
        let transaction = self.connection.transaction()?;
        if let Some((stored_target, stored_content, stored_mode, receipt)) =
            Self::receipt_for_key(&transaction, request_key)?
        {
            if stored_target != target.0
                || stored_content != content
                || stored_mode != mode.as_str()
            {
                return Err(AdmissionError::Conflict);
            }
            transaction.commit()?;
            return Ok(receipt);
        }

        let input_id = InputId(Self::allocate_id(&transaction)?);
        let task_id = TaskId(Self::allocate_id(&transaction)?);
        let (created_agent, worker_task, terminal) = match mode {
            AdmissionMode::Prompt => (None, None, None),
            AdmissionMode::SpawnWorker => {
                let agent_id = AgentId(Self::allocate_id(&transaction)?);
                let worker_task_id = TaskId(Self::allocate_id(&transaction)?);
                transaction.execute(
                    "INSERT INTO agents (id, parent_id, retained) VALUES (?1, ?2, 1)",
                    params![agent_id.0, target.0],
                )?;
                transaction.execute(
                    "INSERT INTO tasks
                     (id, agent_id, state, generation, cancel_requested, terminal)
                     VALUES (?1, ?2, 'accepted', 1, 0, NULL)",
                    params![worker_task_id.0, agent_id.0],
                )?;
                (Some(agent_id), Some(worker_task_id), Some("completed"))
            }
        };
        transaction.execute(
            "INSERT INTO tasks
             (id, agent_id, state, generation, cancel_requested, terminal)
             VALUES (?1, ?2, ?3, 1, 0, ?4)",
            params![
                task_id.0,
                target.0,
                if terminal.is_some() { "terminal" } else { "accepted" },
                terminal,
            ],
        )?;
        transaction.execute(
            "INSERT INTO inputs
             (request_key, target_agent, content, mode, input_id, task_id, created_agent, worker_task)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                request_key,
                target.0,
                content,
                mode.as_str(),
                input_id.0,
                task_id.0,
                created_agent.map(|id| id.0),
                worker_task.map(|id| id.0),
            ],
        )?;
        transaction.commit()?;
        Ok(Receipt {
            input_id,
            task_id,
            created_agent,
            worker_task,
        })
    }

    fn receipt_for_key(
        transaction: &Transaction<'_>,
        request_key: &str,
    ) -> rusqlite::Result<Option<(i64, String, String, Receipt)>> {
        transaction
            .query_row(
                "SELECT target_agent, content, mode, input_id, task_id, created_agent, worker_task
                 FROM inputs WHERE request_key = ?1",
                [request_key],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        Receipt {
                            input_id: InputId(row.get(3)?),
                            task_id: TaskId(row.get(4)?),
                            created_agent: row.get::<_, Option<i64>>(5)?.map(AgentId),
                            worker_task: row.get::<_, Option<i64>>(6)?.map(TaskId),
                        },
                    ))
                },
            )
            .optional()
    }

    fn allocate_id(transaction: &Transaction<'_>) -> rusqlite::Result<i64> {
        let id = transaction.query_row(
            "SELECT value FROM meta WHERE key = 'next_id'",
            [],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE meta SET value = value + 1 WHERE key = 'next_id'",
            [],
        )?;
        Ok(id)
    }

    fn next_commit_seq(transaction: &Transaction<'_>) -> rusqlite::Result<i64> {
        transaction.execute(
            "UPDATE meta SET value = value + 1 WHERE key = 'commit_seq'",
            [],
        )?;
        transaction.query_row(
            "SELECT value FROM meta WHERE key = 'commit_seq'",
            [],
            |row| row.get(0),
        )
    }

    pub fn start_task(&mut self, task_id: TaskId) -> rusqlite::Result<i64> {
        let transaction = self.connection.transaction()?;
        let generation: i64 = transaction.query_row(
            "SELECT generation FROM tasks WHERE id = ?1 AND state = 'accepted'",
            [task_id.0],
            |row| row.get(0),
        )?;
        transaction.execute(
            "UPDATE tasks SET state = 'running' WHERE id = ?1",
            [task_id.0],
        )?;
        transaction.commit()?;
        Ok(generation)
    }

    pub fn set_checkpoint<T: Serialize>(
        &mut self,
        task_id: TaskId,
        checkpoint: &T,
    ) -> rusqlite::Result<()> {
        let payload = serde_json::to_string(checkpoint)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        self.connection.execute(
            "UPDATE tasks SET checkpoint = ?2 WHERE id = ?1",
            params![task_id.0, payload],
        )?;
        Ok(())
    }

    pub fn checkpoint<T: for<'de> Deserialize<'de>>(
        &self,
        task_id: TaskId,
    ) -> rusqlite::Result<T> {
        let payload: String = self.connection.query_row(
            "SELECT checkpoint FROM tasks WHERE id = ?1",
            [task_id.0],
            |row| row.get(0),
        )?;
        serde_json::from_str(&payload).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
    }

    pub fn open_effects(
        &mut self,
        task_id: TaskId,
        specs: &[EffectSpec],
    ) -> rusqlite::Result<Vec<EffectId>> {
        let transaction = self.connection.transaction()?;
        let generation: i64 = transaction.query_row(
            "SELECT generation FROM tasks WHERE id = ?1 AND state = 'running'",
            [task_id.0],
            |row| row.get(0),
        )?;
        let mut effects = Vec::with_capacity(specs.len());
        for spec in specs {
            let effect_id = EffectId(Self::allocate_id(&transaction)?);
            transaction.execute(
                "INSERT INTO effects
                 (id, task_id, ordinal, name, recovery, status, attempt, generation)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'running', 1, ?6)",
                params![
                    effect_id.0,
                    task_id.0,
                    spec.ordinal,
                    spec.name,
                    spec.recovery.as_str(),
                    generation,
                ],
            )?;
            effects.push(effect_id);
        }
        transaction.commit()?;
        Ok(effects)
    }

    pub fn settle_effect(
        &mut self,
        effect_id: EffectId,
        result: &str,
    ) -> rusqlite::Result<bool> {
        let transaction = self.connection.transaction()?;
        let current: Option<(i64, i64)> = transaction
            .query_row(
                "SELECT e.generation, t.generation
                 FROM effects e JOIN tasks t ON t.id = e.task_id
                 WHERE e.id = ?1 AND e.status = 'running' AND t.state = 'running'",
                [effect_id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if !matches!(current, Some((effect_generation, task_generation)) if effect_generation == task_generation)
        {
            transaction.commit()?;
            return Ok(false);
        }
        let settled_seq = Self::next_commit_seq(&transaction)?;
        transaction.execute(
            "UPDATE effects SET status = 'settled', result = ?2, settled_seq = ?3 WHERE id = ?1",
            params![effect_id.0, result, settled_seq],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn effect_result(&self, effect_id: EffectId) -> rusqlite::Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT result FROM effects WHERE id = ?1 AND status = 'settled'",
                [effect_id.0],
                |row| row.get(0),
            )
            .optional()
            .map(Option::flatten)
    }

    pub fn results_in_call_order(
        &self,
        task_id: TaskId,
    ) -> rusqlite::Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT name, result FROM effects
             WHERE task_id = ?1 AND status = 'settled' ORDER BY ordinal",
        )?;
        statement
            .query_map([task_id.0], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
    }

    pub fn settlement_order(&self, task_id: TaskId) -> rusqlite::Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT name FROM effects
             WHERE task_id = ?1 AND settled_seq IS NOT NULL ORDER BY settled_seq",
        )?;
        statement
            .query_map([task_id.0], |row| row.get(0))?
            .collect()
    }

    pub fn append_output(
        &self,
        task_id: TaskId,
        generation: i64,
        text: &str,
    ) -> rusqlite::Result<bool> {
        let changed = self.connection.execute(
            "INSERT INTO outputs (task_id, generation, text)
             SELECT id, generation, ?3 FROM tasks
             WHERE id = ?1 AND generation = ?2 AND state = 'running'",
            params![task_id.0, generation, text],
        )?;
        Ok(changed == 1)
    }

    pub fn finish_task(
        &self,
        task_id: TaskId,
        generation: i64,
        outcome: &str,
    ) -> rusqlite::Result<bool> {
        let changed = self.connection.execute(
            "UPDATE tasks SET state = 'terminal', terminal = ?3
             WHERE id = ?1 AND generation = ?2 AND state = 'running'",
            params![task_id.0, generation, outcome],
        )?;
        Ok(changed == 1)
    }

    pub fn cancel_task(&mut self, task_id: TaskId) -> rusqlite::Result<Option<i64>> {
        let transaction = self.connection.transaction()?;
        let current: Option<(String, i64)> = transaction
            .query_row(
                "SELECT state, generation FROM tasks WHERE id = ?1 AND terminal IS NULL",
                [task_id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state, generation)) = current else {
            transaction.commit()?;
            return Ok(None);
        };
        if state != "running" && state != "accepted" {
            transaction.commit()?;
            return Ok(None);
        }
        let next_generation = generation + 1;
        transaction.execute(
            "UPDATE tasks
             SET state = 'cancelling', generation = ?2, cancel_requested = 1
             WHERE id = ?1",
            params![task_id.0, next_generation],
        )?;
        transaction.commit()?;
        Ok(Some(next_generation))
    }

    pub fn finish_cancel(&self, task_id: TaskId, generation: i64) -> rusqlite::Result<bool> {
        let changed = self.connection.execute(
            "UPDATE tasks SET state = 'terminal', terminal = 'cancelled'
             WHERE id = ?1 AND generation = ?2 AND state = 'cancelling'",
            params![task_id.0, generation],
        )?;
        Ok(changed == 1)
    }

    pub fn terminal(&self, task_id: TaskId) -> rusqlite::Result<Option<String>> {
        self.connection.query_row(
            "SELECT terminal FROM tasks WHERE id = ?1",
            [task_id.0],
            |row| row.get(0),
        )
    }

    pub fn recover_pending(&mut self) -> rusqlite::Result<Vec<RecoveryDecision>> {
        let pending = {
            let mut statement = self.connection.prepare(
                "SELECT id, task_id, recovery, attempt FROM effects
                 WHERE status = 'running' ORDER BY id",
            )?;
            statement
                .query_map([], |row| {
                    Ok((
                        EffectId(row.get(0)?),
                        TaskId(row.get(1)?),
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        let transaction = self.connection.transaction()?;
        let mut decisions = Vec::with_capacity(pending.len());
        for (effect_id, task_id, recovery, attempt) in pending {
            if recovery == "replay_safe" {
                let next_attempt = attempt + 1;
                transaction.execute(
                    "UPDATE effects SET attempt = ?2 WHERE id = ?1",
                    params![effect_id.0, next_attempt],
                )?;
                decisions.push(RecoveryDecision::Retry {
                    effect_id,
                    attempt: next_attempt,
                });
            } else {
                transaction.execute(
                    "UPDATE effects SET status = 'indeterminate' WHERE id = ?1",
                    [effect_id.0],
                )?;
                transaction.execute(
                    "UPDATE tasks SET state = 'terminal', terminal = 'indeterminate'
                     WHERE id = ?1",
                    [task_id.0],
                )?;
                decisions.push(RecoveryDecision::Indeterminate { effect_id });
            }
        }
        transaction.commit()?;
        Ok(decisions)
    }

    pub fn agent_status(&self, agent_id: AgentId) -> rusqlite::Result<AgentStatus> {
        let row: Option<(String, Option<String>)> = self
            .connection
            .query_row(
                "SELECT state, terminal FROM tasks
                 WHERE agent_id = ?1 ORDER BY id DESC LIMIT 1",
                [agent_id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(match row {
            None => AgentStatus::Idle,
            Some((_, Some(terminal))) => AgentStatus::Finished(terminal),
            Some(_) => AgentStatus::Running,
        })
    }

    pub fn group_summary(&self) -> rusqlite::Result<Vec<(AgentId, AgentStatus)>> {
        let agents = {
            let mut statement = self.connection.prepare("SELECT id FROM agents ORDER BY id")?;
            statement
                .query_map([], |row| Ok(AgentId(row.get(0)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        agents
            .into_iter()
            .map(|agent_id| self.agent_status(agent_id).map(|status| (agent_id, status)))
            .collect()
    }

    pub fn effect_count(&self) -> rusqlite::Result<i64> {
        self.connection
            .query_row("SELECT COUNT(*) FROM effects", [], |row| row.get(0))
    }
}
