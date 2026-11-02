use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use tokio::sync::Notify;

use super::*;
use crate::{OperationError, SessionError, SessionIo};

struct Destination(uuid::Uuid);

impl BackgroundSlot for Destination {
    fn owner(&self) -> uuid::Uuid {
        self.0
    }

    fn reserve(&mut self) -> Result<CancellationToken, String> {
        Ok(CancellationToken::new())
    }

    fn install<'a>(
        self: Box<Self>,
        _: &'a mut BackgroundJob,
    ) -> BoxFuture<'a, Result<DelegationReceipt, BackgroundStartError>> {
        unreachable!("these tests supervise the actual transferred operation directly")
    }
}

struct Fixture {
    root: PathBuf,
    parent: Arc<Session>,
    job: BackgroundJob,
    owner: uuid::Uuid,
    destination: BackgroundTransfer,
    calls: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    finish: Arc<Notify>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ion-delegation-io-{}", uuid::Uuid::new_v4()));
        let parent = Arc::new(Session::create(root.join("parent.sqlite"), None).unwrap());
        let model = ModelRef {
            provider: "fixture".into(),
            model: "fixture".into(),
        };
        let (turn, _) = parent.begin_turn("delegate".into(), model.clone()).unwrap();
        parent
            .record_assistant(
                turn,
                Message {
                    role: ion_ai::Role::Assistant,
                    content: vec![ion_ai::Content::ToolCall(ion_ai::ToolCall {
                        id: "delegate".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({}),
                        raw_arguments: None,
                    })],
                    provider_replay: None,
                },
                ion_ai::Usage::unknown(),
                false,
            )
            .unwrap();
        let owner = uuid::Uuid::new_v4();
        let (record, prepared) = parent
            .admit_delegation(
                DelegationSite {
                    session: parent.id(),
                    turn,
                    tool: parent.tool_occurrence(turn, "delegate").unwrap(),
                    composed_child: None,
                },
                DelegationSpec {
                    target: DelegationTarget::Fresh {
                        directory: root.join("children"),
                    },
                    cwd: None,
                    model,
                    reasoning: Reasoning::ProviderDefault,
                    execution_timeout_ms: None,
                    authority: AuthorityScope::new("fixture").unwrap(),
                    input: Message::user_input("child".into(), []),
                },
                DelegationOwnership::Background { owner },
            )
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(Notify::new());
        let finish = Arc::new(Notify::new());
        let work: DelegationWork = {
            let calls = calls.clone();
            let entered = entered.clone();
            let finish = finish.clone();
            let model = record.spec.model.clone();
            Box::new(move |child, submission, _| {
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    entered.notify_one();
                    finish.notified().await;
                    // Commit a synthetic ordinary request and answer, without a
                    // model route, so late parent settlement validates a real
                    // completed child receipt rather than an invented outcome.
                    let committed = match child.io() {
                        Ok(io) => {
                            io.run(move |child| {
                                let crate::session::TurnAdmission::New { turn, .. } = child
                                    .admit_turn(
                                        submission.input,
                                        model,
                                        Some(submission.key),
                                        None,
                                    )?
                                else {
                                    return Err(SessionError::InvalidHistory);
                                };
                                child.record_assistant(
                                    turn,
                                    Message {
                                        role: ion_ai::Role::Assistant,
                                        content: vec![ion_ai::Content::Text(
                                            "fixture answer".into(),
                                        )],
                                        provider_replay: None,
                                    },
                                    ion_ai::Usage::unknown(),
                                    false,
                                )
                            })
                            .await
                        }
                        Err(error) => Err(error),
                    };
                    DelegationCompletion {
                        result: committed
                            .map(|_| Some("fixture answer".into()))
                            .map_err(|error| DelegationFailure::Infrastructure(error.to_string())),
                        execution_timed_out: false,
                        elapsed_ms: None,
                        partial_output: None,
                    }
                })
            })
        };
        let mut destination = BackgroundTransfer {
            slot: Box::new(Destination(owner)),
            join_timeout: Duration::ZERO,
        };
        destination.slot.reserve().unwrap();
        let io: Arc<SessionIo> = Arc::new(parent.transferred_io(&destination).unwrap());
        let child_io = Arc::new(parent.transferred_io(&destination).unwrap());
        let job = BackgroundJob::new(
            io,
            child_io,
            record,
            prepared,
            work,
            CancellationToken::new(),
            CancellationToken::new(),
        );
        Self {
            root,
            parent,
            job,
            owner,
            destination,
            calls,
            entered,
            finish,
        }
    }

    fn writer(&self) -> rusqlite::Connection {
        let writer = rusqlite::Connection::open(self.parent.path()).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer
    }
}

