//! Public admission contracts with real Session storage and synthetic models.
use super::*;
use crate::{
    AuthorityScope, CompletionDelivery, CompletionInbox, CompletionNotice, CompletionRevocation,
    CompletionStatus, RequestKey, SessionEntry, ToolOutcome,
};
use ion_ai::{BoxFuture, ModelStream, ModelStreamEvent, Script, ScriptedModelService, ToolCall};
use std::{
    cell::Cell,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

struct SendOnlyMetadata {
    _cell: Cell<()>,
}

struct PreflightService {
    scripts: ScriptedModelService,
    ready: Box<dyn Fn() -> bool + Send + Sync>,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl PreflightService {
    fn new(
        ready: impl Fn() -> bool + Send + Sync + 'static,
        hook: impl FnOnce() + Send + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            scripts: ScriptedModelService::new([Script::Stream(vec![
                ModelStreamEvent::Completed(ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("done".into())],
                        provider_replay: None,
                    },
                    usage: ion_ai::Usage::unknown(),
                    termination: ResponseTermination::Completed,
                    returned_model: None,
                }),
            ])]),
            ready: Box::new(ready),
            hook: Mutex::new(Some(Box::new(hook))),
        })
    }
}

impl ModelService for PreflightService {
    fn validate_controls(
        &self,
        _model: &ModelRef,
        controls: &GenerationControls,
    ) -> Result<(), ProviderError> {
        if (self.ready)() {
            let hook = self.hook.lock().unwrap().take();
            if let Some(hook) = hook {
                hook();
            }
        }
        controls.validate()
    }

    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        self.scripts.stream(request)
    }
}

