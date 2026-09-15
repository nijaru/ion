//! K4 persistence: a real per-session database, reopened from disk.
//!
//! These tests exercise the durable boundary rather than the resident one: what
//! survives a close, what a second authority can do, and what opening does.

use std::path::Path;
use std::sync::Arc;

use ion_ai::{Content, GenerationControls, Message, ModelRef, Reasoning, ToolChoice};
use ion_core::conversation::context::ContextControl;
use ion_core::{
    CloseMode, ContextPolicy, ConversationConfig, ConversationSpec, EntryKind, EntryRequest,
    InputBody, InputMode, InputRequest, InputSender, PlannedEntry, PlannedTask, PlannedTurn,
    RequestKey, RunLimits, RunningTask, Session, SessionError, TaskCompletion, TaskContext,
    TaskDriver, TaskFuture, TaskId, TaskKind, TaskKindName, TaskPlan, TaskRegistry, TaskRequest,
    TaskStatus,
};
use serde_json::json;

mod support;

use support::TempDb;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

/// Settles with a plan that leaves one scoped successor, so the turn stays live.
struct ScopedSuccessor;

impl TaskKind for ScopedSuccessor {
    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            context
                .checkpoint(Some(json!({"phase": "before-finalization"})), None)
                .await?;
            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: (task.conversation_id).into(),
                kind: EntryKind::new("assistant").expect("entry kind"),
                data: json!({"text": "dispatching"}),
                projection: Vec::new(),
                context: ContextControl::none(),
            });
            plan.create_task(PlannedTask {
                conversation_id: (task.conversation_id).into(),
                kind: kind("tool"),
                schema_version: 1,
                input: json!({"call": "scoped"}),
                dependencies: Vec::new(),
                turn: PlannedTurn::Inherit,
            });
            Ok(TaskCompletion::completed(json!("dispatched")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

/// Settles with a plan whose only successor is background, so the turn ends.
struct BackgroundSuccessor;

impl TaskKind for BackgroundSuccessor {
    fn execute<'a>(&'a self, task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            plan.create_task(PlannedTask {
                conversation_id: (task.conversation_id).into(),
                kind: kind("worker"),
                schema_version: 1,
                input: json!({"task": "background"}),
                dependencies: Vec::new(),
                turn: PlannedTurn::Background,
            });
            Ok(TaskCompletion::completed(json!("spawned")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

/// The foreground turn root a specific conversation currently holds.
async fn foreground(
    driver: &TaskDriver,
    conversation_id: ion_core::ConversationId,
) -> Option<TaskId> {
    driver
        .snapshot()
        .await
        .conversations
        .into_iter()
        .find(|conversation| conversation.id == conversation_id)
        .expect("conversation exists")
        .foreground_turn
}

fn registry() -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("scoped"), 1, Arc::new(ScopedSuccessor))
        .expect("register");
    registry
        .register(kind("background"), 1, Arc::new(BackgroundSuccessor))
        .expect("register");
    registry
}

/// Commit a broad, representative write set and return the session together
/// with the receipts a reopened session must reproduce.
async fn build_session(path: &Path) -> (TaskDriver, TaskId) {
    let mut session = Session::create(path).expect("create");
    let root = session.root_conversation();

    let first = session
        .append_entry(EntryRequest {
            conversation_id: root,
            kind: EntryKind::new("user").expect("entry kind"),
            data: json!({"text": "hello"}),
            projection: Vec::new(),
            context: ContextControl::none(),
        })
        .expect("entry");
    session
        .append_entry(EntryRequest {
            conversation_id: root,
            kind: EntryKind::new("assistant").expect("entry kind"),
            data: json!({"text": "hi"}),
            projection: Vec::new(),
            context: ContextControl::none(),
        })
        .expect("entry");
    let fork = session
        .create_conversation(ConversationSpec {
            parent: Some(ion_core::HistoryParent {
                conversation_id: root,
                at: first.entry_id,
            }),
            owner_task: None,
        })
        .expect("fork")
        .conversation_id;
    session
        .queue_input(InputRequest {
            target: root,
            sender: InputSender::User,
            mode: InputMode::Submit,
            request_key: Some(RequestKey::new("durable-key").expect("key")),
            body: InputBody::Text("go".to_owned()),
        })
        .expect("input");

    let pending = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind("tool"),
            schema_version: 1,
            input: json!({"call": "pending"}),
            dependencies: Vec::new(),
        })
        .expect("task")
        .task_id;
    // A second task owned by nothing, used to prove dependency rows survive.
    session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind("join"),
            schema_version: 1,
            input: json!({"waiting": true}),
            dependencies: vec![pending],
        })
        .expect("dependent task");
    // A conversation owned by a task, written as two mutations in one batch.
    session
        .create_conversation(ConversationSpec {
            parent: None,
            owner_task: Some(pending),
        })
        .expect("owned conversation");

    let driver = TaskDriver::new(session, registry());

    // A turn that keeps its slot because a scoped successor remains.
    let turn = driver
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("scoped"),
            schema_version: 1,
            input: json!({"prompt": "first"}),
            dependencies: Vec::new(),
        })
        .await
        .expect("turn");
    driver.drive_task(turn.task_id).await.expect("drive turn");
    assert_eq!(
        foreground(&driver, root).await,
        Some(turn.task_id),
        "a scoped successor keeps the turn slot open"
    );

    // A second turn, on the fork, whose only successor is background: creating
    // it opens the slot and finalization releases it again.
    let background = driver
        .create_turn(TaskRequest {
            conversation_id: fork,
            kind: kind("background"),
            schema_version: 1,
            input: json!({"prompt": "second"}),
            dependencies: Vec::new(),
        })
        .await
        .expect("turn");
    assert_eq!(foreground(&driver, fork).await, Some(background.task_id));
    driver
        .drive_task(background.task_id)
        .await
        .expect("drive turn");
    assert_eq!(foreground(&driver, fork).await, None);
    assert_eq!(
        foreground(&driver, root).await,
        Some(turn.task_id),
        "the unrelated turn is unaffected"
    );

    // A durable cancellation mark on a still-pending task.
    let cancelled = driver.cancel_task(pending).await.expect("cancel pending");
    assert!(cancelled.changed);

    (driver, turn.task_id)
}

