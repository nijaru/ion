//! K4 read/observation client contract.
//!
//! These tests pin the bounded read surface a live client uses instead of
//! materializing full state, and the observation-recovery rules (retained tail,
//! coverage gap, cursor from an unknown authority).

use std::sync::Arc;

use ion_core::conversation::context::ContextControl;
use ion_core::{
    Change, CommitSeq, EntryKind, PlannedEntry, PlannedTask, RunningTask, Session, SessionError,
    TaskCompletion, TaskContext, TaskDriver, TaskFuture, TaskId, TaskKind, TaskKindName, TaskPlan,
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
                    background: false,
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

#[tokio::test]
async fn changed_wakes_a_waiter_when_a_commit_lands() {
    let (driver, task_id) = authoring_driver();
    let waiter = driver.clone();
    let waiting = tokio::spawn(async move { waiter.changed().await });

    // Give the waiter a chance to subscribe before the commit.
    tokio::task::yield_now().await;
    driver.drive_task(task_id).await.expect("drive");
    tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
        .await
        .expect("waiter woke")
        .expect("waiter joined");
}

#[tokio::test]
async fn changed_does_not_resolve_on_a_commit_that_predates_the_wait() {
    let (driver, task_id) = authoring_driver();
    // Driver construction and task admission already committed, so a waiter
    // must wait for the next commit rather than reporting stale progress.
    let (ready, started) = tokio::sync::oneshot::channel();
    let waiter = driver.clone();
    let waiting = tokio::spawn(async move {
        ready.send(()).expect("the test is waiting");
        waiter.changed().await;
    });
    started.await.expect("the waiter started");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !waiting.is_finished(),
        "changed must not resolve on an earlier commit"
    );

    driver.drive_task(task_id).await.expect("drive");
    tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
        .await
        .expect("waiter woke")
        .expect("waiter joined");
}