fn fixture(service: Arc<dyn ModelService>) -> (PathBuf, Arc<Session>, Agent) {
    let root = std::env::temp_dir().join(format!("ion-admission-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let session = Arc::new(Session::create(root.join("session.sqlite"), None).unwrap());
    let agent = Agent::with_tool_set(
        service,
        Arc::new(ToolSet::new([])),
        ModelRef {
            provider: "fixture".into(),
            model: "fixture".into(),
        },
    );
    (root, session, agent)
}

fn request() -> SubmissionRequest {
    SubmissionRequest {
        key: RequestKey::new("fixture".into(), "delivery".into()).unwrap(),
        input: user_text("work".into()),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn keyed_admission_waits_for_sqlite_off_executor_and_reconciles_busy_and_closed() {
    let executor = std::thread::current().id();
    let service = PreflightService::new(
        || true,
        move || {
            assert_ne!(std::thread::current().id(), executor);
        },
    );
    let (root, session, agent) = fixture(service.clone());
    let connection = rusqlite::Connection::open(session.path()).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let released = Arc::new(AtomicBool::new(false));
    let release_flag = released.clone();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        connection.execute_batch("ROLLBACK").unwrap();
        release_flag.store(true, Ordering::SeqCst);
    });
    let admission = agent.submit_request::<_, SendOnlyMetadata>(
        session.clone(),
        request(),
        "test".into(),
        CancellationToken::new(),
        None,
        |_| {},
    );
    let heartbeat = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !released.load(Ordering::SeqCst),
            "SQLite stalled the executor"
        );
    };
    let (submission, ()) = tokio::join!(admission, heartbeat);
    release.join().unwrap();
    let submission = submission.unwrap();
    assert_eq!(submission.receipt.turn, 1);
    assert_eq!(submission.operation.unwrap().await.unwrap(), "done");

    let stop = CancellationToken::new();
    let active_stop = stop.clone();
    let active = session
        .operations
        .start::<_, AgentError, _>(stop.clone(), async move {
            active_stop.cancelled().await;
            Ok(())
        });
    let changed_agent = agent.with_limits(AgentLimits {
        max_request_bytes: 1,
        ..AgentLimits::default()
    });
    for closed in [false, true] {
        if closed {
            stop.cancel();
            session.shutdown().await.unwrap();
        }
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let retry = changed_agent
            .submit_request::<_, ()>(
                session.clone(),
                request(),
                "changed".into(),
                cancelled,
                None,
                |_| {},
            )
            .await
            .unwrap();
        assert!(retry.operation.is_none());
        assert_eq!(retry.receipt.turn, 1);
        assert!(retry.receipt.end.is_some());
        let mut conflicting = request();
        conflicting.input = user_text("different".into());
        assert!(matches!(
            changed_agent
                .submit_request::<_, ()>(
                    session.clone(),
                    conflicting,
                    "changed".into(),
                    CancellationToken::new(),
                    None,
                    |_| {},
                )
                .await,
            Err(AgentError::Session(SessionError::RequestConflict))
        ));
    }
    active.await.unwrap();
    assert_eq!(service.scripts.requests().len(), 1);
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn scoped_resume_admits_off_executor_and_settles_missing_effects_as_unknown() {
    let executor = std::thread::current().id();
    let service = PreflightService::new(
        || true,
        move || {
            assert_ne!(std::thread::current().id(), executor);
        },
    );
    let (root, session, agent) = fixture(service.clone());
    let agent =
        agent.with_recovery_scope(AuthorityScope::new("fixture/principal/routes-v1").unwrap());
    let (binding, _) = agent.admission_binding(&session, "test").unwrap();
    session
        .admit_turn(user_text("work".into()), agent.model.clone(), None, binding)
        .unwrap();
    session
        .record_assistant(
            1,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "effect".into(),
                    name: "effect".into(),
                    arguments: serde_json::json!({}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            ion_ai::Usage::unknown(),
            true,
        )
        .unwrap();
    let point = session.recovery_state().unwrap().unwrap().point;
    let changed_authority = agent
        .clone()
        .with_recovery_scope(AuthorityScope::new("fixture/another-principal/routes-v1").unwrap());
    assert!(matches!(
        changed_authority
            .resume_turn::<_, ()>(
                session.clone(),
                point,
                "test".into(),
                CancellationToken::new(),
                None,
                |_| {},
            )
            .await,
        Err(AgentError::Session(SessionError::RecoveryAuthorityChanged))
    ));
    assert_eq!(session.recovery_state().unwrap().unwrap().point, point);
    let (accepted, operation) = agent
        .resume_turn::<_, SendOnlyMetadata>(
            session.clone(),
            point,
            "test".into(),
            CancellationToken::new(),
            None,
            |_| {},
        )
        .await
        .unwrap();
    assert!(accepted.checkpoint > point.checkpoint);
    assert_eq!(operation.await.unwrap(), "done");
    assert!(matches!(
        agent
            .resume_turn::<_, ()>(
                session.clone(),
                point,
                "test".into(),
                CancellationToken::new(),
                None,
                |_| {},
            )
            .await,
        Err(AgentError::Session(SessionError::StaleRecoveryPoint))
    ));
    let entries = session.reader().unwrap().view().await.unwrap().entries;
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, SessionEntry::TurnResumed { .. }))
            .count(),
        1
    );
    assert!(entries.iter().any(|entry| matches!(entry, SessionEntry::ToolResult { result, .. } if matches!(result.outcome, ToolOutcome::Unknown))));
    assert_eq!(
        service.scripts.requests().len(),
        1,
        "recovery must not replay effects"
    );
    session.shutdown().await.unwrap();
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[derive(Clone, Copy)]
enum Failure {
    None,
    Known,
    Uncertain,
}