/// R5: observation coverage does not survive a restart by itself.
///
/// A reopened session learns its history from the store rather than from its own
/// commits, so its event buffer starts empty. A cursor from before the restart
/// must therefore require a resnapshot; returning an empty successful delta would
/// tell the client it is current when commits it never saw are gone.
#[test]
fn an_observation_cursor_from_before_a_restart_requires_a_resnapshot() {
    let db = TempDb::new("observations-restart");
    let mut session = Session::create(db.path()).expect("create");
    let conversation = session.root_conversation();
    let cursor = session
        .append_entry(note(conversation, "observed"))
        .expect("commit")
        .commit_seq;
    // A commit the client has not observed, so its cursor is genuinely behind.
    let unseen = session
        .append_entry(note(conversation, "missed"))
        .expect("commit")
        .commit_seq;
    assert!(unseen > cursor);
    drop(session);

    let mut reopened = Session::open(db.path()).expect("reopen");
    assert_eq!(reopened.summary().last_commit, unseen);
    let stale = reopened.observations_after(Some(cursor));
    assert!(
        stale.reset_required,
        "a cursor from before the restart must resnapshot"
    );
    assert!(stale.events.is_empty());

    // A client whose cursor is at the loaded commit saw everything durable, so it
    // resumes streaming instead of resnapshotting.
    let resumed = reopened.observations_after(Some(unseen));
    assert!(!resumed.reset_required);
    assert!(resumed.events.is_empty());

    // And a commit after the reopen is an ordinary delta for that cursor.
    let after = reopened
        .append_entry(note(conversation, "after the restart"))
        .expect("commit")
        .commit_seq;
    let delta = reopened.observations_after(Some(unseen));
    assert!(!delta.reset_required);
    assert_eq!(
        delta.events.len(),
        1,
        "the post-restart commit is the delta"
    );
    assert_eq!(delta.events[0].commit_seq, after);
}

