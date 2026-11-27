//! Access is an owned asynchronous boundary, not an admission or transport grant.
use super::*;
use crate::{InputBudget, InputStatus};
use std::sync::atomic::AtomicUsize;
use tokio::sync::Semaphore;

struct AccessService {
    inner: Arc<ScriptedModelService>,
    allowed: AtomicBool,
    held: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
    checks: AtomicUsize,
    dropped: AtomicBool,
}
impl AccessService {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(ScriptedModelService::new([Script::Stream(vec![
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
            ])])),
            allowed: AtomicBool::new(true),
            held: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            checks: AtomicUsize::new(0),
            dropped: AtomicBool::new(false),
        })
    }
    async fn wait_entered(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.acquire())
            .await
            .unwrap()
            .unwrap()
            .forget();
    }
}
impl ModelService for AccessService {
    fn validate_access(&self) -> BoxFuture<'_, Result<(), ProviderError>> {
        Box::pin(async {
            struct Lookup<'a>(&'a AtomicBool);
            impl Drop for Lookup<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::SeqCst);
                }
            }
            let _lookup = Lookup(&self.dropped);
            self.checks.fetch_add(1, Ordering::SeqCst);
            self.entered.add_permits(1);
            if self.held.load(Ordering::SeqCst) {
                self.release.acquire().await.unwrap().forget();
            }
            if !self.allowed.load(Ordering::SeqCst) {
                return Err(ProviderError {
                    kind: ion_ai::ProviderErrorKind::Authentication,
                    message: "fixture access unavailable".into(),
                    retry_after_ms: None,
                });
            }
            self.inner.validate_access().await
        })
    }
    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        self.inner.stream(request)
    }
}
fn authentication<T>(result: Result<T, AgentError>) {
    assert!(matches!(
        result,
        Err(AgentError::Provider(ProviderError {
            kind: ion_ai::ProviderErrorKind::Authentication,
            ..
        }))
    ));
}