#[tokio::test(flavor = "current_thread")]
async fn automated_admission_classifies_owned_claims_and_preserves_first_withdrawal() {
    for (failure, withdraw) in [
        (Failure::Known, false),
        (Failure::Known, true),
        (Failure::Uncertain, false),
        (Failure::Uncertain, true),
        (Failure::None, true),
    ] {
        let executor = std::thread::current().id();
        let (root, session, agent) = fixture(PreflightService::new(|| false, || {}));
        let completions = CompletionInbox::new(session.id());
        let permit = completions
            .reserve(0, None, CompletionDelivery::Wake)
            .unwrap();
        let receipt = permit.receipt();
        permit.offer(CompletionNotice {
            source_session: uuid::Uuid::now_v7(),
            run: uuid::Uuid::now_v7(),
            child_session: uuid::Uuid::now_v7(),
        });
        // Withdraw only once selection has claimed the queued readiness.
        let claimed = completions.clone();
        let withdrawn = completions.clone();
        let hook = move || {
            assert_ne!(std::thread::current().id(), executor);
            if withdraw {
                withdrawn.revoke(CompletionRevocation::Stopped);
                permit.withdraw();
                withdrawn.close(CompletionRevocation::Closed);
            }
            // Retain the permit through the storage attempt when not withdrawn.
            permit
        };
        let held_permit = Arc::new(Mutex::new(None));
        let held = held_permit.clone();
        let service = PreflightService::new(
            move || !claimed.has_wake(),
            move || *held.lock().unwrap() = Some(hook()),
        );
        let agent = Agent {
            service: service.clone(),
            ..agent
        };
        let connection = rusqlite::Connection::open(session.path()).unwrap();
        match failure {
            Failure::None => {},
            Failure::Known => connection.execute_batch("CREATE TRIGGER refuse BEFORE INSERT ON entries BEGIN SELECT RAISE(FAIL, 'known rejection'); END;").unwrap(),
            Failure::Uncertain => connection.execute_batch("CREATE TRIGGER refuse BEFORE INSERT ON entries BEGIN SELECT RAISE(ROLLBACK, 'uncertain rollback'); END;").unwrap(),
        }
        let inbox = Arc::new(
            SteeringInbox::<SendOnlyMetadata>::new(agent.limits(), crate::InputBudget::default())
                .with_completions(completions.clone()),
        );
        let result = agent
            .submit_completions(
                session.clone(),
                "test".into(),
                CancellationToken::new(),
                inbox.clone(),
                |_| {},
            )
            .await;
        match failure {
            Failure::None => {
                let submission = result.unwrap().unwrap();
                assert_eq!(
                    receipt.snapshot(),
                    CompletionStatus::Offered {
                        turn: submission.turn
                    }
                );
                assert_eq!(submission.operation.await.unwrap(), "done");
            }
            Failure::Known | Failure::Uncertain => {
                let Err(AgentError::Session(error)) = result else {
                    panic!("injected storage failure was not reported")
                };
                assert_eq!(
                    error.is_commit_uncertain(),
                    matches!(failure, Failure::Uncertain)
                );
                let expected = if matches!(failure, Failure::Uncertain) {
                    CompletionStatus::Unresolved
                } else if withdraw {
                    CompletionStatus::Revoked {
                        reason: CompletionRevocation::Stopped,
                    }
                } else {
                    CompletionStatus::Queued
                };
                assert_eq!(receipt.snapshot(), expected);
                let entries: i64 = connection
                    .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
                    .unwrap();
                assert_eq!(entries, 0, "failed admission must not publish a Turn");
                assert_eq!(
                    completions.has_wake(),
                    matches!(failure, Failure::Known) && !withdraw
                );
                if matches!(failure, Failure::Known) && !withdraw {
                    connection.execute_batch("DROP TRIGGER refuse").unwrap();
                    let submission = agent
                        .submit_completions(
                            session.clone(),
                            "test".into(),
                            CancellationToken::new(),
                            inbox,
                            |_| {},
                        )
                        .await
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        receipt.snapshot(),
                        CompletionStatus::Offered {
                            turn: submission.turn
                        }
                    );
                    assert_eq!(submission.operation.await.unwrap(), "done");
                }
            }
        }
        session.shutdown().await.unwrap();
        drop(held_permit);
        drop(connection);
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_keyed_admission_still_commits_and_retry_does_not_restart_it() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let worker_gate = gate.clone();
    let worker_entered = entered.clone();
    let executor = std::thread::current().id();
    let service = PreflightService::new(
        || true,
        move || {
            assert_ne!(std::thread::current().id(), executor);
            worker_entered.notify_one();
            let (lock, changed) = &*worker_gate;
            let (_released, timeout) = changed
                .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |released| {
                    !*released
                })
                .unwrap();
            assert!(!timeout.timed_out());
        },
    );
    let (root, session, agent) = fixture(service);
    let admitting_agent = agent.clone();
    let admitting_session = session.clone();
    let admitting = tokio::spawn(async move {
        admitting_agent
            .submit_request::<_, ()>(
                admitting_session,
                request(),
                "test".into(),
                CancellationToken::new(),
                None,
                |_| {},
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    admitting.abort();
    assert!(matches!(admitting.await, Err(error) if error.is_cancelled()));
    let (lock, changed) = &*gate;
    *lock.lock().unwrap() = true;
    changed.notify_one();
    let reader = session.reader().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if reader
                .request_receipt(&request().key)
                .await
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let retry = agent
        .submit_request::<_, ()>(
            session.clone(),
            request(),
            "test".into(),
            CancellationToken::new(),
            None,
            |_| panic!("reconciliation must not restart execution"),
        )
        .await
        .unwrap();
    assert_eq!(retry.receipt.turn, 1);
    assert!(retry.operation.is_none());
    session.shutdown().await.unwrap();
    let entries = session.reader().unwrap().view().await.unwrap().entries;
    assert_eq!(
        entries
            .iter()
            .filter(|entry| matches!(entry, SessionEntry::TurnStarted { .. }))
            .count(),
        1
    );
    drop(reader);
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn keyed_uncertain_commit_is_not_replaced_by_faulted_reconciliation() {
    let (root, session, agent) = fixture(PreflightService::new(|| false, || {}));
    let connection = rusqlite::Connection::open(session.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER refuse BEFORE INSERT ON entries BEGIN SELECT RAISE(ROLLBACK, 'uncertain rollback'); END;").unwrap();
    let result = agent
        .submit_request::<_, ()>(
            session.clone(),
            request(),
            "test".into(),
            CancellationToken::new(),
            None,
            |_| {},
        )
        .await;
    assert!(matches!(result, Err(AgentError::Session(error)) if error.is_commit_uncertain()));
    let entries: i64 = connection
        .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
        .unwrap();
    assert_eq!(entries, 0);
    session.shutdown().await.unwrap();
    drop(connection);
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

// A Send-only observer whose destructor reenters the operation owner.
struct ReentrantDrop {
    session: Arc<Session>,
    dropped: Arc<AtomicBool>,
    _not_sync: Cell<u8>,
}
impl Drop for ReentrantDrop {
    fn drop(&mut self) {
        assert!(self.session.ensure_idle().is_ok());
        self.dropped.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn preflight_errors_drop_send_only_observers_outside_operation_gate() {
    for admission in ["keyed", "resume", "completion"] {
        let service = PreflightService::new(|| false, || {});
        let (root, session, agent) = fixture(service);
        let dropped = Arc::new(AtomicBool::new(false));
        let probe = ReentrantDrop {
            session: session.clone(),
            dropped: dropped.clone(),
            _not_sync: Cell::new(0),
        };
        let observer = move |_| {
            let _ = &probe;
        };
        let stop = CancellationToken::new();
        stop.cancel();
        match admission {
            "keyed" => assert!(matches!(
                agent
                    .submit_request::<_, ()>(
                        session.clone(),
                        request(),
                        "test".into(),
                        stop,
                        None,
                        observer
                    )
                    .await,
                Err(AgentError::Cancelled)
            )),
            "resume" => assert!(matches!(
                agent
                    .resume_turn::<_, ()>(
                        session.clone(),
                        crate::RecoveryPoint {
                            session: session.id(),
                            turn: 1,
                            checkpoint: 1
                        },
                        "test".into(),
                        stop,
                        None,
                        observer
                    )
                    .await,
                Err(AgentError::Cancelled)
            )),
            "completion" => {
                let completions = CompletionInbox::new(session.id());
                let inbox = Arc::new(
                    SteeringInbox::<()>::new(agent.limits(), crate::InputBudget::default())
                        .with_completions(completions),
                );
                assert!(matches!(
                    agent
                        .submit_completions(session.clone(), "test".into(), stop, inbox, observer)
                        .await,
                    Err(AgentError::Cancelled)
                ));
            }
            _ => unreachable!(),
        }
        assert!(dropped.load(Ordering::SeqCst));
        session.shutdown().await.unwrap();
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}