/// The positive control for the resnapshot rule: a client that reconnects after a
/// restart with a cursor at the loaded commit keeps streaming deltas, including
/// from the watch that wakes it.
#[tokio::test]
async fn a_reopened_session_streams_commits_after_the_loaded_one() {
    let db = TempDb::new("observations-resume");
    let mut session = Session::create(db.path()).expect("create");
    let conversation = session.root_conversation();
    let cursor = session
        .append_entry(note(conversation, "before the restart"))
        .expect("commit")
        .commit_seq;
    drop(session);

    let driver = TaskDriver::open(db.path(), TaskRegistry::new()).expect("reopen");
    assert_eq!(driver.summary().await.last_commit, cursor);
    let committed = driver
        .create_turn(TaskRequest {
            conversation_id: conversation,
            kind: kind("unrelated"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .await
        .expect("commit")
        .commit_seq;
    let batch = driver
        .wait_observations(Some(cursor))
        .await
        .expect("a waiter at the loaded cursor receives the next commit");
    assert!(!batch.reset_required);
    assert!(
        batch
            .events
            .iter()
            .any(|event| event.commit_seq == committed),
        "the commit that woke the waiter is in the delta from the loaded cursor"
    );
}

fn note(conversation: ion_core::ConversationId, text: &str) -> EntryRequest {
    EntryRequest {
        conversation_id: conversation,
        kind: ion_core::EntryKind::new("note").expect("entry kind"),
        data: json!({"text": text}),
        projection: Vec::new(),
        context: ContextControl::none(),
    }
}

#[tokio::test]
async fn reopening_reconstructs_entries_inputs_tasks_and_turns() {
    let db = TempDb::new("roundtrip");
    let (driver, _turn) = build_session(db.path()).await;
    let before = driver.snapshot().await;
    let summary = driver.summary().await;
    let observations = driver.observations_after(None).await;
    assert!(summary.entries >= 3);
    assert!(!observations.events.is_empty());
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    let mut reopened = Session::open(db.path()).expect("reopen");
    assert_eq!(reopened.summary(), summary);
    assert_eq!(reopened.snapshot(), before);
    assert_eq!(reopened.session_id(), summary.session_id);
    assert_eq!(reopened.root_conversation(), summary.root_conversation);

    // The durable request key and admission commit reconstruct duplicate-input
    // receipts without re-admitting the input.
    let input = reopened.snapshot().inputs[0].clone();
    assert_eq!(input.disposition, before.inputs[0].disposition);
    let key = input.request_key.clone().expect("request key survived");
    let replay = reopened
        .queue_input(InputRequest {
            target: input.target,
            sender: input.sender,
            mode: input.mode,
            request_key: Some(key),
            body: input.body,
        })
        .expect("replay");
    assert!(replay.replayed);
    assert_eq!(replay.input_id, input.id);
    assert_eq!(reopened.summary(), summary, "replay commits nothing new");

    // A live owner is exclusive: a second writable authority is refused before it
    // can reconstruct anything, let alone dispatch.
    let refused = Session::open(db.path()).expect_err("a live owner is exclusive");
    assert!(
        matches!(refused, SessionError::SessionInUse(_)),
        "{refused:?}"
    );
    assert_eq!(
        reopened.snapshot(),
        before,
        "the refused open changed nothing"
    );

    // Releasing the owner hands the same durable state to the next process.
    drop(reopened);
    let next = Session::open(db.path()).expect("open after the owner went away");
    assert_eq!(next.snapshot(), before);
}

#[tokio::test]
async fn a_driver_creates_and_opens_an_on_disk_session() {
    let db = TempDb::new("driver-open");
    let driver = TaskDriver::create(db.path(), registry()).expect("create over the driver");
    let root = driver.snapshot().await.root_conversation;
    let turn = driver
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: kind("scoped"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .await
        .expect("turn");
    driver.drive_task(turn.task_id).await.expect("drive");
    let before = driver.snapshot().await;
    let summary = driver.summary().await;
    assert_eq!(
        foreground(&driver, root).await,
        Some(turn.task_id),
        "the scoped successor still holds the slot"
    );
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    // The client can reopen through the driver, and opening reads only: the
    // pending successor is not driven and the slot is unchanged.
    let reopened = TaskDriver::open(db.path(), registry()).expect("open over the driver");
    assert_eq!(reopened.snapshot().await, before);
    assert_eq!(reopened.summary().await, summary);
    assert_eq!(foreground(&reopened, root).await, Some(turn.task_id));
    assert_eq!(
        reopened
            .snapshot()
            .await
            .tasks
            .iter()
            .filter(|task| matches!(task.status, TaskStatus::Pending))
            .count(),
        1,
        "opening starts no work"
    );
}

/// A handler that panics leaves the task durably running, which is the state a
/// reopened process has to find and explicitly recover.
struct Panicking;

impl TaskKind for Panicking {
    fn execute<'a>(&'a self, _task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move { panic!("process died before settling") })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

/// The fixed ids a fixture can address, taken from the records that were written
/// rather than assumed.
struct StoredIds {
    _root: i64,
    entry: i64,
    conversation: i64,
    task: i64,
    dependent: i64,
    owned: i64,
}

#[tokio::test]
async fn an_inconsistent_store_is_refused_rather_than_repaired() {
    let db = TempDb::new("inconsistent");
    let path = db.path().to_path_buf();
    let ids = record_a_representative_store(&path).await;
    let entry = ids.entry;
    let conversation = ids.conversation;
    let task = ids.task;
    let dependent = ids.dependent;
    let owned = ids.owned;

    // Each damage is valid SQL that a damaged database or an older build could
    // present. `(name, statement, required rule)`.
    let fixtures = [
        (
            "lowered-sequence",
            format!("UPDATE session_meta SET last_seq = {entry} WHERE id = 1"),
            "sequence bound",
        ),
        (
            "running-without-invocation",
            format!("UPDATE tasks SET state = 'running', invocation = NULL WHERE id = {task}"),
            "task lifecycle",
        ),
        (
            "terminal-with-an-invocation",
            format!(
                "UPDATE tasks SET state = 'terminal', outcome = '{{\"kind\":\"Completed\",\"value\":null}}', \
                 invocation = '{{\"generation\":1,\"kind\":\"Execute\"}}' WHERE id = {task}"
            ),
            "task lifecycle",
        ),
        (
            "ownership-without-reciprocity",
            format!("UPDATE conversations SET owner_task = {task} WHERE id = {conversation}"),
            "ownership",
        ),
        (
            "ownership-the-task-does-not-list",
            format!("DELETE FROM task_ownership WHERE conversation_id = {owned}"),
            "ownership",
        ),
        (
            "dependency-on-a-missing-task",
            format!("UPDATE task_dependencies SET depends_on = 900 WHERE task_id = {dependent}"),
            "dependency",
        ),
        (
            "forward-dependency",
            format!(
                "INSERT INTO task_dependencies (task_id, position, depends_on) \
                 VALUES ({task}, 0, {dependent})"
            ),
            "dependency",
        ),
        (
            "entry-without-its-conversation",
            format!("UPDATE entries SET conversation_id = 900 WHERE id = {entry}"),
            "entry conversation",
        ),
        (
            "configuration-without-a-commit",
            "UPDATE conversations SET config_revision = NULL WHERE config IS NOT NULL".to_owned(),
            "configuration",
        ),
        (
            "configuration-commit-beyond-the-cursor",
            "UPDATE conversations SET config_revision = 900 WHERE config IS NOT NULL".to_owned(),
            "commit bound",
        ),
        (
            "configuration-this-build-refuses",
            "UPDATE conversations \
             SET config = replace(config, '\"max_attempts_per_step\":3', '\"max_attempts_per_step\":99') \
             WHERE config IS NOT NULL"
                .to_owned(),
            "configuration",
        ),
    ];

    for (name, damage, rule) in fixtures {
        let copy = TempDb::new(&format!("inconsistent-{name}"));
        let copy_path = copy.path().to_path_buf();
        std::fs::copy(&path, &copy_path).expect("copy the store");
        {
            let connection = rusqlite::Connection::open(&copy_path).expect("raw open");
            connection.execute(&damage, []).expect("apply the fixture");
        }

        let error = Session::open(&copy_path)
            .err()
            .unwrap_or_else(|| panic!("{name} must be refused"));
        let message = error.to_string();
        assert!(
            message.contains(rule),
            "{name} must be refused for {rule}, got: {message}"
        );
    }

    // The undamaged store still opens: the fixtures above are the only
    // difference, so a refusal is not a blanket rejection of stored sessions.
    Session::open(&path).expect("the undamaged store still opens");
}

/// A complete configuration, so the fixtures can damage a realistic record.
fn stored_config() -> ConversationConfig {
    ConversationConfig {
        model: ModelRef {
            provider: "scripted".to_owned(),
            model: "test-model".to_owned(),
        },
        instructions: "be careful".to_owned(),
        controls: GenerationControls {
            max_output_tokens: 4096,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: false,
        },
        project_context: vec![Message {
            role: ion_ai::Role::User,
            content: vec![Content::Text("project rule".to_owned())],
            provider_replay: None,
        }],
        instruction_revision: "ion-instructions-v1".to_owned(),
        tool_names: vec!["read".to_owned()],
        context: ContextPolicy {
            max_request_bytes: 4 * 1024 * 1024,
            max_input_tokens: 100_000,
            compact_at_tokens: 80_000,
            summary_max_tokens: 2048,
        },
        limits: RunLimits {
            max_model_steps: 20,
            max_attempts_per_step: 3,
            max_cost_microusd: None,
            deadline_ms: 600_000,
            max_response_bytes: 1024 * 1024,
            max_tool_output_bytes: 64 * 1024,
        },
    }
}

/// Store a small but representative session: a root, an unowned conversation, an
/// entry, a task with a dependent, and a task-owned conversation.
async fn record_a_representative_store(path: &Path) -> StoredIds {
    let mut session = Session::create(path).expect("create");
    let root = session.root_conversation();
    let entry = session
        .append_entry(EntryRequest {
            conversation_id: root,
            kind: EntryKind::new("user").expect("entry kind"),
            data: json!({"text": "hello"}),
            projection: Vec::new(),
            context: ContextControl::none(),
        })
        .expect("entry")
        .entry_id;
    let conversation = session
        .create_conversation(ConversationSpec {
            parent: None,
            owner_task: None,
        })
        .expect("conversation")
        .conversation_id;
    let task = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind("tool"),
            schema_version: 1,
            input: json!({"call": "one"}),
            dependencies: Vec::new(),
        })
        .expect("task")
        .task_id;
    let dependent = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind("join"),
            schema_version: 1,
            input: json!({"waiting": true}),
            dependencies: vec![task],
        })
        .expect("dependent")
        .task_id;
    let owned = session
        .create_conversation(ConversationSpec {
            parent: None,
            owner_task: Some(task),
        })
        .expect("owned conversation")
        .conversation_id;
    // A stored configuration, so the fixtures below can damage one.
    session
        .configure_conversation(root, None, stored_config())
        .expect("configure");
    drop(session);
    StoredIds {
        _root: root.get(),
        entry: entry.get(),
        conversation: conversation.get(),
        task: task.get(),
        dependent: dependent.get(),
        owned: owned.get(),
    }
}

