use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::*;
use crate::session::state::apply_mutation;
use crate::session::transaction::{Mutation, MutationBatch};
use crate::store::StoreError;
use crate::{
    AbortContext, CloseMode, RunningTask, TaskCompletion, TaskContext, TaskDriver, TaskFuture,
    TaskKind, TaskKindName, TaskRegistry,
};
use tokio::sync::Notify;

#[derive(Debug)]
struct FaultStore(Arc<AtomicBool>);
impl Persistence for FaultStore {
    fn commit(&mut self, _: &MutationBatch) -> Result<(), StoreError> {
        if self.0.load(Ordering::SeqCst) {
            Err(StoreError("injected failure".into()))
        } else {
            Ok(())
        }
    }
}
fn create(session: &mut Session) -> TaskId {
    session
        .create_task(TaskRequest {
            conversation_id: session.root_conversation(),
            kind: TaskKindName::new("live").unwrap(),
            schema_version: 1,
            input: Value::Null,
            dependencies: vec![],
        })
        .unwrap()
        .task_id
}

/// A commit must not deep-copy unrelated durable records. Records are shared by
/// `Arc`, so an untouched task keeps its allocation across another task's
/// checkpoint, while the touched record is replaced copy-on-write.
#[test]
fn commit_copies_only_touched_task_records() {
    let mut session = Session::new().expect("session");
    let touched = create(&mut session);
    let untouched = create(&mut session);
    let invocation = session
        .reserve_task_invocation(touched, InvocationKind::Execute)
        .expect("reserve");

    let touched_before = session.task_record_ptr(touched).expect("touched record");
    let untouched_before = session
        .task_record_ptr(untouched)
        .expect("untouched record");

    session
        .checkpoint_task(
            touched,
            invocation.generation,
            Some(serde_json::json!({"phase": "copy-on-write"})),
            None,
        )
        .expect("checkpoint");

    assert_eq!(
        session.task_record_ptr(untouched),
        Some(untouched_before),
        "an untouched task must keep its resident allocation"
    );
    assert_ne!(
        session.task_record_ptr(touched),
        Some(touched_before),
        "the touched task is replaced copy-on-write"
    );
    assert_eq!(
        session.task_record(touched).unwrap().checkpoint,
        Some(serde_json::json!({"phase": "copy-on-write"}))
    );
}

#[test]
fn failed_persistence_does_not_install_draft_or_publish_successors() {
    let mut session = Session::new().unwrap();
    let task = create(&mut session);
    let invocation = session
        .reserve_task_invocation(task, InvocationKind::Execute)
        .unwrap();
    let before = session.snapshot();
    session.store = Box::new(FaultStore(Arc::new(AtomicBool::new(true))));
    let outcome = TaskCompletion::completed(Value::Null).outcome;
    assert!(matches!(
        session.settle_task_with(task, invocation.generation, outcome, None, |transaction| {
            transaction.create_conversation(ConversationSpec {
                parent: None,
                owner_task: Some(task),
            })
        }),
        Err(SessionError::Persistence(_))
    ));
    assert_eq!(session.snapshot(), before);
    assert!(
        session
            .observations_after(Some(before.last_commit))
            .events
            .is_empty()
    );
    assert!(session.fault_signal().is_cancelled());
    assert!(matches!(
        session.mark_task_cancellation(task),
        Err(SessionError::Closed)
    ));
}

struct Live {
    entered: Arc<Notify>,
    dropped: Arc<Notify>,
}
struct DropSignal(Arc<Notify>);
impl Drop for DropSignal {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}
impl TaskKind for Live {
    fn execute<'a>(&'a self, _: RunningTask, _: TaskContext) -> TaskFuture<'a> {
        Box::pin(async move {
            let _guard = DropSignal(self.dropped.clone());
            self.entered.notify_one();
            std::future::pending().await
        })
    }
    fn recover<'a>(&'a self, task: RunningTask, context: TaskContext) -> TaskFuture<'a> {
        self.execute(task, context)
    }
    fn abort<'a>(&'a self, _: RunningTask, _: AbortContext) -> TaskFuture<'a> {
        panic!("fault is not durable cancellation")
    }
}

#[tokio::test]
async fn store_failure_fences_and_joins_live_invocations() {
    let mut session = Session::new().unwrap();
    let task = create(&mut session);
    let fail = Arc::new(AtomicBool::new(false));
    session.store = Box::new(FaultStore(fail.clone()));
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    let mut registry = TaskRegistry::default();
    registry
        .register(
            TaskKindName::new("live").unwrap(),
            1,
            Arc::new(Live {
                entered: entered.clone(),
                dropped: dropped.clone(),
            }),
        )
        .unwrap();
    let driver = TaskDriver::new(session, registry);
    let running = tokio::spawn({
        let driver = driver.clone();
        async move { driver.drive_task(task).await }
    });
    entered.notified().await;
    let before = driver.snapshot().await;
    fail.store(true, Ordering::SeqCst);
    assert!(driver.cancel_task(task).await.is_err());
    dropped.notified().await;
    assert!(running.await.unwrap().is_err());
    assert_eq!(driver.snapshot().await, before);
    driver.close(CloseMode::Fault).await;
}