#[tokio::test]
async fn empty_compaction_does_not_require_provider_access() {
    let service = AccessService::new();
    service.allowed.store(false, Ordering::SeqCst);
    let (root, session, agent) = fixture(service.clone());
    let before = session.view().unwrap().entries;
    let result = agent
        .compact(None, session.clone(), CancellationToken::new(), |_| {})
        .await;
    assert_eq!(service.checks.load(Ordering::SeqCst), 0);
    assert!(service.inner.requests().is_empty());
    assert!(!result.unwrap());
    assert_eq!(session.view().unwrap().entries, before);
    session.shutdown().await.unwrap();
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn missing_access_returns_ordinary_input_without_a_turn_or_stream() {
    let service = AccessService::new();
    service.allowed.store(false, Ordering::SeqCst);
    let (root, session, agent) = fixture(service.clone());
    let before = session.view().unwrap().entries;
    let input = InputBudget::default()
        .prepare(request().input, (), agent.limits())
        .unwrap();
    let receipt = input.receipt();
    authentication(
        agent
            .submit_prepared(
                session.clone(),
                input,
                "test".into(),
                CancellationToken::new(),
                None::<Arc<SteeringInbox<()>>>,
                |_| panic!("unaccepted input published an event"),
            )
            .await,
    );
    assert_eq!(receipt.status(), InputStatus::NotAccepted);
    assert_eq!(session.view().unwrap().entries, before);
    assert!(service.inner.requests().is_empty());
    session.shutdown().await.unwrap();
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn keyed_receipt_remains_available_without_access_while_busy_stopped_and_closed() {
    let service = AccessService::new();
    let (root, session, agent) = fixture(service.clone());
    let submission = agent
        .submit_request::<_, ()>(
            session.clone(),
            request(),
            "test".into(),
            CancellationToken::new(),
            None,
            |_| {},
        )
        .await
        .unwrap();
    submission.operation.unwrap().await.unwrap();
    service.allowed.store(false, Ordering::SeqCst);
    let checks = service.checks.load(Ordering::SeqCst);
    let stop = CancellationToken::new();
    let active_stop = stop.clone();
    let active = session
        .operations
        .start::<_, AgentError, _>(stop.clone(), async move {
            active_stop.cancelled().await;
            Ok(())
        });
    for closed in [false, true] {
        if closed {
            stop.cancel();
            session.shutdown().await.unwrap();
        }
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let retry = agent
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
        assert_eq!(retry.receipt.turn, submission.receipt.turn);
        assert!(retry.operation.is_none());
    }
    active.await.unwrap();
    assert_eq!(service.checks.load(Ordering::SeqCst), checks);
    assert_eq!(service.inner.requests().len(), 1);
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn failed_access_reconciles_a_concurrently_committed_key() {
    let service = AccessService::new();
    service.held.store(true, Ordering::SeqCst);
    service.allowed.store(false, Ordering::SeqCst);
    let (root, session, agent) = fixture(service.clone());
    let waiting = agent.submit_request::<_, ()>(
        session.clone(),
        request(),
        "test".into(),
        CancellationToken::new(),
        None,
        |_| {},
    );
    tokio::pin!(waiting);
    tokio::select! {
        () = service.wait_entered() => {},
        _ = &mut waiting => panic!("access did not wait"),
    }
    let competing = Agent {
        service: service.inner.clone(),
        ..agent.clone()
    };
    let accepted = competing
        .submit_request::<_, ()>(
            session.clone(),
            request(),
            "test".into(),
            CancellationToken::new(),
            None,
            |_| {},
        )
        .await
        .unwrap();
    accepted.operation.unwrap().await.unwrap();
    service.release.add_permits(1);
    let reconciled = waiting.await.unwrap();
    assert_eq!(reconciled.receipt.turn, accepted.receipt.turn);
    assert!(reconciled.operation.is_none());
    assert_eq!(service.inner.requests().len(), 1);
    session.shutdown().await.unwrap();
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

fn recovery(session: &Session, agent: &Agent) -> crate::RecoveryPoint {
    let (binding, _) = agent.admission_binding(session, "test").unwrap();
    session
        .admit_turn(request().input, agent.model.clone(), None, binding)
        .unwrap();
    session.recovery_state().unwrap().unwrap().point
}

#[tokio::test]
async fn missing_access_preserves_recovery_checkpoint_and_journal() {
    let service = AccessService::new();
    service.allowed.store(false, Ordering::SeqCst);
    let (root, session, agent) = fixture(service.clone());
    let agent = agent.with_recovery_scope(AuthorityScope::new("access-fixture").unwrap());
    let point = recovery(&session, &agent);
    let before = session.view().unwrap().entries;
    authentication(
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
    );
    assert_eq!(session.recovery_state().unwrap().unwrap().point, point);
    assert_eq!(session.view().unwrap().entries, before);
    assert!(service.inner.requests().is_empty());
    session.shutdown().await.unwrap();
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cancellation_drops_pending_access_before_ordinary_keyed_or_recovery_commit() {
    for mode in ["ordinary", "keyed", "recovery"] {
        let service = AccessService::new();
        service.held.store(true, Ordering::SeqCst);
        let (root, session, agent) = fixture(service.clone());
        let agent = agent.with_recovery_scope(AuthorityScope::new("access-fixture").unwrap());
        let point = (mode == "recovery").then(|| recovery(&session, &agent));
        let before = session.view().unwrap().entries;
        let stop = CancellationToken::new();
        let input = InputBudget::default()
            .prepare(request().input, (), agent.limits())
            .unwrap();
        let receipt = input.receipt();
        {
            let waiting = async {
                match mode {
                    "ordinary" => agent
                        .submit_prepared(
                            session.clone(),
                            input,
                            "test".into(),
                            stop.clone(),
                            None::<Arc<SteeringInbox<()>>>,
                            |_| {},
                        )
                        .await
                        .map(|_| ()),
                    "keyed" => agent
                        .submit_request::<_, ()>(
                            session.clone(),
                            request(),
                            "test".into(),
                            stop.clone(),
                            None,
                            |_| {},
                        )
                        .await
                        .map(|_| ()),
                    "recovery" => agent
                        .resume_turn::<_, ()>(
                            session.clone(),
                            point.unwrap(),
                            "test".into(),
                            stop.clone(),
                            None,
                            |_| {},
                        )
                        .await
                        .map(|_| ()),
                    _ => unreachable!(),
                }
            };
            tokio::pin!(waiting);
            tokio::select! {
                () = service.wait_entered() => {},
                _ = &mut waiting => panic!("access did not wait"),
            }
            assert_eq!(session.view().unwrap().entries, before);
            stop.cancel();
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(5), waiting)
                    .await
                    .unwrap(),
                Err(AgentError::Cancelled)
            ));
        }
        assert!(service.dropped.load(Ordering::SeqCst));
        assert_eq!(receipt.status(), InputStatus::NotAccepted);
        assert_eq!(session.view().unwrap().entries, before);
        if let Some(point) = point {
            assert_eq!(session.recovery_state().unwrap().unwrap().point, point);
        }
        assert!(service.inner.requests().is_empty());
        session.shutdown().await.unwrap();
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}
