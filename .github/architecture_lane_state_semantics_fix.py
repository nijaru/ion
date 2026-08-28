from pathlib import Path
import re


def replace_once(path: str, old: str, new: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    p.write_text(text.replace(old, new))


def regex_once(path: str, pattern: str, replacement: str, label: str) -> None:
    p = Path(path)
    text = p.read_text()
    new, count = re.subn(pattern, replacement, text, count=1, flags=re.S)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    p.write_text(new)


replace_once(
    "crates/ion-core/src/session/lane.rs",
    '''/// Durable identity reserved for the next run while a lane is busy.
///
/// The IDs are provisioned when queueing is acknowledged, but no operation
/// row or semantic entry exists until the lane actually accepts this run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NextRun {
    pub(crate) operation_id: OperationId,
    pub(crate) entry_id: EntryId,
    pub(crate) prompt: String,
}

impl NextRun {
    pub(crate) fn reserve(prompt: String) -> Self {
        Self {
            operation_id: OperationId::generate(),
            entry_id: EntryId::generate(),
            prompt,
        }
    }
}
''',
    '''/// Semantic input reserved for the next run while a lane is busy.
///
/// The entry identity is provisioned when queueing is acknowledged. There is
/// deliberately no operation identity until the lane actually accepts the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NextRun {
    pub(crate) entry_id: EntryId,
    pub(crate) prompt: String,
}

impl NextRun {
    pub(crate) fn reserve(prompt: String) -> Self {
        Self {
            entry_id: EntryId::generate(),
            prompt,
        }
    }
}
''',
    "next-run identity",
)

replace_once(
    "crates/ion-core/src/store/schema.rs",
    '''    current_operation_id TEXT,
    pending_operation_id TEXT,
    pending_entry_id TEXT,
    pending_prompt TEXT,
''',
    '''    current_operation_id TEXT,
    pending_entry_id TEXT,
    pending_prompt TEXT,
''',
    "lane pending columns",
)
replace_once(
    "crates/ion-core/src/store/schema.rs",
    '''    CHECK (
        (pending_operation_id IS NULL AND pending_entry_id IS NULL AND pending_prompt IS NULL)
        OR
        (pending_operation_id IS NOT NULL AND pending_entry_id IS NOT NULL AND pending_prompt IS NOT NULL)
    ),
    CHECK (pending_operation_id IS NULL OR pending_operation_id != current_operation_id)
);

CREATE UNIQUE INDEX IF NOT EXISTS lanes_pending_operation_unique
    ON lanes (pending_operation_id) WHERE pending_operation_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS lanes_pending_entry_unique
''',
    '''    CHECK (
        (pending_entry_id IS NULL AND pending_prompt IS NULL)
        OR
        (pending_entry_id IS NOT NULL AND pending_prompt IS NOT NULL)
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS lanes_pending_entry_unique
''',
    "lane pending checks",
)

replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''        "INSERT INTO lanes (
            session_id, name, leaf_id, current_operation_id,
            pending_operation_id, pending_entry_id, pending_prompt,
            config, created_at, updated_at
         ) VALUES (?1, ?2, NULL, NULL, NULL, NULL, NULL, ?3, ?4, ?4)",
''',
    '''        "INSERT INTO lanes (
            session_id, name, leaf_id, current_operation_id,
            pending_entry_id, pending_prompt, config, created_at, updated_at
         ) VALUES (?1, ?2, NULL, NULL, NULL, NULL, ?3, ?4, ?4)",
''',
    "create lane pending columns",
)

regex_once(
    "crates/ion-core/src/store/sql.rs",
    r'''fn queue_main_next_run\(
.*?
\}

