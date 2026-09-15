//! The invocation request basis: an invocation freezes its transcript boundary
//! before it reads a single entry, so history that lands while the pages are
//! being read cannot join a request whose boundary was already decided.
//!
//! These tests drive the real driver: a probe kind captures a basis, waits on a
//! barrier while another task commits an entry, and then reports what it read.

use std::sync::Arc;
use std::time::Duration;

use ion_core::conversation::context::ContextControl;
use ion_core::{
    ContextCut, ConversationId, ConversationSpec, EntryKind, EntryRequest, PlannedEntry,
    PlannedTarget, RequestBasis, RunningTask, Session, TaskCompletion, TaskContext, TaskDriver,
    TaskFuture, TaskId, TaskKind, TaskKindName, TaskPlan, TaskRegistry, TaskRequest, TaskStatus,
};
use serde_json::json;
use tokio::sync::Notify;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

struct Gate {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

/// Captures a basis, hands control to the test, then pages inside the basis.
struct Probe {
    gate: Arc<Gate>,
}

impl TaskKind for Probe {
    fn execute<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let basis = context.request_basis().await?;
            self.gate.entered.notify_one();
            self.gate.release.notified().await;

            let mut kinds = Vec::new();
            let mut after = None;
            loop {
                let page = context.request_entries(&basis, after, 4).await?;
                for entry in &page.entries {
                    kinds.push(entry.kind.as_str().to_owned());
                }
                match page.next {
                    Some(next) => after = Some(next),
                    None => break,
                }
            }

            // A basis names the conversation it was captured for, and the
            // runtime refuses to page another one through it.
            let foreign = task
                .input
                .get("foreign")
                .and_then(serde_json::Value::as_i64)
                .and_then(|value| ConversationId::new(value).ok())
                .ok_or_else(|| ion_core::TaskRunError::new("probe needs a foreign conversation"))?;
            let refused = context
                .request_entries(
                    &RequestBasis {
                        conversation_id: foreign,
                        cut: basis.cut,
                        placed: Vec::new(),
                    },
                    None,
                    4,
                )
                .await;
            assert!(
                refused.is_err(),
                "a basis must not page another conversation's history"
            );

            let cut = match basis.cut {
                ContextCut::Empty => json!("empty"),
                ContextCut::Through(entry) => json!(entry.get()),
            };
            Ok(TaskCompletion::completed(
                json!({"cut": cut, "kinds": kinds, "placed": basis.placed.len()}),
            ))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

/// Appends one entry to the probe's conversation through a plan, from its own
/// turn in a different conversation.
struct Appender {
    target: ConversationId,
    label: &'static str,
}

impl TaskKind for Appender {
    fn execute<'a>(&'a self, _task: RunningTask, _context: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let mut plan = TaskPlan::new();
            plan.append_entry(PlannedEntry {
                conversation_id: PlannedTarget::Existing(self.target),
                kind: EntryKind::new(self.label).expect("entry kind"),
                data: json!({"text": self.label}),
                projection: Vec::new(),
                context: ContextControl::none(),
            });
            Ok(TaskCompletion::completed(json!("appended")).with_plan(plan))
        })
    }

    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }

    fn abort<'a>(&'a self, _task: RunningTask, _context: ion_core::AbortContext) -> TaskFuture<'a> {
        Box::pin(async { Ok(TaskCompletion::aborted(json!("aborted"))) })
    }
}

fn registry(gate: Arc<Gate>) -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    registry
        .register(kind("probe"), 1, Arc::new(Probe { gate }))
        .expect("probe");
    registry
}

fn turn(conversation_id: ConversationId, task_kind: &str, input: serde_json::Value) -> TaskRequest {
    TaskRequest {
        conversation_id,
        kind: kind(task_kind),
        schema_version: 1,
        input,
        dependencies: Vec::new(),
    }
}

fn note(conversation_id: ConversationId, text: &str) -> EntryRequest {
    EntryRequest {
        conversation_id,
        kind: EntryKind::new("note").expect("entry kind"),
        data: json!({"text": text}),
        projection: Vec::new(),
        context: ContextControl::none(),
    }
}

