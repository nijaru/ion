use futures_util::future::BoxFuture;
use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    CodingAgent, CodingAgentError, CodingSession, CodingSessionError, CodingToolOutput,
    CodingToolSource, OperationError, SessionEntry, ToolActivityKind, ToolDefinition, ToolExecutor,
    ToolExposure, ToolOutcome, ToolPresentation, ToolRegistration, TurnEndReason,
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

// Noninterruptible, started synchronous I/O must be joined even after its
// asynchronous caller disappears. The gate makes the ordering deterministic.
struct BlockingWrite {
    started: Arc<tokio::sync::Semaphore>,
    release: Arc<(Mutex<bool>, Condvar)>,
    finished: Arc<AtomicBool>,
    path: PathBuf,
}
impl BlockingWrite {
    fn release(&self) {
        let (lock, ready) = &*self.release;
        *lock.lock().unwrap() = true;
        ready.notify_all();
    }
}
struct ReleaseOnDrop(Arc<BlockingWrite>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}
impl CodingToolSource for BlockingWrite {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition {
                spec: ToolSpec {
                    name: "blocking_write".into(),
                    description: "controlled blocking I/O".into(),
                    input_schema: serde_json::json!({"type":"object","properties":{}}),
                },
                presentation: ToolPresentation::argument(ToolActivityKind::Write, "path"),
                exposure: ToolExposure::Direct,
            },
            self,
        )]
    }
}
impl ToolExecutor for BlockingWrite {
    fn execute<'a>(
        &'a self,
        invocation: ion_core::ToolInvocation<'a>,
    ) -> BoxFuture<'a, CodingToolOutput> {
        let stop = invocation.stop().clone();
        Box::pin(async move {
            let release = self.release.clone();
            let started = self.started.clone();
            let finished = self.finished.clone();
            let path = self.path.clone();
            let mut worker = tokio::task::spawn_blocking(move || {
                let (lock, ready) = &*release;
                let mut opened = lock.lock().unwrap();
                started.add_permits(1);
                while !*opened {
                    opened = ready.wait(opened).unwrap();
                }
                fs::write(path, "once").unwrap();
                finished.store(true, Ordering::SeqCst);
            });
            tokio::select! { result=&mut worker => { result.unwrap(); }, ()=stop.cancelled() => { worker.await.unwrap(); } }
            CodingToolOutput {
                value: serde_json::json!({"written":true}),
                images: Vec::new(),
                is_error: false,
            }
        })
    }
}
fn response(content: Vec<Content>) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
            provider_replay: None,
        },
        usage: Usage::unknown(),
        termination: ResponseTermination::Completed,
        returned_model: Some("test".into()),
    })])
}
fn model() -> ModelRef {
    ModelRef {
        provider: "test".into(),
        model: "test".into(),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_timed_out_and_aborted_callers_cannot_release_started_work() {
    for mode in ["drop", "timeout", "abort"] {
        let root =
            std::env::temp_dir().join(format!("ion-owned-custody-{mode}-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let tools = Arc::new(BlockingWrite {
            started: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new((Mutex::new(false), Condvar::new())),
            finished: Arc::new(AtomicBool::new(false)),
            path: root.join("effect"),
        });
        let _release_on_failure = ReleaseOnDrop(tools.clone());
        let service = Arc::new(ScriptedModelService::new([
            response(vec![Content::ToolCall(ToolCall {
                id: "old".into(),
                name: "blocking_write".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            })]),
            response(vec![Content::Text("fresh".into())]),
        ]));
        let agent = Arc::new(CodingAgent::new(service.clone(), tools.clone(), model()));
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let stop = CancellationToken::new();
        let operation = agent.submit(
            session.clone(),
            "old".into(),
            String::new(),
            stop.clone(),
            |_| {},
        );
        if mode == "abort" {
            let caller = tokio::spawn(operation);
            tokio::time::timeout(Duration::from_secs(4), tools.started.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
        } else {
            tokio::time::timeout(Duration::from_secs(4), tools.started.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
            if mode == "timeout" {
                assert!(
                    tokio::time::timeout(Duration::from_millis(5), operation)
                        .await
                        .is_err()
                );
            } else {
                drop(operation);
            }
        }
        let stopped = stop.is_cancelled();
        let blocked = !tools.finished.load(Ordering::SeqCst);
        let premature = agent
            .submit(
                session.clone(),
                "must wait".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await;
        let preference = agent.select_model(&session);
        let before = session.view().unwrap();
        let absent_before_release = !tools.path.exists();
        tools.release();
        tokio::time::timeout(Duration::from_secs(4), async {
            while matches!(
                session.ensure_idle(),
                Err(CodingSessionError::OperationActive)
            ) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        let settled = session.view().unwrap();
        let fresh = agent
            .submit(
                session.clone(),
                "fresh".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            )
            .await;
        let effect = fs::read_to_string(&tools.path).unwrap();
        session.shutdown().await.unwrap();
        drop(session);
        fs::remove_dir_all(root).unwrap();
        assert!(stopped && blocked && absent_before_release, "{mode}");
        assert!(
            matches!(
                premature,
                Err(CodingAgentError::Session(
                    CodingSessionError::OperationActive
                ))
            ),
            "{mode}: {premature:?}"
        );
        assert!(
            matches!(
                preference,
                Err(CodingAgentError::Session(
                    CodingSessionError::OperationActive
                ))
            ),
            "{mode}"
        );
        assert_eq!(before.turns().len(), 1);
        assert!(before.turns()[0].end.is_none());
        assert_eq!(settled.turns()[0].end, Some(TurnEndReason::Cancelled));
        assert!(settled.entries.iter().any(|entry|matches!(entry,SessionEntry::ToolResult { result,.. } if matches!(&result.outcome,ToolOutcome::Observed { output,.. } if output.value["written"]==true))));
        assert_eq!(fresh.unwrap(), "fresh");
        assert_eq!(effect, "once");
        assert_eq!(service.requests().len(), 2);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn stop_before_admission_is_inert_and_genuine_task_panic_closes_live_execution() {
    let root = std::env::temp_dir().join(format!("ion-owned-fault-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let stop = CancellationToken::new();
    stop.cancel();
    let cancelled = session
        .run_user_shell("must not admit".into(), false, stop, |_, _| async {
            panic!("pre-stopped work executed")
        })
        .await;
    assert!(matches!(
        cancelled,
        Err(CodingSessionError::UserShellCancelled)
    ));
    assert_eq!(session.entry_count().unwrap(), 0);
    let stop = CancellationToken::new();
    let failed = session
        .run_user_shell("interrupted".into(), false, stop.clone(), |_, _| async {
            panic!("controlled execution task panic before host work")
        })
        .await;
    let denied = session
        .run_user_shell(
            "must not resume".into(),
            false,
            CancellationToken::new(),
            |_, _| async { panic!("poisoned execution resumed") },
        )
        .await;
    let preference = session.select_model(model());
    let shutdown = session.shutdown().await;
    let view = session.view().unwrap();
    drop(session);
    fs::remove_dir_all(root).unwrap();
    assert!(stop.is_cancelled());
    for result in [failed, denied] {
        assert!(matches!(
            result,
            Err(CodingSessionError::Operation(OperationError::TaskFailed(_)))
        ));
    }
    assert!(matches!(
        preference,
        Err(CodingSessionError::Operation(OperationError::TaskFailed(_)))
    ));
    assert!(matches!(
        shutdown,
        Err(CodingSessionError::Operation(OperationError::TaskFailed(_)))
    ));
    assert_eq!(view.entries.len(), 1);
    assert!(matches!(
        view.entries[0],
        SessionEntry::UserShellAdmitted { .. }
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn unread_settlement_failure_survives_shutdown_and_blocks_silent_replacement() {
    for abandoned in [true, false] {
        let root = std::env::temp_dir().join(format!("ion-owned-storage-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
        let fault = rusqlite::Connection::open(&path).unwrap();
        fault.execute_batch("CREATE TRIGGER refuse_settlement BEFORE INSERT ON entries WHEN json_extract(CAST(NEW.body AS TEXT),'$.kind')='user_shell_settled' BEGIN SELECT RAISE(ABORT,'result unavailable'); END;").unwrap();
        let effect = root.join("effect");
        let target = effect.clone();
        let ready = Arc::new(tokio::sync::Semaphore::new(0));
        let started = ready.clone();
        let (release, held) = tokio::sync::oneshot::channel();
        let operation = session.run_user_shell(
            "write".into(),
            false,
            CancellationToken::new(),
            move |_, _| async move {
                started.add_permits(1);
                held.await.unwrap();
                fs::write(target, "once").unwrap();
                shell_output()
            },
        );
        ready.acquire().await.unwrap().forget();
        let retained = if abandoned {
            drop(operation);
            None
        } else {
            Some(operation)
        };
        release.send(()).unwrap();
        let failure = session.shutdown().await;
        let delivered = if let Some(operation) = retained {
            Some(operation.await)
        } else {
            None
        };
        let preference = session.select_model(model());
        let before = session.entry_count().unwrap();
        let denied = session
            .run_user_shell(
                "must not rerun".into(),
                false,
                CancellationToken::new(),
                |_, _| async { panic!("failed/closed owner dispatched") },
            )
            .await;
        let unchanged = session.entry_count().unwrap() == before;
        let effect = fs::read_to_string(effect).unwrap();
        let view = session.view().unwrap();
        drop(fault);
        drop(session);
        fs::remove_dir_all(root).unwrap();
        assert!(
            matches!(failure,Err(CodingSessionError::Operation(OperationError::UnreportedFailure(error))) if error.contains("result unavailable"))
        );
        if abandoned {
            assert!(matches!(
                preference,
                Err(CodingSessionError::Operation(
                    OperationError::UnreportedFailure(_)
                ))
            ));
            assert!(matches!(
                denied,
                Err(CodingSessionError::Operation(
                    OperationError::UnreportedFailure(_)
                ))
            ));
        } else {
            assert!(matches!(
                delivered,
                Some(Err(CodingSessionError::Sqlite(_)))
            ));
            assert!(matches!(
                preference,
                Err(CodingSessionError::Operation(OperationError::Closed))
            ));
            assert!(matches!(
                denied,
                Err(CodingSessionError::Operation(OperationError::Closed))
            ));
        }
        assert!(unchanged);
        assert_eq!(effect, "once");
        assert_eq!(view.entries.len(), 1);
        assert!(matches!(
            view.entries[0],
            SessionEntry::UserShellAdmitted { .. }
        ));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn idle_reservation_excludes_concurrent_admission_and_releases_on_preparation_failure() {
    use futures_util::FutureExt;
    let root = std::env::temp_dir().join(format!("ion-owned-reservation-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let idle = session.reserve_idle().unwrap();
    let contender = session.clone();
    let runtime = tokio::runtime::Handle::current();
    let denied = std::thread::spawn(move || {
        let _runtime = runtime.enter();
        contender
            .run_user_shell(
                "must not admit".into(),
                false,
                CancellationToken::new(),
                |_, _| async { panic!("reserved Session admitted work") },
            )
            .now_or_never()
            .unwrap()
    })
    .join()
    .unwrap();
    let unchanged = session.entry_count().unwrap() == 0;
    drop(idle); // Failed preparation publishes nothing and leaves the old owner usable.
    let resumed = session
        .run_user_shell(
            "new".into(),
            false,
            CancellationToken::new(),
            |_, _| async { shell_output() },
        )
        .await;
    drop(session);
    fs::remove_dir_all(root).unwrap();
    assert!(matches!(denied, Err(CodingSessionError::OperationActive)));
    assert!(unchanged);
    assert_eq!(resumed.unwrap().value["stdout"], "settled");
}

fn shell_output() -> CodingToolOutput {
    CodingToolOutput {
        value: serde_json::json!({"stdout":"settled","cancelled":true}),
        images: Vec::new(),
        is_error: false,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_and_operation_waiters_share_actual_completion() {
    let root = std::env::temp_dir().join(format!("ion-owned-join-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let session =
        Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
    let (release, held) = tokio::sync::oneshot::channel();
    let stop = CancellationToken::new();
    let operation = session.run_user_shell(
        "fixture".into(),
        false,
        stop.clone(),
        move |_, _| async move {
            held.await.unwrap();
            shell_output()
        },
    );
    let waiter = tokio::spawn(operation);
    tokio::task::yield_now().await;
    let closing = session.clone();
    let shutdown = tokio::spawn(async move { closing.shutdown().await });
    stop.cancelled().await; // Shutdown has registered its independent join wait.
    release.send(()).unwrap();
    let (joined, closed) = tokio::time::timeout(Duration::from_secs(4), async {
        tokio::join!(waiter, shutdown)
    })
    .await
    .unwrap();
    assert_eq!(joined.unwrap().unwrap().value["stdout"], "settled");
    closed.unwrap().unwrap();
    assert!(matches!(
        session.select_model(model()),
        Err(CodingSessionError::Operation(OperationError::Closed))
    ));
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_shutdown_keeps_admission_closed_and_started_work_holds_writer_lease() {
    let root = std::env::temp_dir().join(format!("ion-owned-lease-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let path = root.join("session.sqlite");
    let session = Arc::new(CodingSession::create(&path, Some(&root)).unwrap());
    let (release, held) = tokio::sync::oneshot::channel();
    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let ready = started.clone();
    let operation = session.run_user_shell(
        "fixture".into(),
        false,
        CancellationToken::new(),
        move |_, _| async move {
            ready.add_permits(1);
            held.await.unwrap();
            shell_output()
        },
    );
    started.acquire().await.unwrap().forget();
    drop(operation);
    let incomplete = tokio::time::timeout(Duration::from_millis(5), session.shutdown()).await;
    let before = session.entry_count().unwrap();
    let denied = session
        .run_user_shell(
            "must not start".into(),
            false,
            CancellationToken::new(),
            |_, _| async { panic!("closed admission dispatched") },
        )
        .await;
    let unchanged = session.entry_count().unwrap() == before;
    drop(session);
    let held_lease = matches!(
        CodingSession::open(&path),
        Err(CodingSessionError::AlreadyOpen)
    );
    release.send(()).unwrap();
    let reopened = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            match CodingSession::open(&path) {
                Ok(session) => break session,
                Err(CodingSessionError::AlreadyOpen) => {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
                Err(error) => panic!("{error}"),
            }
        }
    })
    .await
    .unwrap();
    let observed = reopened.view().unwrap();
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
    assert!(incomplete.is_err());
    assert!(matches!(
        denied,
        Err(CodingSessionError::Operation(OperationError::Closed))
    ));
    assert!(unchanged && held_lease);
    assert!(
        matches!(observed.entries.last(),Some(SessionEntry::UserShellSettled { outcome: ion_core::UserShellOutcome::Observed { output,.. },.. }) if output["stdout"]=="settled")
    );
}