#[tokio::test]
async fn reopening_leaves_running_tasks_for_explicit_recovery() {
    let db = TempDb::new("running");
    let path = db.path().to_path_buf();

    let mut session = Session::create(&path).expect("create");
    let root = session.root_conversation();
    let task = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: kind("panicking"),
            schema_version: 1,
            input: json!({"prompt": "interrupted"}),
            dependencies: Vec::new(),
        })
        .expect("task")
        .task_id;
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("panicking"), 1, Arc::new(Panicking))
        .expect("register");
    let driver = TaskDriver::new(session, registry);
    let outcome = driver.drive_task(task).await.expect("drive");
    assert!(matches!(outcome, ion_core::DriveOutcome::Interrupted(_)));

    let committed = driver.summary().await.last_commit;
    let before = driver.snapshot().await;
    let running = before
        .tasks
        .iter()
        .find(|record| record.id == task)
        .expect("task record");
    assert!(matches!(running.status, TaskStatus::Running));
    let generation = running.generation;
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    let reopened = Session::open(&path).expect("reopen");
    let record = reopened
        .snapshot()
        .tasks
        .into_iter()
        .find(|record| record.id == task)
        .expect("task survived");
    assert!(matches!(record.status, TaskStatus::Running));
    assert_eq!(record.generation, generation);
    assert_eq!(
        record.invocation.expect("invocation").generation,
        generation
    );
    assert_eq!(
        reopened.summary().last_commit,
        committed,
        "opening commits nothing and starts no work"
    );
}