/// A store that only records committed write sets. If resident semantics can be
/// rebuilt from these batches alone, the persistence contract is sufficient for
/// a real backend (SQLite in K4).
#[derive(Debug, Clone, Default)]
struct RecordingStore {
    batches: Arc<std::sync::Mutex<Vec<MutationBatch>>>,
}

impl Persistence for RecordingStore {
    fn commit(&mut self, batch: &MutationBatch) -> Result<(), StoreError> {
        self.batches
            .lock()
            .expect("batch mutex")
            .push(batch.clone());
        Ok(())
    }
}

fn replay(batches: &[MutationBatch], session_id: crate::SessionId) -> SessionState {
    let mut state = SessionState::empty(session_id);
    for batch in batches {
        for write in &batch.writes {
            apply_mutation(&mut state, write).expect("committed write applies");
            if let Mutation::AdmitInput(input) = write {
                state.input_commits.insert(input.id, batch.commit_seq);
            }
        }
        state.last_seq = Some(batch.last_seq);
        state.last_commit = Some(batch.commit_seq);
    }
    state
}

/// The committed write set must reconstruct equivalent semantic records,
/// including same-batch references (owned conversation + reciprocal task link,
/// and planned successor entries/tasks) and duplicate-input receipt mappings.
#[test]
fn committed_write_set_reconstructs_resident_state() {
    use crate::conversation::context::ContextControl;
    use crate::{
        ConversationSpec, EntryKind, EntryRequest, InputBody, InputMode, InputRequest, InputSender,
        InvocationKind, PlannedEntry, PlannedTask, PlannedTurn, TaskPlan,
    };

    let recording = RecordingStore::default();
    let mut session =
        Session::with_store(crate::SessionId::new(), Box::new(recording.clone())).expect("session");
    let root = session.root_conversation();

    session
        .create_conversation(ConversationSpec::independent())
        .expect("conversation");
    session
        .append_entry(EntryRequest {
            conversation_id: root,
            kind: EntryKind::new("user").expect("entry kind"),
            data: serde_json::json!({"text": "hello"}),
            projection: Vec::new(),
            context: ContextControl::none(),
        })
        .expect("entry");
    session
        .queue_input(InputRequest {
            target: root,
            sender: InputSender::User,
            mode: InputMode::Submit,
            request_key: Some(crate::RequestKey::new("key").expect("key")),
            body: InputBody::Text("go".to_owned()),
        })
        .expect("input");

    // A foreground turn with a checkpoint, then an atomic finalization plan that
    // creates an entry plus two successors, one background.
    let turn = session
        .create_turn(TaskRequest {
            conversation_id: root,
            kind: TaskKindName::new("generation").expect("kind"),
            schema_version: 1,
            input: serde_json::json!({"prompt": "hi"}),
            dependencies: Vec::new(),
        })
        .expect("turn");
    let invocation = session
        .reserve_task_invocation(turn.task_id, InvocationKind::Execute)
        .expect("reserve");
    session
        .checkpoint_task(
            turn.task_id,
            invocation.generation,
            Some(serde_json::json!({"attempt": 1})),
            None,
        )
        .expect("checkpoint");
    let mut plan = TaskPlan::new();
    plan.append_entry(PlannedEntry {
        conversation_id: (root).into(),
        kind: EntryKind::new("assistant").expect("entry kind"),
        data: serde_json::json!({"text": "calling tools"}),
        projection: Vec::new(),
        context: ContextControl::none(),
    });
    let scoped = plan.create_task(PlannedTask {
        conversation_id: (root).into(),
        kind: TaskKindName::new("tool").expect("kind"),
        schema_version: 1,
        input: serde_json::json!({"call": "a"}),
        dependencies: Vec::new(),
        turn: PlannedTurn::Inherit,
    });
    plan.create_task(PlannedTask {
        conversation_id: (root).into(),
        kind: TaskKindName::new("tool").expect("kind"),
        schema_version: 1,
        input: serde_json::json!({"call": "b"}),
        dependencies: vec![crate::TaskDependency::Planned(scoped)],
        turn: PlannedTurn::Background,
    });
    session
        .settle_task_with(
            turn.task_id,
            invocation.generation,
            TaskCompletion::completed(serde_json::json!("dispatched")).outcome,
            None,
            |transaction| transaction.apply_task_plan(&plan, turn.task_id),
        )
        .expect("settle with plan");

    let batches = recording.batches.lock().expect("batch mutex").clone();
    let reconstructed = replay(&batches, session.session_id());
    assert_eq!(reconstructed, session.state);
    assert_eq!(reconstructed.input_commits.len(), 1);
}