async fn wait(driver: &TaskDriver, task: TaskId) -> ion_core::TaskOutcome {
    let record = tokio::time::timeout(Duration::from_secs(10), driver.wait_task(task))
        .await
        .expect("no stall")
        .expect("wait");
    let TaskStatus::Terminal(outcome) = record.status else {
        panic!("the probe settles");
    };
    outcome
}

/// The whole point of the basis: an entry committed after the boundary was
/// captured is not part of the request, even though the request read its pages
/// later and the client can see the append.
#[tokio::test]
async fn a_basis_excludes_an_append_that_lands_after_it() {
    let gate = Arc::new(Gate {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    });
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let before = session
        .append_entry(note(root, "before"))
        .expect("commit")
        .entry_id;
    // The appender's own turn lives elsewhere, so it does not contend for the
    // probe conversation's foreground slot.
    let elsewhere = session
        .create_conversation(ConversationSpec::independent())
        .expect("conversation")
        .conversation_id;
    let mut registry = registry(gate.clone());
    registry
        .register(
            kind("append"),
            1,
            Arc::new(Appender {
                target: root,
                label: "after",
            }),
        )
        .expect("appender");
    let driver = TaskDriver::new(session, registry);

    let probe = driver
        .create_turn(turn(root, "probe", json!({"foreign": elsewhere.get()})))
        .await
        .expect("probe turn")
        .task_id;
    let appender = driver
        .create_turn(turn(elsewhere, "append", json!({})))
        .await
        .expect("appender turn")
        .task_id;

    let running = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(probe).await }
    });
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .expect("the probe captures its basis");

    // The append commits while the probe is suspended inside its request.
    driver.drive_task(appender).await.expect("append");
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("the probe finishes")
        .expect("join")
        .expect("drive");

    let outcome = wait(&driver, probe).await;
    assert_eq!(
        outcome.value["kinds"],
        json!(["note"]),
        "the request sees only what existed when it froze its boundary"
    );
    assert_eq!(
        outcome.value["cut"],
        json!(before.get()),
        "the frozen boundary is the pre-existing entry, not the appended one"
    );

    // The same session now shows both entries, so the exclusion came from the
    // request's boundary rather than from the append failing.
    let page = driver
        .conversation_entries(root, None, 8)
        .await
        .expect("transcript");
    let kinds: Vec<&str> = page
        .entries
        .iter()
        .map(|entry| entry.kind.as_str())
        .collect();
    assert_eq!(kinds, vec!["note", "after"]);
    driver.close(ion_core::CloseMode::Graceful).await;
}

/// `Empty` is a boundary, not "unbounded": a request that saw no history stays
/// empty even after the conversation gains one.
#[tokio::test]
async fn an_empty_cut_stays_empty() {
    let gate = Arc::new(Gate {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    });
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let elsewhere = session
        .create_conversation(ConversationSpec::independent())
        .expect("conversation")
        .conversation_id;
    let mut registry = registry(gate.clone());
    registry
        .register(
            kind("append"),
            1,
            Arc::new(Appender {
                target: root,
                label: "first",
            }),
        )
        .expect("appender");
    let driver = TaskDriver::new(session, registry);

    let probe = driver
        .create_turn(turn(root, "probe", json!({"foreign": elsewhere.get()})))
        .await
        .expect("probe turn")
        .task_id;
    let appender = driver
        .create_turn(turn(elsewhere, "append", json!({})))
        .await
        .expect("appender turn")
        .task_id;

    let running = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(probe).await }
    });
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .expect("the probe captures an empty basis");
    driver.drive_task(appender).await.expect("append");
    gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("the probe finishes")
        .expect("join")
        .expect("drive");

    let outcome = wait(&driver, probe).await;
    assert_eq!(outcome.value["cut"], json!("empty"));
    assert_eq!(
        outcome.value["kinds"],
        json!([]),
        "an empty boundary must not widen when history appears"
    );
    assert_eq!(
        driver
            .conversation_entries(root, None, 8)
            .await
            .expect("transcript")
            .entries
            .len(),
        1,
        "the append itself still committed"
    );
    driver.close(ion_core::CloseMode::Graceful).await;
}