/// R2: the turn cancellation barrier is durable turn state, so it survives a
/// reopen and keeps fencing the members of a turn whose root had already
/// settled before the cancellation.
#[tokio::test]
async fn a_turn_cancellation_barrier_survives_reopen() {
    let db = TempDb::new("turn-barrier");
    let (driver, turn) = build_session(db.path()).await;
    let conversation = driver
        .conversation(driver.snapshot().await.root_conversation)
        .await
        .expect("root conversation");
    assert_eq!(
        conversation.foreground_turn,
        Some(turn),
        "the turn under test is the live one"
    );
    assert!(!conversation.turn_cancelled);

    driver.cancel_turn(turn).await.expect("cancel the turn");
    let cancelled_snapshot = driver.snapshot().await;
    let cancelled = cancelled_snapshot
        .conversations
        .iter()
        .find(|record| record.id == cancelled_snapshot.root_conversation)
        .expect("root conversation");
    assert_eq!(cancelled.foreground_turn, Some(turn));
    assert!(cancelled.turn_cancelled, "the barrier is set");
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    let reopened = TaskDriver::open(db.path(), registry()).expect("reopen");
    let snapshot = reopened.snapshot().await;
    let conversation = snapshot
        .conversations
        .iter()
        .find(|record| record.id == snapshot.root_conversation)
        .expect("root conversation");
    assert_eq!(conversation.foreground_turn, Some(turn));
    assert!(
        conversation.turn_cancelled,
        "the barrier must survive a reopen, or a recovered member's cleanup could run in a stopped turn"
    );
    assert_eq!(snapshot, cancelled_snapshot);
}