async fn reopen(root: &std::path::Path) -> Session {
    // Weak::upgrade can fail before the last Session's destructor releases its
    // file lease. Observe the actual resource, not the Arc's reference count.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match Session::open(root.join("parent.sqlite")) {
                Ok(session) => break session,
                Err(SessionError::AlreadyOpen) => tokio::task::yield_now().await,
                Err(error) => panic!("reopen after actual storage completion: {error}"),
            }
        }
    })
    .await
    .expect("actual storage work must eventually release the writer")
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_transfer_wait_retains_writer_until_actual_commit_without_dispatch() {
    let mut fixture = Fixture::new();
    let writer = fixture.writer();
    let parent = Arc::downgrade(&fixture.parent);
    let mut starting = Box::pin(fixture.job.start(fixture.owner, || {}));
    assert!(futures_util::poll!(&mut starting).is_pending());
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(futures_util::poll!(&mut starting).is_pending());
    drop(starting);
    assert!(
        matches!(
            fixture.job.start(fixture.owner, || {}).await,
            Err(BackgroundStartError::Infrastructure(
                SessionError::InvalidHistory
            ))
        ),
        "an abandoned transfer cannot be retried"
    );
    let Fixture {
        root,
        parent: retained,
        job,
        calls,
        ..
    } = fixture;
    drop(job);
    drop(retained);
    assert!(parent.upgrade().is_some());
    assert!(matches!(
        Session::open(root.join("parent.sqlite")),
        Err(SessionError::AlreadyOpen)
    ));
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    let reopened = reopen(&root).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let receipts = reopened.view().unwrap().delegations();
    assert_eq!(receipts.len(), 1);
    assert!(
        receipts[0].transferred,
        "actual commit survives abandoned wait"
    );
    assert!(
        receipts[0].outcome.is_none(),
        "abandonment cannot attest joining"
    );
    assert!(
        !root.join("children").exists(),
        "cold work must not be replayed"
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn reserved_background_lane_cannot_start_a_new_transfer_after_shutdown() {
    let mut fixture = Fixture::new();
    fixture.parent.shutdown().await.unwrap();
    assert!(matches!(
        fixture.job.start(fixture.owner, || {}).await,
        Err(BackgroundStartError::Refused(_))
    ));
    let receipts = fixture
        .parent
        .reader()
        .unwrap()
        .view()
        .await
        .unwrap()
        .delegations();
    assert_eq!(receipts.len(), 1);
    assert!(!receipts[0].transferred);
    assert!(receipts[0].outcome.is_none());
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert!(!fixture.root.join("children").exists());
    let Fixture {
        root, parent, job, ..
    } = fixture;
    drop(job);
    drop(parent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn transferred_outcome_commits_after_parent_shutdown_but_ordinary_writes_stay_closed() {
    let mut fixture = Fixture::new();
    let ordinary = fixture.parent.io().unwrap();
    let (_, operation) = fixture.job.start(fixture.owner, || {}).await.unwrap();
    // The transferred root has not run on this current-thread executor yet.
    // Its retained materialization capability must also survive this cut.
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.parent.shutdown().await.unwrap();
    assert!(matches!(
        ordinary
            .run(|session| session.set_name(Some("must not write")))
            .await,
        Err(SessionError::Operation(OperationError::Closed))
    ));
    assert!(matches!(
        fixture.parent.io(),
        Err(SessionError::Operation(OperationError::Closed))
    ));
    assert!(
        matches!(
            fixture.parent.transferred_io(&fixture.destination),
            Err(SessionError::Operation(OperationError::Closed))
        ),
        "closed storage cannot mint another transferred capability"
    );
    fixture.entered.notified().await;
    fixture.finish.notify_one();
    let settled = operation.await.unwrap();
    assert!(
        matches!(&settled.outcome, Some(DelegationOutcome::Joined { error: None, receipt: Some(receipt), answer: Some(answer), .. }) if receipt.end == Some(crate::TurnEndReason::Completed) && answer == "fixture answer")
    );
    let recorded = fixture
        .parent
        .reader()
        .unwrap()
        .view()
        .await
        .unwrap()
        .delegations();
    assert_eq!(recorded, vec![settled]);
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "late settlement cannot replay child work"
    );
    fixture.parent.shutdown().await.unwrap();
    drop(ordinary);
    let Fixture {
        root, parent, job, ..
    } = fixture;
    drop(job);
    drop(parent);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_background_result_retains_delayed_settlement_and_never_reexecutes() {
    let mut fixture = Fixture::new();
    let failed = Arc::new(AtomicUsize::new(0));
    let failure_hook = failed.clone();
    let (_, operation) = fixture
        .job
        .start(fixture.owner, move || {
            failure_hook.fetch_add(1, Ordering::SeqCst);
        })
        .await
        .unwrap();
    fixture.entered.notified().await;
    let writer = fixture.writer();
    fixture.finish.notify_one();
    tokio::time::sleep(Duration::from_millis(20)).await;
    let parent = Arc::downgrade(&fixture.parent);
    drop(operation);
    let Fixture {
        root,
        parent: retained,
        job,
        calls,
        ..
    } = fixture;
    drop(job);
    drop(retained);
    assert!(
        parent.upgrade().is_some(),
        "unread result cannot abandon actual settlement"
    );
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    let reopened = reopen(&root).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(failed.load(Ordering::SeqCst), 0);
    let receipts = reopened.view().unwrap().delegations();
    assert_eq!(receipts.len(), 1);
    assert!(receipts[0].transferred);
    assert!(
        matches!(&receipts[0].outcome, Some(DelegationOutcome::Joined { error: None, receipt: Some(receipt), answer: Some(answer), .. }) if receipt.end == Some(crate::TurnEndReason::Completed) && answer == "fixture answer")
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}
