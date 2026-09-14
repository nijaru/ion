//! K4 read/observation client contract.
//!
//! These tests pin the bounded read surface a live client uses instead of
//! materializing full state, and the observation-recovery rules (retained tail,
//! coverage gap, cursor from an unknown authority).

use std::sync::Arc;

use ion_core::conversation::context::ContextControl;
use ion_core::{
    Change, CloseMode, CommitSeq, EntryKind, InputBody, InputMode, InputRequest, InputSender,
    PlannedEntry, PlannedTask, PlannedTurn, RunningTask, Session, SessionError, TaskCompletion,
    TaskContext, TaskDriver, TaskDriverError, TaskFuture, TaskId, TaskKind, TaskKindName, TaskPlan,
    TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::json;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

/// Appends three entries and creates two successors, then settles.
struct Author;

impl TaskKind for Author {
    fn execute<'a>(&'a self, task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            for index in 0..3 {
                plan.append_entry(PlannedEntry {
                    conversation_id: (task.conversation_id).into(),
                    kind: EntryKind::new("note").expect("entry kind"),
                    data: json!({"index": index}),
                    projection: Vec::new(),
                    context: ContextControl::none(),
                });
            }
            for name in ["left", "right"] {
                plan.create_task(PlannedTask {
                    conversation_id: (task.conversation_id).into(),
                    kind: kind(name),
                    schema_version: 1,
                    input: json!({}),
                    dependencies: Vec::new(),
                    turn: PlannedTurn::Inherit,
                });
            }
            Ok(TaskCompletion::completed(json!("authored")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

fn authoring_driver() -> (TaskDriver, TaskId) {
    let mut session = Session::new().expect("session");
    let task = session
        .create_task(TaskRequest {
            conversation_id: session.root_conversation(),
            kind: kind("author"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .expect("task");
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("author"), 1, Arc::new(Author))
        .expect("register");
    (TaskDriver::new(session, registry), task.task_id)
}

#[tokio::test]
async fn driver_exposes_bounded_reads_and_paged_transcript() {
    let (driver, task_id) = authoring_driver();
    driver.drive_task(task_id).await.expect("drive");

    let summary = driver.summary().await;
    assert_eq!(summary.entries, 3);
    assert_eq!(summary.conversations, 1);
    assert_eq!(summary.tasks.terminal, 1);
    assert_eq!(summary.tasks.pending, 2);

    // Single-record lookup needs no full snapshot.
    let record = driver.task(task_id).await.expect("task record");
    assert!(matches!(record.status, TaskStatus::Terminal(_)));
    // A sequence value beyond the last commit is unused by any record.
    let unused = summary.last_commit.get() + 1;
    assert!(
        driver
            .task(TaskId::new(unused).expect("unused seq"))
            .await
            .is_none()
    );

    let conversation = driver.snapshot().await.root_conversation;

    let first = driver
        .conversation_entries(conversation, None, 1)
        .await
        .expect("page");
    assert_eq!(first.entries.len(), 1);
    let cursor = first.next.expect("more pages");

    let second = driver
        .conversation_entries(conversation, Some(cursor), 1)
        .await
        .expect("page");
    assert_eq!(second.entries.len(), 1);
    assert_ne!(second.entries[0].id, first.entries[0].id);

    let rest = driver
        .conversation_entries(conversation, second.next, 10)
        .await
        .expect("page");
    assert_eq!(rest.entries.len(), 1);
    assert!(rest.next.is_none());

    // An empty page request is not an error and never repeats the cursor.
    let empty = driver
        .conversation_entries(conversation, None, 0)
        .await
        .expect("page");
    assert!(empty.entries.is_empty());
    assert!(empty.next.is_none());

    // An invisible cursor is rejected rather than silently treated as "start".
    let unknown = ion_core::EntryId::new(unused).expect("unused seq");
    assert!(matches!(
        driver
            .conversation_entries(conversation, Some(unknown), 1)
            .await,
        Err(SessionError::InvisibleContextReference(id)) if id == unknown
    ));
}

#[tokio::test]
async fn observations_offer_a_tail_and_reject_unknown_cursors() {
    let (driver, _task_id) = authoring_driver();
    let initial = driver.observations_after(None).await;
    assert!(!initial.reset_required);
    assert!(!initial.events.is_empty());
    let mut last = None;
    for event in &initial.events {
        if let Some(previous) = last {
            assert!(event.commit_seq > previous, "commit order is monotonic");
        }
        last = Some(event.commit_seq);
    }
    let cursor = last.expect("at least one commit");

    // Replaying a cursor that is already current yields nothing, not a reset.
    let current = driver.observations_after(Some(cursor)).await;
    assert!(!current.reset_required);
    assert!(current.events.is_empty());

    // A later commit is visible from the same cursor, and opening the
    // foreground slot is invalidated explicitly, not only via task changes.
    let conversation = driver.snapshot().await.root_conversation;
    driver
        .create_turn(TaskRequest {
            conversation_id: conversation,
            kind: kind("next"),
            schema_version: 1,
            input: json!({}),
            dependencies: Vec::new(),
        })
        .await
        .expect("turn");
    let later = driver.observations_after(Some(cursor)).await;
    assert!(!later.reset_required);
    assert!(!later.events.is_empty());
    assert!(later.events.iter().all(|event| event.commit_seq > cursor));
    assert!(later.events.iter().any(|event| {
        event.changes.iter().any(
            |change| matches!(change, Change::ForegroundTurnChanged(id) if *id == conversation),
        )
    }));

    // A cursor from an unknown authority must not look like an empty tail.
    let ahead = CommitSeq::new(cursor.get() + 1_000).expect("seq");
    let future = driver.observations_after(Some(ahead)).await;
    assert!(future.reset_required);
    assert!(future.events.is_empty());
}

#[tokio::test]
async fn observations_require_a_resnapshot_past_retained_coverage() {
    let mut session = Session::new().expect("session");
    let conversation = session.root_conversation();
    let first = session
        .append_entry(ion_core::EntryRequest {
            conversation_id: conversation,
            kind: EntryKind::new("note").expect("entry kind"),
            data: json!({"index": 0}),
            projection: Vec::new(),
            context: ContextControl::none(),
        })
        .expect("entry")
        .commit_seq;

    for index in 1..200 {
        session
            .append_entry(ion_core::EntryRequest {
                conversation_id: conversation,
                kind: EntryKind::new("note").expect("entry kind"),
                data: json!({"index": index}),
                projection: Vec::new(),
                context: ContextControl::none(),
            })
            .expect("entry");
    }

    // The retained tail still answers a recent cursor.
    let recent = session.summary().last_commit;
    let tail = session.observations_after(Some(recent));
    assert!(!tail.reset_required);
    assert!(tail.events.is_empty());

    // An evicted cursor asks for a resnapshot instead of lying by omission.
    let evicted = session.observations_after(Some(first));
    assert!(evicted.reset_required);
    assert!(evicted.events.is_empty());
}

/// A cursor outside retained coverage must resolve a wait as `reset_required`
/// rather than as an empty delta: a client told "nothing new" would drop the
/// commits it cannot see.
#[tokio::test]
async fn a_wait_past_retained_coverage_requires_a_resnapshot() {
    let (driver, _) = authoring_driver();
    let start = driver.summary().await.last_commit;
    // Commit past the retained observation capacity (128), so `start` is evicted.
    for _ in 0..200 {
        driver
            .admit_input(InputRequest {
                target: driver.snapshot().await.root_conversation,
                sender: InputSender::User,
                mode: InputMode::QueueOnly,
                request_key: None,
                body: InputBody::Text("filler".to_owned()),
            })
            .await
            .expect("commit");
    }
    let batch = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        driver.wait_observations(Some(start)),
    )
    .await
    .expect("an evicted cursor resolves instead of waiting for a commit")
    .expect("wait");
    assert!(
        batch.reset_required,
        "a cursor outside coverage must ask for a resnapshot"
    );
    assert!(batch.events.is_empty());
}

#[tokio::test]
async fn a_waiter_subscribes_before_it_reads_coverage() {
    let (driver, task_id) = authoring_driver();
    let root = driver.snapshot().await.root_conversation;

    // A commit that already happened is delivered rather than waited past: the
    // wait reads committed state instead of only future notifications.
    let before = driver.summary().await.last_commit;
    driver.drive_task(task_id).await.expect("drive");
    let settled = driver.summary().await.last_commit;
    assert!(settled > before, "the drive committed");
    let batch = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        driver.wait_observations(Some(before)),
    )
    .await
    .expect("a commit that predates the wait must not be waited past")
    .expect("wait");
    assert!(!batch.reset_required);
    assert!(
        batch.events.iter().any(|event| event.commit_seq == settled),
        "the delta contains the commit that landed before the wait"
    );

    // The reproduced gap: read an empty delta, let a commit land, then wait. The
    // subscription exists before the read, so the commit is not lost.
    let current = driver.summary().await.last_commit;
    let empty = driver.observations_after(Some(current)).await;
    assert!(empty.events.is_empty() && !empty.reset_required);
    // A queue-only input commits without opening a turn, so it does not depend
    // on the foreground slot the authoring plan is still holding.
    let committed = driver
        .admit_input(InputRequest {
            target: root,
            sender: InputSender::User,
            mode: InputMode::QueueOnly,
            request_key: None,
            body: InputBody::Text("later".to_owned()),
        })
        .await
        .expect("commit")
        .commit_seq;
    let batch = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        driver.wait_observations(Some(current)),
    )
    .await
    .expect("a commit between the read and the wait must not be lost")
    .expect("wait");
    assert!(
        batch
            .events
            .iter()
            .any(|event| event.commit_seq == committed),
        "the delta contains the commit that landed after the read"
    );

    // A waiter that has nothing to deliver stays pending until something lands.
    let waiting = tokio::spawn({
        let driver = driver.clone();
        let cursor = committed;
        async move { driver.wait_observations(Some(cursor)).await }
    });
    // One yield registers the waiter: `#[tokio::test]` uses the current-thread
    // scheduler and every await before the wait is ready, so this is a barrier
    // rather than a sleep.
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished(), "nothing was committed yet");
    let committed = driver
        .admit_input(InputRequest {
            target: root,
            sender: InputSender::User,
            mode: InputMode::QueueOnly,
            request_key: None,
            body: InputBody::Text("later still".to_owned()),
        })
        .await
        .expect("commit")
        .commit_seq;
    let batch = tokio::time::timeout(std::time::Duration::from_secs(10), waiting)
        .await
        .expect("a waiter asleep at its cursor wakes on the next commit")
        .expect("join")
        .expect("wait");
    assert!(
        batch
            .events
            .iter()
            .any(|event| event.commit_seq == committed),
        "the wake carries the commit that caused it"
    );
}

#[tokio::test]
async fn closing_resolves_a_wait_that_has_nothing_to_deliver() {
    let (driver, task_id) = authoring_driver();
    let cursor = driver.summary().await.last_commit;
    let waiting = tokio::spawn({
        let driver = driver.clone();
        async move { driver.wait_observations(Some(cursor)).await }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());

    driver.close(CloseMode::Graceful).await;
    let error = tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
        .await
        .expect("close must resolve the wait rather than leave it pending")
        .expect("join")
        .expect_err("a closed session cannot deliver observations");
    assert!(
        matches!(error, TaskDriverError::Session(SessionError::Closed)),
        "expected a typed closed result, got {error:?}"
    );
    let _ = task_id;
}