/// Exclusive ownership (`k4_ownership.rs`) makes a second live store authority
/// unreachable through the API, so the commit-cursor compare-and-set is
/// exercised the way it now earns its keep: a writer that bypassed ownership
/// has moved the cursor and the owner's next commit must be fenced rather than
/// silently interleaving.
#[tokio::test]
async fn a_commit_cursor_moved_behind_the_writer_fences_it() {
    let db = TempDb::new("fence");
    let path = db.path().to_path_buf();
    let mut session = Session::create(&path).expect("create");
    let root = session.root_conversation();
    let entry = |text: &str| EntryRequest {
        conversation_id: root,
        kind: EntryKind::new("note").expect("entry kind"),
        data: json!({"text": text}),
        projection: Vec::new(),
        context: ContextControl::none(),
    };

    session.append_entry(entry("from first")).expect("commit");
    let committed = session.summary().last_commit;
    {
        let foreign = rusqlite::Connection::open(&path).expect("foreign connection");
        foreign
            .execute(
                "UPDATE session_meta SET last_commit = last_commit + 1 WHERE id = 1",
                [],
            )
            .expect("advance the cursor behind the owner's back");
    }

    let error = session
        .append_entry(entry("from second"))
        .expect_err("a stale authority must be rejected");
    assert!(matches!(error, SessionError::Persistence(_)), "{error:?}");
    assert_eq!(
        session.summary().last_commit,
        committed,
        "the failed commit changed no resident cursor"
    );
    // The fenced authority is closed rather than left usable.
    assert!(matches!(
        session.append_entry(entry("again")),
        Err(SessionError::Closed)
    ));

    drop(session);
    let durable = Session::open(&path).expect("reopen");
    let texts: Vec<_> = durable
        .snapshot()
        .entries
        .iter()
        .map(|entry| entry.data["text"].clone())
        .collect();
    assert_eq!(texts, vec![json!("from first")]);
}

/// Environment variable naming the database a crash child should write to.
const CRASH_DB: &str = "ION_K4_CRASH_DB";

/// Payloads the crash child commits and the parent then looks for.
const CRASH_PAYLOADS: [&str; 3] = [
    "committed-before-crash-0",
    "committed-before-crash-1",
    "committed-before-crash-2",
];