fn append_entry\(''',
    '''fn queue_main_next_run(
    connection: &mut Connection,
    session_id: SessionId,
    next_run: &crate::session::lane::NextRun,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let entry_id = next_run.entry_id.as_uuid().to_string();
    let entry_exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM entries WHERE id = ?1)",
        [&entry_id],
        |row| row.get(0),
    )?;
    if entry_exists {
        return Err(rusqlite::Error::InvalidParameterName(
            "reserved next-run entry identity already exists".to_owned(),
        ));
    }
    let now = now_ms();
    let changed = tx.execute(
        "UPDATE lanes SET
            pending_entry_id = ?3,
            pending_prompt = ?4,
            updated_at = ?5
         WHERE session_id = ?1 AND name = ?2
           AND current_operation_id IS NOT NULL
           AND pending_entry_id IS NULL",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            crate::session::lane::MAIN,
            entry_id,
            next_run.prompt,
            now,
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(
            "main lane cannot reserve another next run".to_owned(),
        ));
    }
    tx.execute(
        "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
        rusqlite::params![session_id.as_uuid().to_string(), now],
    )?;
    tx.commit()
}

fn append_entry(''',
    "queue next-run persistence",
)

regex_once(
    "crates/ion-core/src/store/sql.rs",
    r'''fn begin_operation\(
.*?
\}

fn commit\(''',
    '''fn begin_operation(
    connection: &mut Connection,
    session_id: SessionId,
    operation_id: OperationId,
    root_inbox: &InboxRecord,
    checkpoint: &CheckpointRecord,
    entry: &EntryRecord,
) -> Result<(), rusqlite::Error> {
    let tx = connection.transaction()?;
    let session = session_id.as_uuid().to_string();
    let operation = operation_id.as_uuid().to_string();
    let entry_id = entry.id.as_uuid().to_string();
    let (current, pending_entry, pending_prompt): (
        Option<String>,
        Option<String>,
        Option<String>,
    ) = tx.query_row(
        "SELECT current_operation_id, pending_entry_id, pending_prompt
         FROM lanes WHERE session_id = ?1 AND name = ?2",
        rusqlite::params![session, crate::session::lane::MAIN],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if current.is_some() {
        return Err(rusqlite::Error::InvalidParameterName(
            "main lane already has a current operation".to_owned(),
        ));
    }
    let user_prompt = match &entry.entry {
        SessionEntry::UserMessage { text } => text,
        _ => {
            return Err(rusqlite::Error::InvalidParameterName(
                "operation acceptance must append its user prompt".to_owned(),
            ));
        }
    };
    if user_prompt != &checkpoint.payload.prompt || root_inbox.text.as_str() != user_prompt {
        return Err(rusqlite::Error::InvalidParameterName(
            "operation prompt disagrees with its accepted entry".to_owned(),
        ));
    }
    match (pending_entry, pending_prompt) {
        (None, None) => {}
        (Some(reserved_entry), Some(reserved_prompt))
            if reserved_entry == entry_id && reserved_prompt.as_str() == user_prompt => {}
        _ => {
            return Err(rusqlite::Error::InvalidParameterName(
                "operation does not match the lane's reserved next run".to_owned(),
            ));
        }
    }

    let accepted_seq: i64 = tx.query_row(
        "SELECT COALESCE(MAX(accepted_seq), 0) + 1 FROM operations WHERE session_id = ?1",
        [&session],
        |row| row.get(0),
    )?;
    tx.execute(
        "INSERT INTO operations (id, session_id, kind, accepted_at, accepted_seq)
         VALUES (?1, ?2, 'run', ?3, ?4)",
        rusqlite::params![operation, session, now_ms(), accepted_seq],
    )?;
    insert_inbox(&tx, session_id, operation_id, root_inbox)?;
    insert_capability_snapshot(&tx, &checkpoint.capability_snapshot)?;
    insert_checkpoint(&tx, operation_id, checkpoint)?;
    insert_entry(&tx, session_id, entry)?;
    let changed = tx.execute(
        "UPDATE lanes SET
            current_operation_id = ?3,
            pending_entry_id = NULL,
            pending_prompt = NULL,
            updated_at = ?4
         WHERE session_id = ?1 AND name = ?2 AND current_operation_id IS NULL",
        rusqlite::params![
            session_id.as_uuid().to_string(),
            crate::session::lane::MAIN,
            operation_id.as_uuid().to_string(),
            now_ms(),
        ],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::InvalidParameterName(
            "main lane lost operation admission".to_owned(),
        ));
    }
    tx.commit()?;
    Ok(())
}

fn commit(''',
    "operation admission identity",
)

regex_once(
    "crates/ion-core/src/store/sql.rs",
    r'''fn load_main_lane\(
.*?
\}

fn load\(''',
    '''fn load_main_lane(
    connection: &Connection,
    session_id: SessionId,
) -> Result<crate::session::lane::Lane, StoreError> {
    struct RawLane {
        name: String,
        leaf: Option<String>,
        current_operation: Option<String>,
        pending_entry: Option<String>,
        pending_prompt: Option<String>,
        config: String,
    }

    let raw = connection
        .query_row(
            "SELECT name, leaf_id, current_operation_id,
                    pending_entry_id, pending_prompt, config
             FROM lanes WHERE session_id = ?1 AND name = ?2",
            rusqlite::params![session_id.as_uuid().to_string(), crate::session::lane::MAIN],
            |row| {
                Ok(RawLane {
                    name: row.get(0)?,
                    leaf: row.get(1)?,
                    current_operation: row.get(2)?,
                    pending_entry: row.get(3)?,
                    pending_prompt: row.get(4)?,
                    config: row.get(5)?,
                })
            },
        )
        .map_err(StoreError::from)?;
    let leaf = raw
        .leaf
        .as_deref()
        .map(|value| {
            crate::ids::EntryId::parse(value)
                .ok_or_else(|| StoreError::Sqlite("corrupt lane leaf id".to_owned()))
        })
        .transpose()?;
    let parse_operation = |value: &str| {
        Uuid::parse_str(value)
            .map(crate::ids::OperationId::from_uuid)
            .map_err(|_| StoreError::Sqlite("corrupt current operation id".to_owned()))
    };
    let current_operation = raw
        .current_operation
        .as_deref()
        .map(parse_operation)
        .transpose()?;
    let pending_next_run = match (raw.pending_entry, raw.pending_prompt) {
        (None, None) => None,
        (Some(entry), Some(prompt)) => Some(crate::session::lane::NextRun {
            entry_id: crate::ids::EntryId::parse(&entry)
                .ok_or_else(|| StoreError::Sqlite("corrupt pending entry id".to_owned()))?,
            prompt,
        }),
        _ => {
            return Err(StoreError::Sqlite(
                "partial pending next-run state".to_owned(),
            ));
        }
    };
    let config = serde_json::from_str(&raw.config)
        .map_err(|err| StoreError::Sqlite(format!("corrupt lane config: {err}")))?;
    Ok(crate::session::lane::Lane {
        name: raw.name,
        state: crate::session::lane::State {
            leaf,
            current_operation,
            pending_next_run,
        },
        config,
    })
}

fn load(''',
    "lane loader",
)

replace_once(
    "crates/ion-core/src/store/sql.rs",
    '''    if let Some(next_run) = &lane.state.pending_next_run {
        if operations.iter().any(|operation| operation.id == next_run.operation_id) {
            return Err(StoreError::Sqlite(
                "pending next-run operation identity already exists".to_owned(),
            ));
        }
        if entries.iter().any(|entry| entry.id == next_run.entry_id) {
            return Err(StoreError::Sqlite(
                "pending next-run entry identity already exists".to_owned(),
            ));
        }
    }
''',
    '''    if let Some(next_run) = &lane.state.pending_next_run
        && entries.iter().any(|entry| entry.id == next_run.entry_id)
    {
        return Err(StoreError::Sqlite(
            "pending next-run entry identity already exists".to_owned(),
        ));
    }
''',
    "lane pending validation",
)

replace_once(
    "crates/ion-core/src/error.rs",
    "use crate::ids::OperationId;",
    "use crate::ids::{EntryId, OperationId};",
    "command error ids",
)
replace_once(
    "crates/ion-core/src/error.rs",
    '''    #[error("an operation is already running")]
    Busy { operation_id: OperationId },
''',
    '''    #[error("an operation is already running")]
    Busy { operation_id: OperationId },
    #[error("the lane already has a pending next run ({entry_id})")]
    NextRunQueued { entry_id: EntryId },
''',
    "next-run command error",
)

replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    "    QueuedAcceptanceCommit,\n",
    "    PendingNextRunCommit,\n",
    "effect boundary name",
)
replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''    Enqueue {
        prompt: String,
        reply: oneshot::Sender<Result<OperationId, CommandError>>,
    },
''',
    '''    NextRun {
        prompt: String,
        reply: oneshot::Sender<Result<crate::ids::EntryId, CommandError>>,
    },
''',
    "session command next run",
)

regex_once(
    "crates/ion-core/src/runtime/mod.rs",
    r'''    /// Accept a prompt durably\. If another operation is active, reserve
    /// stable identity for the lane's next run; its operation is created only
    /// after the active operation reaches a terminal outcome\.
    pub async fn enqueue\(&self, prompt: impl Into<String>\) -> Result<OperationId, CommandError> \{
.*?
    \}

    /// Join the active operation''',
    '''    /// Persist the lane's next-run input. If the lane is idle, it is
    /// accepted immediately; otherwise only its semantic entry identity is
    /// reserved. An `OperationId` does not exist until actual acceptance.
    pub async fn next_run(
        &self,
        prompt: impl Into<String>,
    ) -> Result<crate::ids::EntryId, CommandError> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .try_send(SessionCommand::NextRun {
                prompt: prompt.into(),
                reply,
            })
            .map_err(command_send_error)?;
        rx.await.map_err(|_| CommandError::RuntimeDropped)?
    }

    /// Join the active operation''',
    "SessionHandle next_run",
)

replace_once(
    "crates/ion-core/src/runtime/mod.rs",
    '''            SessionCommand::Enqueue { prompt, reply } => {
                let _ = reply.send(self.enqueue(prompt).await);
                false
            }
''',
    '''            SessionCommand::NextRun { prompt, reply } => {
                let _ = reply.send(self.next_run(prompt).await);
                false
            }
''',
    "runtime command dispatch",
)

regex_once(
    "crates/ion-core/src/runtime/mod.rs",
    r'''    async fn submit_if_idle\(&mut self, prompt: String\) -> Result<OperationId, CommandError> \{
.*?
    async fn switch_model\(''',
    '''    async fn submit_if_idle(&mut self, prompt: String) -> Result<OperationId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if let Some(active) = &self.operation {
            return Err(CommandError::Busy {
                operation_id: active.machine.operation_id(),
            });
        }
        if let Some(pending) = &self.pending_next_run {
            return Err(CommandError::NextRunQueued {
                entry_id: pending.entry_id,
            });
        }
        let (active, _) = self.accept_operation_record(prompt, None).await?;
        let operation_id = active.machine.operation_id();
        self.start_active(active);
        self.advance().await;
        Ok(operation_id)
    }

    /// Persist one next-run input durably. A busy lane receives only a
    /// provisioned semantic entry identity; operation identity is created
    /// when the lane becomes idle and actually accepts the run.
    async fn next_run(&mut self, prompt: String) -> Result<crate::ids::EntryId, CommandError> {
        if self.closed {
            return Err(CommandError::Closed);
        }
        if let Some(pending) = &self.pending_next_run {
            return Err(CommandError::NextRunQueued {
                entry_id: pending.entry_id,
            });
        }
        if self.operation.is_none() {
            let (active, entry_id) = self.accept_operation_record(prompt, None).await?;
            self.start_active(active);
            self.advance().await;
            return Ok(entry_id);
        }

        let next_run = crate::session::lane::NextRun::reserve(prompt);
        let entry_id = next_run.entry_id;
        self.store
            .queue_main_next_run(self.session_id, next_run.clone())
            .await
            .map_err(persistence_command_error)?;
        self.wait_effect_boundary(EffectBoundary::PendingNextRunCommit)
            .await;
        self.pending_next_run = Some(next_run);
        Ok(entry_id)
    }

    /// Create the durable operation only when the lane is free. A pending
    /// next run supplies its pre-provisioned semantic entry identity but does
    /// not pre-provision operation identity or freeze model/tool capability
    /// state before this acceptance boundary.
    async fn accept_operation_record(
        &mut self,
        prompt: String,
        reservation: Option<crate::session::lane::NextRun>,
    ) -> Result<(ActiveOperation, crate::ids::EntryId), CommandError> {
        let operation_id = OperationId::generate();
        let tool_registry = self.tools.snapshot();
        let (machine, applied) =
            OperationMachine::accept(operation_id, prompt.clone(), tool_registry.specs());
        let capability_snapshot = tool_registry.capability_snapshot();
        let root_inbox = InboxRecord {
            id: InboxId::generate(),
            kind: InboxKind::Prompt,
            text: prompt,
            status: InboxStatus::Applied,
        };
        let entry = match reservation.as_ref() {
            Some(next_run) => EntryRecord {
                id: next_run.entry_id,
                seq: self.next_entry_seq,
                parent: self.entries.last().map(|record| record.id),
                entry: applied.entries[0].clone(),
            },
            None => self.stage_entry(&applied.entries[0]),
        };
        let entry_id = entry.id;
        let checkpoint = CheckpointRecord {
            state_seq: 1,
            payload: CheckpointPayload {
                state: machine.state().clone(),
                cancel_requested: false,
                prompt: machine.prompt().to_owned(),
                capability_snapshot_id: capability_snapshot.id.clone(),
                open_effect: None,
            },
            capability_snapshot: capability_snapshot.clone(),
        };
        self.store
            .begin_operation(
                self.session_id,
                operation_id,
                root_inbox,
                checkpoint,
                entry.clone(),
            )
            .await
            .map_err(persistence_command_error)?;

        self.entries.push(entry);
        self.next_entry_seq += 1;
        if reservation.is_some() {
            self.pending_next_run = None;
        }
        Ok((
            ActiveOperation {
                machine,
                capability_snapshot,
                tool_registry,
                cancel: self.cancel_root.child_token(),
                state_seq: 1,
                open_effect: None,
                pending_steers: Vec::new(),
            },
            entry_id,
        ))
    }

    fn start_active(&mut self, active: ActiveOperation) {
        let operation_id = active.machine.operation_id();
        let prompt = active.machine.prompt().to_owned();
        self.operation = Some(active);
        self.draft_text.clear();
        self.draft_thinking.clear();
        self.assistant_frame_seq = 0;
        self.draft_calls.clear();
        self.draft_usage = None;
        self.live_tools.clear();
        self.pending_compact = None;
        self.overflow_retry_used = false;
        self.last_step_was_compaction = false;
        self.model_step = 0;
        self.operation_tool_calls = 0;
        self.emit(RuntimeEvent::OperationStarted {
            cursor: RuntimeCursor::default(),
            operation_id,
            prompt,
        });
    }

    async fn promote_pending_next_run(&mut self) -> bool {
        if self.operation.is_some() {
            return false;
        }
        let Some(next_run) = self.pending_next_run.clone() else {
            return false;
        };
        match self
            .accept_operation_record(next_run.prompt.clone(), Some(next_run.clone()))
            .await
        {
            Ok((active, _)) => {
                self.start_active(active);
                true
            }
            Err(err) => {
                error!(
                    session = %self.session_id,
                    entry = %next_run.entry_id,
                    %err,
                    "could not promote the durable next run; fencing until reopen"
                );
                self.closed = true;
                false
            }
        }
    }

    async fn switch_model(''',
    "runtime next-run semantics",
)

replace_once(
    "crates/ion/src/tui.rs",
    "UiEffect::Enqueue { text } => match session.enqueue(text).await {",
    "UiEffect::Enqueue { text } => match session.next_run(text).await {",
    "TUI next run",
)

regex_once(
    "crates/ion-core/src/tests/print_mode.rs",
    r'''#\[tokio::test\]
async fn enqueue_promotes_distinct_operations_in_acceptance_order\(\) \{
.*?
\}

#\[tokio::test\]
async fn queued_operation_survives_close_and_promotes_after_reopen\(\) \{
.*?
\}

#\[tokio::test\]
async fn steer_requires_an_active_operation''',
    '''#[tokio::test]
async fn next_run_provisions_entry_before_later_operation_admission() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![
            ScriptedMessage::delayed(Duration::from_millis(100), "first"),
            ScriptedMessage::text("second"),
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let first = session.submit_if_idle("one").await.expect("first submit");
    sleep(STEP).await;
    let _queued_entry = session.next_run("two").await.expect("next run");

    let mut starts = Vec::new();
    let mut finishes = Vec::new();
    while finishes.len() < 2 {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        match event {
            RuntimeEvent::OperationStarted { operation_id, .. } => starts.push(operation_id),
            RuntimeEvent::OperationFinished { operation_id, .. } => finishes.push(operation_id),
            _ => {}
        }
    }
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0], first);
    assert_ne!(starts[1], first);
    assert_eq!(finishes, starts);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn pending_next_run_survives_close_and_promotes_after_reopen() {
    let db = temp_db("queued-reopen");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "active never settles",
        )]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let first = session.submit_if_idle("active").await.expect("submit");
    wait_for_state(&session, |state| {
        matches!(state, OperationState::AssistantEffectPending)
    })
    .await;
    let queued_entry = session.next_run("queued").await.expect("next run");

    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    let loaded = store.load(session_id).await.expect("load queued state");
    assert_eq!(loaded.operations.len(), 1);
    assert_eq!(loaded.operations[0].id, first);
    assert_eq!(
        loaded.operations[0].latest.1.state,
        OperationState::Suspended
    );
    let pending = loaded
        .lane
        .state
        .pending_next_run
        .as_ref()
        .expect("durable next run");
    assert_eq!(pending.entry_id, queued_entry);
    assert_eq!(pending.prompt, "queued");
    assert!(
        loaded.entries.iter().all(|entry| entry.id != queued_entry),
        "pending input must not exist as a semantic entry before acceptance"
    );

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_millis(100),
            "queued result",
        )]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let second = match snapshot.operation {
        OperationStatus::Active { operation_id, .. } => operation_id,
        OperationStatus::Idle => panic!("pending next run was not promoted on reopen"),
    };
    assert_ne!(first, second);
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::OperationFinished { operation_id, .. } if *operation_id == second
    )));

    let loaded = store.load(session_id).await.expect("load settled state");
    assert_eq!(loaded.operations.len(), 2);
    assert_eq!(loaded.operations[0].id, first);
    assert_eq!(loaded.operations[1].id, second);
    assert!(loaded.entries.iter().any(|entry| {
        entry.id == queued_entry
            && matches!(
                &entry.entry,
                SessionEntry::UserMessage { text } if text == "queued"
            )
    }));
    assert_eq!(
        loaded.operations[0].latest.1.state,
        OperationState::Finished(OperationOutcome::Cancelled)
    );
    assert_eq!(
        loaded.operations[1].latest.1.state,
        OperationState::Finished(OperationOutcome::Completed)
    );

    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn steer_requires_an_active_operation''',
    "print next-run tests",
)

replace_once(
    "crates/ion-core/src/tests/crash_recovery.rs",
    "EffectBoundary::QueuedAcceptanceCommit",
    "EffectBoundary::PendingNextRunCommit",
    "pending next-run crash gate",
)
replace_once(
    "crates/ion-core/src/tests/crash_recovery.rs",
    'let enqueue = tokio::spawn(async move { enqueue_session.enqueue("second").await });',
    'let enqueue = tokio::spawn(async move { enqueue_session.next_run("second").await });',
    "crash next-run call",
)
replace_once(
    "crates/ion-core/src/tests/crash_recovery.rs",
    '''    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(loaded.operations.len(), 1);
    let pending = loaded
        .lane
        .state
        .pending_next_run
        .as_ref()
        .expect("durable next run");
    assert_eq!(pending.prompt, "second");
    assert!(
        loaded
            .operations
            .iter()
            .all(|operation| operation.id != pending.operation_id),
        "a queued run must not exist as an Accepted operation"
    );
''',
    '''    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(loaded.operations.len(), 1);
    let pending = loaded
        .lane
        .state
        .pending_next_run
        .as_ref()
        .expect("durable next run");
    assert_eq!(pending.prompt, "second");
    assert!(
        loaded.entries.iter().all(|entry| entry.id != pending.entry_id),
        "pending input must not exist as a semantic entry before acceptance"
    );
''',
    "crash pending assertion",
)

replace_once(
    "docs/architecture-v2.md",
    '''**VS Code Agent Host / Agent Host Protocol (AHP)** is the strongest current reference for the host/client boundary:

- the agent host owns sessions independently of UI clients;
- clients may disconnect while work continues;
- multiple clients can observe/control one session;
- reconnect is state-first: snapshot plus ordered actions/deltas;
- protocol capabilities/versioning are negotiated;
- client-contributed tools are scoped to the connected client;
- AHP coordinates hosted sessions above a particular harness protocol rather than replacing harness semantics.

Ion does not need to copy AHP's Redux-shaped protocol, but the ownership conclusion is important: **the TUI must be a client/projection of authoritative runtime state, not the execution owner.**
''',
    '''**Zed / Agent Client Protocol (ACP)** is the strongest current interoperability signal for the client/agent boundary:

- ACP has real multi-editor and multi-agent adoption rather than being a single-product internal protocol;
- its explicit session lifecycle, prompt/update/cancel flow, capability negotiation, and permission requests keep client UX concerns outside the agent reasoning loop;
- resume can replay session updates before the response completes, which forces clients to treat session state as an ordered protocol rather than a local UI object;
- Zed exercises this boundary with its own agent plus external agents such as Claude Agent and Codex CLI.

**VS Code Agent Host / Agent Host Protocol (AHP)** is complementary evidence, not a primary agent-substrate reference. Its August 2026 host redesign is unusually strong evidence for persistent host/client semantics:

- the dedicated agent host owns sessions independently of editor clients;
- work can continue with no editor attached;
- multiple local or remote clients can observe/control one session;
- reconnect is state-first: snapshot plus ordered actions/deltas;
- the host can sit above more than one agent harness/protocol.

ACP is the better signal for Ion's interoperable frontend boundary; AHP is the better corroborating signal for a durable host that outlives any one client. The shared conclusion is important: **TUI, ACP, and future remote clients are projections of authoritative runtime state, never the execution owner.**
''',
    "reference weighting",
)

replace_once(
    "docs/architecture-v2.md",
    '''capture lane.pending_next_run
append resulting semantic entries at lane.leaf
create immutable Operation { lane, source_leaf, accepted intent }
write first total OperationState
set lane.current_operation
clear captured pending_next_run
commit all of the above atomically
''',
    '''capture lane.pending_next_run
preserve its already-provisioned semantic EntryId
provision OperationId at acceptance, never while merely queued
append the resulting semantic entry at lane.leaf
create immutable Operation { lane, source_leaf, accepted intent }
write first total OperationState
set lane.current_operation
clear captured pending_next_run
commit all of the above atomically
''',
    "acceptance identity invariant",
)

regex_once(
    "docs/architecture-v2.md",
    r'''The most important remaining substrate mismatch is now busy-lane queueing: prompts still become complete queued `Accepted` operations rather than lane-owned `pending_next_run` input\.

## Next implementation order

.*?
The broad agent-architecture survey is now sufficient to proceed\. Further research should answer concrete implementation questions rather than reopen the substrate without new evidence\.''',
    '''The busy-lane queueing migration is now the active checkpoint: `pending_next_run` owns a provisioned semantic `EntryId`, while `OperationId` is provisioned only when the lane actually accepts the run. SQLite persists the pending identity without inventing it, and no queued `Accepted` operation exists.

## Next implementation order

1. Finish and validate total lane state (`leaf`, `current_operation`, `pending_next_run`) with the provision-before-persistence identity rule above.
2. Make operation acceptance explicitly lane-addressable over the durable lane/source-leaf origin contract.
3. Generalize the session owner from hidden `main` to multiple lanes while retaining one writer and concurrent slow effects.
4. Introduce family-scoped agent control with admission-first identity, separate retained registry/execution permits, explicit wait semantics, cancellation ownership, and deterministic reattachment.
5. Replace `ChildManager`/child-only topology with lane/fork/fresh agent admission. Add worktree/remote topology only when its owner exists.
6. Add durable agent messaging/background completion through the common session-input path.
7. Add scoped capability publication/teardown around agent creation/resume.
8. Finish typed tool/effect admission boundaries and expand `ion-eval` around recovery/multi-agent invariants.
9. Reconcile `DESIGN.md`, public API/module vocabulary, and dead compatibility scaffolding.
10. **Only then redesign the TUI** as an ACP-capable client of the authoritative session/agent host contract.

The broad agent-architecture survey is now sufficient to proceed. Further research should answer concrete implementation questions rather than reopen the substrate without new evidence.''',
    "next implementation order",
)