/// Run only inside the child process spawned by
/// `committed_writes_survive_process_death`. It commits, then dies without
/// unwinding, closing or checkpointing anything.
#[test]
fn crash_child_commits_then_dies() {
    let Some(path) = std::env::var_os(CRASH_DB) else {
        return;
    };
    let mut session = Session::create(&path).expect("create");
    let root = session.root_conversation();
    for payload in CRASH_PAYLOADS {
        session
            .append_entry(EntryRequest {
                conversation_id: root,
                kind: EntryKind::new("note").expect("entry kind"),
                data: json!({"payload": payload}),
                projection: Vec::new(),
                context: ContextControl::none(),
            })
            .expect("commit");
    }
    // No destructors, no clean close, no WAL checkpoint: the process simply
    // stops. Everything already committed must still be there.
    std::process::abort();
}

#[cfg(unix)]
#[test]
fn committed_writes_survive_process_death() {
    use std::os::unix::process::ExitStatusExt;

    let db = TempDb::new("crash");
    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "crash_child_commits_then_dies", "--nocapture"])
        .env(CRASH_DB, db.path())
        .status()
        .expect("spawn crash child");

    assert_eq!(
        status.signal(),
        Some(6),
        "the child must die on SIGABRT, not exit or panic: {status:?}"
    );

    let reopened = Session::open(db.path()).expect("reopen after process death");
    let payloads: Vec<_> = reopened
        .snapshot()
        .entries
        .iter()
        .map(|entry| entry.data["payload"].clone())
        .collect();
    assert_eq!(
        payloads,
        CRASH_PAYLOADS.map(|payload| json!(payload)).to_vec(),
        "every commit acknowledged before the crash must be durable"
    );
    assert!(reopened.summary().last_commit.get() > 0);

    // Recovery is idempotent: after the first reopen releases ownership, the
    // next one sees exactly the same history.
    let snapshot = reopened.snapshot();
    drop(reopened);
    let again = Session::open(db.path()).expect("second reopen");
    assert_eq!(again.snapshot(), snapshot);
}

/// The per-conversation entry index is derived state: it must agree with the
/// records it was rebuilt from, including when appends from two conversations
/// interleave and when a fork inherits a prefix of them.
#[tokio::test]
async fn the_entry_index_agrees_with_records_before_and_after_reopen() {
    let db = TempDb::new("entry-index");
    let mut session = Session::create(db.path()).expect("create");
    let root = session.root_conversation();
    let other = session
        .create_conversation(ConversationSpec::independent())
        .expect("second conversation")
        .conversation_id;

    let mut root_ids = Vec::new();
    let mut other_ids = Vec::new();
    for index in 0..4 {
        root_ids.push(
            session
                .append_entry(note(root, &format!("root {index}")))
                .expect("root append")
                .entry_id,
        );
        other_ids.push(
            session
                .append_entry(note(other, &format!("other {index}")))
                .expect("other append")
                .entry_id,
        );
    }

    let live_root = session
        .conversation_entries(root, None, 64)
        .expect("root page")
        .entries;
    let live_other = session
        .conversation_entries(other, None, 64)
        .expect("other page")
        .entries;
    assert_eq!(
        live_root.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        root_ids,
        "a conversation's page is exactly its own entries, in append order"
    );
    assert_eq!(
        live_other.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        other_ids
    );

    // A fork inherits its parent's prefix; the index must not leak the sibling
    // conversation into it.
    let fork = session
        .create_conversation(ConversationSpec::fork(root, root_ids[1]))
        .expect("fork")
        .conversation_id;
    let inherited = session
        .conversation_entries(fork, None, 64)
        .expect("fork page")
        .entries;
    assert_eq!(
        inherited.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![root_ids[0], root_ids[1]]
    );
    drop(session);

    let reopened = Session::open(db.path()).expect("open");
    for (conversation, expected) in [(root, root_ids.clone()), (other, other_ids.clone())] {
        let page = reopened
            .conversation_entries(conversation, None, 64)
            .expect("page")
            .entries;
        assert_eq!(
            page.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            expected,
            "reconstruction rebuilds the index from the same records"
        );
    }
    let inherited = reopened
        .conversation_entries(fork, None, 64)
        .expect("fork page")
        .entries;
    assert_eq!(
        inherited.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        vec![root_ids[0], root_ids[1]]
    );
}
