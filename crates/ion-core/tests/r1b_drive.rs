use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ion_ai::{
    BoxFuture, Content, GenerationControls, Message, ModelRef, ModelResponse, ModelStreamEvent,
    ProviderError, ProviderErrorKind, Reasoning, ResponseTermination, Role, ToolChoice, Usage,
};
use ion_core::{
    AdmitInputRequest, AuthorityCeiling, ContentDigest, ContextPolicy, ControlCeiling,
    ConversationConfig, DriveExit, EgressRealm, InputBody, InputMode, InputSender,
    ModelAttemptState, ModelBoundaries, ModelBoundary, ModelBoundaryIdentity, ModelStart,
    ObservationError, ParkReason, ProviderAdmissionError, ProviderBinding, ProviderBindingId,
    ProviderCapabilities, ProviderStartReceipt, RequestKey, ReturnedModelPolicy,
    SemanticCompatibilityId, Session, SessionChange, SessionId, SnapshotRequest,
    StartReceiptCapability, StartReconciliation, StartTurnRequest, StepDisposition, StepPurpose,
    TurnLimits, TurnOutcome, WatchQueueLimits, WatchRequest, WorkspaceBinding,
};
use tokio_util::sync::CancellationToken;

fn database(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "ion-r1b-drive-{name}-{}-{}",
        std::process::id(),
        SessionId::new()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let database = dir.join("session.sqlite");
    (dir, database)
}

fn config() -> ConversationConfig {
    let provider_id = ProviderBindingId::new("scripted").expect("provider id");
    ConversationConfig {
        instructions: "answer carefully".to_owned(),
        project_context: Vec::new(),
        providers: vec![ProviderBinding {
            id: provider_id.clone(),
            model: ModelRef {
                provider: "scripted".to_owned(),
                model: "test".to_owned(),
            },
            adapter: SemanticCompatibilityId::new("scripted-adapter-v1").expect("adapter"),
            request_encoding: SemanticCompatibilityId::new("scripted-request-v1")
                .expect("encoding"),
            replay_family: None,
            capabilities: ProviderCapabilities {
                max_input_tokens: 100_000,
                max_output_tokens: 8192,
                tools: false,
                parallel_tool_calls: false,
                structured_output: true,
                replay: false,
                reasoning: true,
            },
            returned_model: ReturnedModelPolicy::Exact,
            start_receipts: StartReceiptCapability::None,
            egress: EgressRealm::Local,
        }],
        default_provider: provider_id,
        fallback_route: Vec::new(),
        compaction_route: Vec::new(),
        tools: Vec::new(),
        initial_tools: Vec::new(),
        controls: GenerationControls {
            max_output_tokens: 4096,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::None,
            parallel_tool_calls: false,
        },
        control_ceiling: ControlCeiling {
            max_output_tokens: 8192,
            sampling: false,
            parallel_tool_calls: false,
            allowed_reasoning: vec![
                Reasoning::ProviderDefault,
                Reasoning::Off,
                Reasoning::Low,
                Reasoning::Medium,
                Reasoning::High,
            ],
        },
        context: ContextPolicy {
            max_request_bytes: 4 * 1024 * 1024,
            max_input_tokens: 100_000,
            max_checkpoint_bytes: 128 * 1024,
            max_tail_bytes: 1024 * 1024,
        },
        workspace: WorkspaceBinding {
            id: "workspace".to_owned(),
            canonical_root: "/tmp/project".to_owned(),
            backend: "local".to_owned(),
            object_identity: "dev:ino".to_owned(),
        },
        authority: AuthorityCeiling {
            workspace_mutation: false,
            unconfined_execution: false,
            remote_tools: false,
            egress_realms: vec![EgressRealm::Local],
        },
        limits: TurnLimits {
            max_model_steps: 8,
            max_model_attempts_per_step: 3,
            max_tool_invocations: 32,
            max_parallel_read_tools: 4,
            max_response_bytes: 1024 * 1024,
            max_tool_preview_bytes: 64 * 1024,
            max_cost_microusd: None,
        },
    }
}

async fn started_turn(
    session: &Session,
    key: &str,
    text: &str,
) -> (ion_core::SessionHandle, ion_core::TurnId) {
    let handle = session.handle();
    let conversation = session.primary_conversation();
    let admitted = handle
        .admit_input(
            conversation,
            AdmitInputRequest {
                sender: InputSender::User,
                mode: InputMode::Submit,
                request_key: Some(RequestKey::new(key).expect("request key")),
                body: InputBody::Text(text.to_owned()),
            },
        )
        .await
        .expect("admit input");
    let started = handle
        .start_turn(StartTurnRequest {
            conversation,
            input: admitted.input().id,
            admitted_at_unix_ms: 1,
            wall_deadline_unix_ms: None,
        })
        .await
        .expect("start turn");
    (handle, started.turn.id)
}

fn identity() -> ModelBoundaryIdentity {
    ModelBoundaryIdentity {
        binding: ProviderBindingId::new("scripted").expect("binding"),
        adapter: SemanticCompatibilityId::new("scripted-adapter-v1").expect("adapter"),
        request_encoding: SemanticCompatibilityId::new("scripted-request-v1").expect("encoding"),
        egress: EgressRealm::Local,
    }
}

fn allowed_boundaries(
    boundaries: impl IntoIterator<Item = Arc<dyn ModelBoundary>>,
) -> ModelBoundaries {
    ModelBoundaries::new(boundaries, Arc::new(|_: &ProviderBinding| Ok(()))).expect("boundaries")
}

fn fingerprint(
    request: &ion_core::SemanticRequest,
    effect_key: &str,
) -> Result<ContentDigest, ProviderError> {
    ContentDigest::of(&(request, effect_key)).map_err(|error| ProviderError {
        kind: ProviderErrorKind::InvalidRequest,
        message: error.to_string(),
    })
}

struct CompleteBoundary {
    egress: EgressRealm,
    starts: AtomicUsize,
    stream_dropped: CancellationToken,
}

impl CompleteBoundary {
    fn new() -> Arc<Self> {
        Self::in_realm(EgressRealm::Local)
    }

    fn in_realm(egress: EgressRealm) -> Arc<Self> {
        Arc::new(Self {
            egress,
            starts: AtomicUsize::new(0),
            stream_dropped: CancellationToken::new(),
        })
    }
}

/// Delivers one terminal event but deliberately never closes the transport.
struct TerminalWithoutEof {
    response: Option<ModelResponse>,
    dropped: CancellationToken,
}

impl futures_util::Stream for TerminalWithoutEof {
    type Item = Result<ModelStreamEvent, ProviderError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.response.take() {
            Some(response) => {
                std::task::Poll::Ready(Some(Ok(ModelStreamEvent::Completed(response))))
            }
            None => std::task::Poll::Pending,
        }
    }
}

impl Drop for TerminalWithoutEof {
    fn drop(&mut self) {
        self.dropped.cancel();
    }
}

impl ModelBoundary for CompleteBoundary {
    fn identity(&self) -> ModelBoundaryIdentity {
        ModelBoundaryIdentity {
            egress: self.egress.clone(),
            ..identity()
        }
    }

    fn fingerprint(
        &self,
        request: &ion_core::SemanticRequest,
        effect_key: &str,
    ) -> Result<ContentDigest, ProviderError> {
        fingerprint(request, effect_key)
    }

    fn start<'a>(
        &'a self,
        _attempt: ion_core::AttemptId,
        _effect_key: String,
        _request: ion_core::SemanticRequest,
        _stop: CancellationToken,
    ) -> BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let response = ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("done".to_owned())],
                    provider_replay: None,
                },
                usage: Usage::known(10, 4),
                termination: ResponseTermination::Completed,
            };
            let stream: ion_ai::ModelStream = Box::pin(TerminalWithoutEof {
                response: Some(response),
                dropped: self.stream_dropped.clone(),
            });
            ModelStart::Started {
                stream,
                start_receipt: None,
            }
        })
    }
}

struct WaitingBoundary {
    starts: AtomicUsize,
    started: tokio::sync::Notify,
}

impl WaitingBoundary {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            starts: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
        })
    }

    async fn wait_started(&self) {
        loop {
            let notified = self.started.notified();
            if self.starts.load(Ordering::SeqCst) != 0 {
                return;
            }
            notified.await;
        }
    }
}

impl ModelBoundary for WaitingBoundary {
    fn identity(&self) -> ModelBoundaryIdentity {
        identity()
    }

    fn fingerprint(
        &self,
        request: &ion_core::SemanticRequest,
        effect_key: &str,
    ) -> Result<ContentDigest, ProviderError> {
        fingerprint(request, effect_key)
    }

    fn start<'a>(
        &'a self,
        _attempt: ion_core::AttemptId,
        _effect_key: String,
        _request: ion_core::SemanticRequest,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.started.notify_waiters();
            stop.cancelled().await;
            ModelStart::Indeterminate {
                reason: "stopped by test".to_owned(),
                usage: Usage::unknown(),
                start_receipt: None,
            }
        })
    }
}

#[tokio::test]
async fn resume_selects_terminal_response_without_waiting_for_transport_eof() {
    let (dir, path) = database("final");
    let created = Session::create(&path, config()).await.expect("create");
    let session = created.session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;
    let boundary = CompleteBoundary::new();
    let boundaries = allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>]);

    let exit = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        handle.resume(turn, boundaries),
    )
    .await
    .expect("terminal event ends the attempt without EOF")
    .expect("resume");
    assert!(matches!(
        exit,
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 1);
    assert!(
        boundary.stream_dropped.is_cancelled(),
        "owned transport is released"
    );

    let snapshot = handle
        .snapshot(SnapshotRequest {
            conversation: session.primary_conversation(),
            max_inputs: 16,
            max_entries: 16,
            max_bytes: 1024 * 1024,
        })
        .await
        .expect("snapshot");
    assert!(snapshot.unfinished_turn.is_none());
    assert_eq!(snapshot.transcript_tail.len(), 2);

    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[tokio::test]
async fn cancelled_before_resume_never_calls_provider() {
    let (dir, path) = database("cancel-before");
    let created = Session::create(&path, config()).await.expect("create");
    let session = created.session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;
    let boundary = CompleteBoundary::new();
    let boundaries = allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>]);

    handle.cancel_turn(turn).await.expect("cancel");
    let exit = handle.resume(turn, boundaries).await.expect("resume");
    assert!(matches!(
        exit,
        DriveExit::Settled(TurnOutcome::Cancelled { .. })
    ));
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);

    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[tokio::test]
async fn cancellation_signals_an_already_admitted_provider_effect() {
    let (dir, path) = database("cancel-live");
    let created = Session::create(&path, config()).await.expect("create");
    let session = created.session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;
    let boundary = WaitingBoundary::new();
    let boundaries = allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>]);

    let drive_handle = handle.clone();
    let drive = tokio::spawn(async move { drive_handle.resume(turn, boundaries).await });
    boundary.wait_started().await;
    handle.cancel_turn(turn).await.expect("cancel");

    let exit = drive.await.expect("join").expect("resume");
    match exit {
        DriveExit::Settled(TurnOutcome::Cancelled {
            unresolved_attempts,
        }) => assert_eq!(unresolved_attempts.len(), 1),
        other => panic!("unexpected drive exit: {other:?}"),
    }
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 1);

    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[tokio::test]
async fn missing_provider_boundary_parks_before_dispatch_intent() {
    let (dir, path) = database("missing-provider");
    let created = Session::create(&path, config()).await.expect("create");
    let session = created.session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;

    let exit = handle
        .resume(turn, ModelBoundaries::default())
        .await
        .expect("resume");
    assert_eq!(exit, DriveExit::Parked(ParkReason::ProviderUnavailable));

    let snapshot = handle
        .snapshot(SnapshotRequest {
            conversation: session.primary_conversation(),
            max_inputs: 16,
            max_entries: 16,
            max_bytes: 1024 * 1024,
        })
        .await
        .expect("snapshot");
    assert!(snapshot.current_model_step.is_none());
    assert!(snapshot.model_attempts.is_empty());

    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

struct AdmissionControl {
    checks: AtomicUsize,
    deny_at: AtomicUsize,
    error: ProviderAdmissionError,
}

impl ion_core::ProviderAdmission for AdmissionControl {
    fn check(&self, binding: &ProviderBinding) -> Result<(), ProviderAdmissionError> {
        assert_eq!(binding.egress, EgressRealm::Local);
        let check = self.checks.fetch_add(1, Ordering::SeqCst) + 1;
        if check >= self.deny_at.load(Ordering::SeqCst) {
            Err(self.error)
        } else {
            Ok(())
        }
    }
}

fn snapshot_request(session: &Session) -> SnapshotRequest {
    SnapshotRequest {
        conversation: session.primary_conversation(),
        max_inputs: 16,
        max_entries: 16,
        max_bytes: 1024 * 1024,
    }
}

#[tokio::test]
async fn wrong_service_realm_never_reaches_admission_or_provider_start() {
    for (frozen, actual) in [
        (
            EgressRealm::Local,
            EgressRealm::Remote("cloud-a".to_owned()),
        ),
        (
            EgressRealm::Remote("cloud-a".to_owned()),
            EgressRealm::Local,
        ),
        (
            EgressRealm::Remote("cloud-a".to_owned()),
            EgressRealm::Remote("cloud-b".to_owned()),
        ),
    ] {
        let (dir, path) = database("wrong-realm");
        let mut cfg = config();
        cfg.providers[0].egress = frozen.clone();
        cfg.authority.egress_realms = vec![frozen];
        let boundary = CompleteBoundary::in_realm(actual);
        let boundaries = ModelBoundaries::new(
            [boundary.clone() as Arc<dyn ModelBoundary>],
            Arc::new(|_: &ProviderBinding| panic!("wrong realm must fail before live admission")),
        )
        .expect("boundaries");
        assert!(matches!(
            boundaries.resolve(&cfg.providers[0]),
            Err(ion_core::ModelBoundaryError::Incompatible {
                fact: "service realm",
                ..
            })
        ));
        let session = Session::create(&path, cfg).await.expect("create").session;
        let (handle, turn) = started_turn(&session, "one", "private prompt").await;
        assert_eq!(
            handle.resume(turn, boundaries).await.expect("resume"),
            DriveExit::Parked(ParkReason::ProviderUnavailable)
        );
        let snapshot = handle
            .snapshot(snapshot_request(&session))
            .await
            .expect("snapshot");
        assert!(snapshot.model_attempts.is_empty());
        assert!(snapshot.current_model_step.is_none());
        assert_eq!(snapshot.unfinished_turn.unwrap().budget.model_attempts, 0);
        assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
        session.close().await.expect("close");
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}

#[tokio::test]
async fn monetary_ceiling_without_a_host_quote_never_dispatches() {
    let (dir, path) = database("unpriced-cap");
    let mut configured = config();
    configured.limits.max_cost_microusd = Some(100);
    let session = Session::create(&path, configured).await.unwrap().session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;
    let boundary = CompleteBoundary::new();
    let boundaries = ModelBoundaries::new(
        [boundary.clone() as Arc<dyn ModelBoundary>],
        Arc::new(|_: &ProviderBinding| Ok(())),
    )
    .unwrap();
    assert_eq!(
        handle.resume(turn, boundaries.clone()).await.unwrap(),
        DriveExit::Parked(ParkReason::Capacity)
    );
    let snapshot = handle.snapshot(snapshot_request(&session)).await.unwrap();
    assert!(snapshot.model_attempts.is_empty());
    assert_eq!(
        snapshot
            .unfinished_turn
            .unwrap()
            .budget
            .reserved_cost_microusd,
        0
    );
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
    session.close().await.unwrap();
    let reopened = Session::open(&path).await.unwrap();
    let snapshot = reopened
        .handle()
        .snapshot(snapshot_request(&reopened))
        .await
        .unwrap();
    assert!(snapshot.model_attempts.is_empty());
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
    assert_eq!(
        reopened.handle().resume(turn, boundaries).await.unwrap(),
        DriveExit::Parked(ParkReason::Capacity)
    );
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
    reopened.close().await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn preflight_denial_consumes_no_attempt_and_restoration_requires_explicit_resume() {
    for (error, reason) in [
        (
            ProviderAdmissionError::MissingCredentials,
            ParkReason::MissingCredentials,
        ),
        (
            ProviderAdmissionError::Unavailable,
            ParkReason::ProviderUnavailable,
        ),
        (
            ProviderAdmissionError::EgressDenied,
            ParkReason::AuthorityDenied,
        ),
    ] {
        let (dir, path) = database("preflight-denied");
        let session = Session::create(&path, config())
            .await
            .expect("create")
            .session;
        let (handle, turn) = started_turn(&session, "one", "private prompt").await;
        let boundary = CompleteBoundary::new();
        let admission = Arc::new(AdmissionControl {
            checks: AtomicUsize::new(0),
            deny_at: AtomicUsize::new(1),
            error,
        });
        let boundaries = ModelBoundaries::new(
            [boundary.clone() as Arc<dyn ModelBoundary>],
            admission.clone(),
        )
        .expect("boundaries");
        assert_eq!(
            handle
                .resume(turn, boundaries.clone())
                .await
                .expect("resume"),
            DriveExit::Parked(reason.clone())
        );
        let before = handle
            .snapshot(snapshot_request(&session))
            .await
            .expect("snapshot");
        assert!(before.model_attempts.is_empty());
        assert_eq!(
            before
                .unfinished_turn
                .as_ref()
                .unwrap()
                .budget
                .model_attempts,
            0
        );
        assert_eq!(admission.checks.load(Ordering::SeqCst), 1);
        assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
        session.close().await.expect("close");

        let reopened = Session::open(&path).await.expect("passive open");
        let handle = reopened.handle();
        let request = snapshot_request(&reopened);
        let after = handle.snapshot(request).await.expect("passive snapshot");
        assert_eq!(after.coverage, before.coverage);
        assert_eq!(after.current_model_step, before.current_model_step);
        assert_eq!(admission.checks.load(Ordering::SeqCst), 1);
        assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
        assert_eq!(
            handle
                .resume(turn, boundaries.clone())
                .await
                .expect("still denied"),
            DriveExit::Parked(reason)
        );
        assert_eq!(
            handle.snapshot(request).await.unwrap().coverage,
            before.coverage
        );
        assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);

        admission.deny_at.store(usize::MAX, Ordering::SeqCst);
        // Restoring credentials/policy is live state, not a durable request change.
        assert_eq!(
            handle.snapshot(request).await.unwrap().coverage,
            before.coverage
        );
        assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
        assert!(matches!(
            handle
                .resume(turn, boundaries)
                .await
                .expect("restored explicit resume"),
            DriveExit::Settled(TurnOutcome::Completed { .. })
        ));
        assert_eq!(admission.checks.load(Ordering::SeqCst), 4);
        assert_eq!(boundary.starts.load(Ordering::SeqCst), 1);
        reopened.close().await.expect("close");
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}

#[tokio::test]
async fn revocation_after_intent_records_one_not_started_attempt_without_network_start() {
    for (error, reason) in [
        (
            ProviderAdmissionError::MissingCredentials,
            ParkReason::MissingCredentials,
        ),
        (
            ProviderAdmissionError::Unavailable,
            ParkReason::ProviderUnavailable,
        ),
        (
            ProviderAdmissionError::EgressDenied,
            ParkReason::AuthorityDenied,
        ),
    ] {
        let (dir, path) = database("second-check-denied");
        let session = Session::create(&path, config())
            .await
            .expect("create")
            .session;
        let (handle, turn) = started_turn(&session, "one", "private prompt").await;
        let boundary = CompleteBoundary::new();
        let admission = Arc::new(AdmissionControl {
            checks: AtomicUsize::new(0),
            deny_at: AtomicUsize::new(2),
            error,
        });
        let boundaries = ModelBoundaries::new(
            [boundary.clone() as Arc<dyn ModelBoundary>],
            admission.clone(),
        )
        .expect("boundaries");
        assert_eq!(
            handle
                .resume(turn, boundaries.clone())
                .await
                .expect("resume"),
            DriveExit::Parked(reason)
        );
        let before = handle
            .snapshot(snapshot_request(&session))
            .await
            .expect("snapshot");
        assert_eq!(before.model_attempts.len(), 1);
        assert_eq!(
            before
                .unfinished_turn
                .as_ref()
                .unwrap()
                .budget
                .model_attempts,
            1
        );
        assert_eq!(
            before.model_attempts[0].state,
            ModelAttemptState::NotStarted {
                reason: error.to_string(),
            }
        );
        assert_eq!(admission.checks.load(Ordering::SeqCst), 2);
        assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
        session.close().await.expect("close");

        let reopened = Session::open(&path).await.expect("passive open");
        let handle = reopened.handle();
        let watched = handle
            .snapshot_and_watch(WatchRequest {
                snapshot: snapshot_request(&reopened),
                queue: WatchQueueLimits {
                    max_receipts: 32,
                    max_bytes: 1024 * 1024,
                },
            })
            .await
            .expect("watch");
        assert_eq!(watched.snapshot.coverage, before.coverage);
        assert_eq!(watched.snapshot.model_attempts, before.model_attempts);
        assert_eq!(admission.checks.load(Ordering::SeqCst), 2);
        admission.deny_at.store(usize::MAX, Ordering::SeqCst);
        assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
        assert!(matches!(
            handle
                .resume(turn, boundaries)
                .await
                .expect("restored explicit resume"),
            DriveExit::Settled(TurnOutcome::Completed { .. })
        ));
        assert_eq!(boundary.starts.load(Ordering::SeqCst), 1);
        let mut retry_seen = false;
        while let Ok(receipt) = watched.watch.try_recv() {
            for change in receipt.update.changes {
                match change {
                    SessionChange::ModelAttempt(attempt) => {
                        assert_ne!(
                            attempt.id, before.model_attempts[0].id,
                            "old evidence is immutable"
                        );
                        assert_eq!(attempt.ordinal, 2);
                        retry_seen = true;
                    }
                    SessionChange::ModelStep(step) => assert_eq!(
                        step.manifest,
                        before.current_model_step.as_ref().unwrap().manifest,
                        "live auth/policy is excluded from durable fingerprints"
                    ),
                    _ => {}
                }
            }
        }
        assert!(retry_seen);
        reopened.close().await.expect("close");
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}

#[tokio::test]
async fn failed_not_started_commit_fences_without_starting_or_erasing_intent() {
    let (dir, path) = database("revocation-evidence-fault");
    let session = Session::create(&path, config())
        .await
        .expect("create")
        .session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;
    let boundary = CompleteBoundary::new();
    let admission = Arc::new(AdmissionControl {
        checks: AtomicUsize::new(0),
        deny_at: AtomicUsize::new(2),
        error: ProviderAdmissionError::EgressDenied,
    });
    let boundaries = ModelBoundaries::new([boundary.clone() as Arc<dyn ModelBoundary>], admission)
        .expect("boundaries");
    let injector = rusqlite::Connection::open(&path).expect("injector");
    injector
        .execute_batch(
            "CREATE TRIGGER fail_not_started BEFORE UPDATE ON model_attempts
         BEGIN SELECT RAISE(ABORT, 'injected evidence failure'); END;",
        )
        .expect("inject fault");
    assert!(matches!(
        handle.resume(turn, boundaries).await.expect("drive"),
        DriveExit::Faulted { .. }
    ));
    assert_eq!(handle.health(), ion_core::SessionHealth::Fenced);
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
    let before = handle
        .snapshot(snapshot_request(&session))
        .await
        .expect("snapshot");
    assert_eq!(before.model_attempts.len(), 1);
    assert!(matches!(
        before.model_attempts[0].state,
        ModelAttemptState::IntentCommitted { .. }
    ));
    injector
        .execute_batch("DROP TRIGGER fail_not_started;")
        .expect("remove fault");
    drop(injector);
    session.close().await.expect("close");

    let reopened = Session::open(&path).await.expect("passive open");
    let handle = reopened.handle();
    let after = handle
        .snapshot(snapshot_request(&reopened))
        .await
        .expect("snapshot");
    assert_eq!(after.coverage, before.coverage);
    assert_eq!(after.model_attempts, before.model_attempts);
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
    // Lost negative evidence is not reconstructible from current availability.
    assert_eq!(
        handle
            .resume(
                turn,
                allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>])
            )
            .await
            .expect("explicit recovery"),
        DriveExit::Parked(ParkReason::RecoveryRequired)
    );
    let recovered = handle
        .snapshot(snapshot_request(&reopened))
        .await
        .expect("snapshot");
    assert_eq!(recovered.model_attempts.len(), 1);
    assert!(matches!(
        recovered.model_attempts[0].state,
        ModelAttemptState::Indeterminate { .. }
    ));
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
    reopened.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

fn fallback_config() -> ConversationConfig {
    let mut config = config();
    let fallback_id = ProviderBindingId::new("fallback").expect("fallback id");
    let mut fallback = config.providers[0].clone();
    fallback.id = fallback_id.clone();
    fallback.model.provider = "fallback".to_owned();
    fallback.model.model = "fallback-test".to_owned();
    fallback.adapter = SemanticCompatibilityId::new("fallback-adapter-v1").expect("adapter");
    fallback.request_encoding =
        SemanticCompatibilityId::new("fallback-request-v1").expect("encoding");
    config.providers.push(fallback);
    config.fallback_route = vec![fallback_id];
    config
}

struct FailingBoundary {
    starts: AtomicUsize,
    kind: ProviderErrorKind,
}

impl FailingBoundary {
    fn new(kind: ProviderErrorKind) -> Arc<Self> {
        Arc::new(Self {
            starts: AtomicUsize::new(0),
            kind,
        })
    }
}

impl ModelBoundary for FailingBoundary {
    fn identity(&self) -> ModelBoundaryIdentity {
        identity()
    }

    fn fingerprint(
        &self,
        request: &ion_core::SemanticRequest,
        effect_key: &str,
    ) -> Result<ContentDigest, ProviderError> {
        fingerprint(request, effect_key)
    }

    fn start<'a>(
        &'a self,
        _attempt: ion_core::AttemptId,
        _effect_key: String,
        _request: ion_core::SemanticRequest,
        _stop: CancellationToken,
    ) -> BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let stream: ion_ai::ModelStream =
                Box::pin(futures_util::stream::iter([Err(ProviderError {
                    kind: self.kind,
                    message: "primary rejected request".to_owned(),
                })]));
            ModelStart::Started {
                stream,
                start_receipt: None,
            }
        })
    }
}

struct FallbackBoundary {
    starts: AtomicUsize,
}

impl FallbackBoundary {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            starts: AtomicUsize::new(0),
        })
    }
}

impl ModelBoundary for FallbackBoundary {
    fn identity(&self) -> ModelBoundaryIdentity {
        ModelBoundaryIdentity {
            binding: ProviderBindingId::new("fallback").expect("binding"),
            adapter: SemanticCompatibilityId::new("fallback-adapter-v1").expect("adapter"),
            request_encoding: SemanticCompatibilityId::new("fallback-request-v1")
                .expect("encoding"),
            egress: EgressRealm::Local,
        }
    }

    fn fingerprint(
        &self,
        request: &ion_core::SemanticRequest,
        effect_key: &str,
    ) -> Result<ContentDigest, ProviderError> {
        fingerprint(request, effect_key)
    }

    fn start<'a>(
        &'a self,
        _attempt: ion_core::AttemptId,
        _effect_key: String,
        _request: ion_core::SemanticRequest,
        _stop: CancellationToken,
    ) -> BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let response = ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("fallback done".to_owned())],
                    provider_replay: None,
                },
                usage: Usage::known(8, 3),
                termination: ResponseTermination::Completed,
            };
            let stream: ion_ai::ModelStream = Box::pin(futures_util::stream::iter([Ok(
                ModelStreamEvent::Completed(response),
            )]));
            ModelStart::Started {
                stream,
                start_receipt: None,
            }
        })
    }
}

#[tokio::test]
async fn provider_fallback_supersedes_predecessor_in_one_atomic_commit() {
    let (dir, path) = database("fallback");
    let created = Session::create(&path, fallback_config())
        .await
        .expect("create");
    let session = created.session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;

    let established = handle
        .snapshot_and_watch(WatchRequest {
            snapshot: SnapshotRequest {
                conversation: session.primary_conversation(),
                max_inputs: 16,
                max_entries: 16,
                max_bytes: 1024 * 1024,
            },
            queue: WatchQueueLimits {
                max_receipts: 32,
                max_bytes: 1024 * 1024,
            },
        })
        .await
        .expect("watch");

    let primary = FailingBoundary::new(ProviderErrorKind::Authentication);
    let fallback = FallbackBoundary::new();
    let boundaries = allowed_boundaries([
        primary.clone() as Arc<dyn ModelBoundary>,
        fallback.clone() as Arc<dyn ModelBoundary>,
    ]);

    let exit = handle.resume(turn, boundaries).await.expect("resume");
    assert!(matches!(
        exit,
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    assert_eq!(primary.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fallback.starts.load(Ordering::SeqCst), 1);

    let mut saw_atomic_fallback = false;
    loop {
        let receipt = match established.watch.try_recv() {
            Ok(receipt) => receipt,
            Err(ObservationError::Empty) => break,
            Err(error) => panic!("watch failed: {error}"),
        };
        let mut predecessor = None;
        let mut successor = None;
        let mut updated_turn = None;
        for change in &receipt.update.changes {
            match change {
                SessionChange::ModelStep(step) => match &step.disposition {
                    StepDisposition::Superseded {
                        successor: Some(successor_id),
                        ..
                    } => predecessor = Some((step.id, *successor_id)),
                    StepDisposition::Open
                        if matches!(&step.purpose, StepPurpose::Fallback { .. }) =>
                    {
                        successor = Some(step);
                    }
                    _ => {}
                },
                SessionChange::Turn(value) if value.id == turn => updated_turn = Some(value),
                _ => {}
            }
        }
        if let (Some((predecessor_id, successor_id)), Some(successor), Some(updated_turn)) =
            (predecessor, successor, updated_turn)
        {
            assert_eq!(successor.id, successor_id);
            assert!(matches!(
                &successor.purpose,
                StepPurpose::Fallback { predecessor } if *predecessor == predecessor_id
            ));
            assert_eq!(successor.manifest.settings.revision, 1);
            assert_eq!(successor.manifest.settings.provider.as_str(), "fallback");
            assert_eq!(updated_turn.settings, successor.manifest.settings);
            saw_atomic_fallback = true;
        }
    }
    assert!(
        saw_atomic_fallback,
        "supersession, successor creation and TurnSettings update must share one commit"
    );

    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[tokio::test]
async fn frozen_fallback_still_requires_live_egress_admission() {
    let (dir, path) = database("fallback-live-denial");
    let session = Session::create(&path, fallback_config())
        .await
        .expect("create")
        .session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;
    let primary = FailingBoundary::new(ProviderErrorKind::Authentication);
    let fallback = FallbackBoundary::new();
    let boundaries = ModelBoundaries::new(
        [
            primary.clone() as Arc<dyn ModelBoundary>,
            fallback.clone() as Arc<dyn ModelBoundary>,
        ],
        Arc::new(|binding: &ProviderBinding| {
            if binding.id.as_str() == "fallback" {
                Err(ProviderAdmissionError::EgressDenied)
            } else {
                Ok(())
            }
        }),
    )
    .expect("boundaries");
    assert_eq!(
        handle.resume(turn, boundaries).await.expect("resume"),
        DriveExit::Parked(ParkReason::AuthorityDenied)
    );
    assert_eq!(primary.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fallback.starts.load(Ordering::SeqCst), 0);
    let snapshot = handle
        .snapshot(snapshot_request(&session))
        .await
        .expect("snapshot");
    assert_eq!(
        snapshot
            .current_model_step
            .as_ref()
            .unwrap()
            .manifest
            .settings
            .provider
            .as_str(),
        "fallback"
    );
    assert!(
        snapshot.model_attempts.is_empty(),
        "denied fallback consumes no attempt"
    );
    assert_eq!(snapshot.unfinished_turn.unwrap().budget.model_attempts, 1);
    assert!(matches!(
        handle
            .resume(
                turn,
                allowed_boundaries([
                    primary.clone() as Arc<dyn ModelBoundary>,
                    fallback.clone() as Arc<dyn ModelBoundary>,
                ])
            )
            .await
            .expect("restored explicit resume"),
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    assert_eq!(primary.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fallback.starts.load(Ordering::SeqCst), 1);
    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

struct RecoveringBoundary {
    starts: AtomicUsize,
    reconciles: AtomicUsize,
}

impl RecoveringBoundary {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            starts: AtomicUsize::new(0),
            reconciles: AtomicUsize::new(0),
        })
    }
}

impl ModelBoundary for RecoveringBoundary {
    fn identity(&self) -> ModelBoundaryIdentity {
        identity()
    }

    fn fingerprint(
        &self,
        request: &ion_core::SemanticRequest,
        effect_key: &str,
    ) -> Result<ContentDigest, ProviderError> {
        fingerprint(request, effect_key)
    }

    fn start_receipts(&self) -> StartReceiptCapability {
        StartReceiptCapability::Authoritative
    }

    fn start<'a>(
        &'a self,
        _attempt: ion_core::AttemptId,
        _effect_key: String,
        _request: ion_core::SemanticRequest,
        _stop: CancellationToken,
    ) -> BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            self.starts.fetch_add(1, Ordering::SeqCst);
            ModelStart::Indeterminate {
                reason: "transport outcome unknown".to_owned(),
                usage: Usage::unknown(),
                start_receipt: None,
            }
        })
    }

    fn reconcile_start<'a>(
        &'a self,
        _attempt: ion_core::AttemptId,
        _effect_key: String,
    ) -> BoxFuture<'a, StartReconciliation> {
        Box::pin(async move {
            self.reconciles.fetch_add(1, Ordering::SeqCst);
            StartReconciliation::Started(ProviderStartReceipt {
                kind: "test-start".to_owned(),
                data: serde_json::json!({"started": true}),
            })
        })
    }
}

#[tokio::test]
async fn passive_open_does_not_reconcile_but_resume_reconciles_indeterminate_attempt() {
    let (dir, path) = database("reconcile-on-resume");
    let mut cfg = config();
    cfg.providers[0].start_receipts = StartReceiptCapability::Authoritative;
    let created = Session::create(&path, cfg).await.expect("create");
    let session = created.session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;

    let boundary = RecoveringBoundary::new();
    let boundaries = allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>]);
    let first = handle.resume(turn, boundaries).await.expect("first resume");
    assert_eq!(first, DriveExit::Parked(ParkReason::RecoveryRequired));
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 1);
    assert_eq!(boundary.reconciles.load(Ordering::SeqCst), 0);

    session.close().await.expect("close");
    let reopened = Session::open(&path).await.expect("open");
    assert_eq!(
        boundary.reconciles.load(Ordering::SeqCst),
        0,
        "passive open must not reconcile provider state"
    );

    let before = reopened
        .handle()
        .snapshot(snapshot_request(&reopened))
        .await
        .expect("snapshot");
    let denied = ModelBoundaries::new(
        [boundary.clone() as Arc<dyn ModelBoundary>],
        Arc::new(|_: &ProviderBinding| Err(ProviderAdmissionError::EgressDenied)),
    )
    .expect("boundaries");
    assert_eq!(
        reopened
            .handle()
            .resume(turn, denied)
            .await
            .expect("denied reconciliation"),
        DriveExit::Parked(ParkReason::AuthorityDenied)
    );
    let after = reopened
        .handle()
        .snapshot(snapshot_request(&reopened))
        .await
        .expect("snapshot");
    assert_eq!(before.coverage, after.coverage);
    assert_eq!(
        before.model_attempts, after.model_attempts,
        "denial cannot prove an old effect never started"
    );
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 1);
    assert_eq!(boundary.reconciles.load(Ordering::SeqCst), 0);

    let boundaries = allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>]);
    let second = reopened
        .handle()
        .resume(turn, boundaries)
        .await
        .expect("second resume");
    assert_eq!(second, DriveExit::Parked(ParkReason::RecoveryRequired));
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 1);
    assert_eq!(boundary.reconciles.load(Ordering::SeqCst), 1);

    let snapshot = reopened
        .handle()
        .snapshot(SnapshotRequest {
            conversation: reopened.primary_conversation(),
            max_inputs: 16,
            max_entries: 16,
            max_bytes: 1024 * 1024,
        })
        .await
        .expect("snapshot");
    assert_eq!(snapshot.model_attempts.len(), 1);
    match &snapshot.model_attempts[0].state {
        ModelAttemptState::Indeterminate {
            start_receipt: Some(receipt),
            ..
        } => assert_eq!(receipt.kind, "test-start"),
        other => panic!("unexpected recovered attempt state: {other:?}"),
    }

    reopened.close().await.expect("close reopened");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[tokio::test]
async fn safety_refusal_does_not_route_to_fallback_provider() {
    let (dir, path) = database("safety-no-fallback");
    let created = Session::create(&path, fallback_config())
        .await
        .expect("create");
    let session = created.session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;

    let primary = FailingBoundary::new(ProviderErrorKind::Safety);
    let fallback = FallbackBoundary::new();
    let boundaries = allowed_boundaries([
        primary.clone() as Arc<dyn ModelBoundary>,
        fallback.clone() as Arc<dyn ModelBoundary>,
    ]);

    let exit = handle.resume(turn, boundaries).await.expect("resume");
    assert_eq!(exit, DriveExit::Parked(ParkReason::ProviderUnavailable));
    assert_eq!(primary.starts.load(Ordering::SeqCst), 1);
    assert_eq!(fallback.starts.load(Ordering::SeqCst), 0);

    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

/// Host-controlled completion lets lifecycle tests distinguish a dropped client
/// waiter from an explicit stop of the accepted provider work.
struct ReleasedBoundary {
    started: CancellationToken,
    release: CancellationToken,
    stopped: CancellationToken,
}

impl ModelBoundary for ReleasedBoundary {
    fn identity(&self) -> ModelBoundaryIdentity {
        identity()
    }

    fn fingerprint(
        &self,
        request: &ion_core::SemanticRequest,
        effect_key: &str,
    ) -> Result<ContentDigest, ProviderError> {
        fingerprint(request, effect_key)
    }

    fn start<'a>(
        &'a self,
        _attempt: ion_core::AttemptId,
        _effect_key: String,
        _request: ion_core::SemanticRequest,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            self.started.cancel();
            tokio::select! {
                () = stop.cancelled() => {
                    self.stopped.cancel();
                    ModelStart::Indeterminate {
                        reason: "host stop".to_owned(),
                        usage: Usage::unknown(),
                        start_receipt: None,
                    }
                }
                () = self.release.cancelled() => {
                    let response = ModelResponse {
                        message: Message {
                            role: Role::Assistant,
                            content: vec![Content::Text("completed without a waiter".to_owned())],
                            provider_replay: None,
                        },
                        usage: Usage::known(1, 1),
                        termination: ResponseTermination::Completed,
                    };
                    let stream: ion_ai::ModelStream = Box::pin(futures_util::stream::iter([
                        Ok(ModelStreamEvent::Completed(response)),
                    ]));
                    ModelStart::Started { stream, start_receipt: None }
                }
            }
        })
    }
}

#[tokio::test]
async fn dropped_resume_waiter_does_not_cancel_accepted_work_or_close_session() {
    let (dir, path) = database("dropped-waiter");
    let session = Session::create(&path, config())
        .await
        .expect("create")
        .session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;
    let boundary = Arc::new(ReleasedBoundary {
        started: CancellationToken::new(),
        release: CancellationToken::new(),
        stopped: CancellationToken::new(),
    });
    let boundaries = allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>]);
    let waiter_handle = handle.clone();
    let waiter = tokio::spawn(async move { waiter_handle.resume(turn, boundaries).await });
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        boundary.started.cancelled(),
    )
    .await
    .expect("effect started");
    waiter.abort();
    assert!(waiter.await.expect_err("waiter aborted").is_cancelled());
    assert!(!boundary.stopped.is_cancelled());

    boundary.release.cancel();
    // Reattaching joins the same drive, or inspects its committed terminal result.
    let exit = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        handle.resume(turn, ModelBoundaries::default()),
    )
    .await
    .expect("completion bounded")
    .expect("reattach");
    assert!(matches!(
        exit,
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    assert!(!boundary.stopped.is_cancelled());
    assert_eq!(session.health(), ion_core::SessionHealth::Open);
    let snapshot = handle
        .snapshot(SnapshotRequest {
            conversation: session.primary_conversation(),
            max_inputs: 16,
            max_entries: 16,
            max_bytes: 1024 * 1024,
        })
        .await
        .expect("session remains readable");
    assert_eq!(snapshot.transcript_tail.len(), 2);
    // A terminal Turn does not consume the Session's admission lifetime.
    let (_, successor) = started_turn(&session, "two", "next request").await;
    assert_ne!(turn, successor);
    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[tokio::test]
async fn close_joins_local_work_without_durable_turn_cancellation() {
    let (dir, path) = database("close-suspends");
    let session = Session::create(&path, config())
        .await
        .expect("create")
        .session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;
    let boundary = WaitingBoundary::new();
    let boundaries = allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>]);
    let waiter_handle = handle.clone();
    let waiter = tokio::spawn(async move { waiter_handle.resume(turn, boundaries).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), boundary.wait_started())
        .await
        .expect("effect started");
    tokio::time::timeout(std::time::Duration::from_secs(5), session.close())
        .await
        .expect("close bounded")
        .expect("close");
    assert!(matches!(
        waiter.await.expect("joined waiter").expect("drive"),
        DriveExit::Stopped { .. }
    ));
    let reopened = Session::open(&path)
        .await
        .expect("ownership released after join");
    let snapshot = reopened
        .handle()
        .snapshot(SnapshotRequest {
            conversation: reopened.primary_conversation(),
            max_inputs: 16,
            max_entries: 16,
            max_bytes: 1024 * 1024,
        })
        .await
        .expect("passive snapshot");
    let active = snapshot.unfinished_turn.expect("unfinished turn preserved");
    assert_eq!(active.id, turn);
    assert!(!active.cancellation.requested);
    assert_eq!(active.cancellation.generation, 0);
    assert!(active.outcome.is_none());
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 1);
    reopened.close().await.expect("close reopened");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

struct ProcessReceiptBoundary {
    marker: PathBuf,
    starts: AtomicUsize,
    reconciles: AtomicUsize,
}

impl ModelBoundary for ProcessReceiptBoundary {
    fn identity(&self) -> ModelBoundaryIdentity {
        identity()
    }

    fn fingerprint(
        &self,
        request: &ion_core::SemanticRequest,
        effect_key: &str,
    ) -> Result<ContentDigest, ProviderError> {
        fingerprint(request, effect_key)
    }

    fn start_receipts(&self) -> StartReceiptCapability {
        StartReceiptCapability::Authoritative
    }

    fn start<'a>(
        &'a self,
        attempt: ion_core::AttemptId,
        effect_key: String,
        _request: ion_core::SemanticRequest,
        _stop: CancellationToken,
    ) -> BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            use std::io::Write;
            self.starts.fetch_add(1, Ordering::SeqCst);
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&self.marker)
                .expect("one external start only");
            let bytes = serde_json::to_vec(&(attempt, effect_key)).expect("receipt encoding");
            file.write_all(&bytes).expect("external durable receipt");
            file.sync_all().expect("receipt durability");
            // Process loss occurs after the external start, before returning any
            // receipt/evidence to the Session. No cooperative close is involved.
            std::future::pending().await
        })
    }

    fn reconcile_start<'a>(
        &'a self,
        attempt: ion_core::AttemptId,
        effect_key: String,
    ) -> BoxFuture<'a, StartReconciliation> {
        Box::pin(async move {
            self.reconciles.fetch_add(1, Ordering::SeqCst);
            let bytes = std::fs::read(&self.marker).expect("external receipt survives owner");
            let stored: (ion_core::AttemptId, String) =
                serde_json::from_slice(&bytes).expect("complete receipt");
            assert_eq!(stored, (attempt, effect_key));
            StartReconciliation::Started(ProviderStartReceipt {
                kind: "process-test-v1".to_owned(),
                data: serde_json::json!({"attempt": attempt}),
            })
        })
    }
}

#[tokio::test]
async fn process_loss_child() {
    let Some(dir) = std::env::var_os("ION_R1B_PROCESS_LOSS_CHILD") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let mut cfg = config();
    cfg.providers[0].start_receipts = StartReceiptCapability::Authoritative;
    let session = Session::create(dir.join("session.sqlite"), cfg)
        .await
        .expect("create")
        .session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;
    let boundary = Arc::new(ProcessReceiptBoundary {
        marker: dir.join("external-receipt.json"),
        starts: AtomicUsize::new(0),
        reconciles: AtomicUsize::new(0),
    });
    let boundaries = allowed_boundaries([boundary as Arc<dyn ModelBoundary>]);
    let _ = handle.resume(turn, boundaries).await;
    panic!("parent must kill owner before start returns");
}

#[tokio::test]
async fn killed_owner_reopens_passively_then_reconciles_external_start() {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let (dir, path) = database("process-loss");
    let marker = dir.join("external-receipt.json");
    let mut child = ChildGuard(
        std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", "process_loss_child", "--nocapture"])
            .env("ION_R1B_PROCESS_LOSS_CHILD", &dir)
            .stdout(std::process::Stdio::null())
            .spawn()
            .expect("spawn owner"),
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            // Wait for a complete receipt, not merely create_new's directory entry.
            if std::fs::read(&marker)
                .ok()
                .and_then(|bytes| {
                    serde_json::from_slice::<(ion_core::AttemptId, String)>(&bytes).ok()
                })
                .is_some()
            {
                break;
            }
            assert!(
                child.0.try_wait().expect("child status").is_none(),
                "owner exited early"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("external start bounded");
    child.0.kill().expect("kill owner");
    let status = child.0.wait().expect("reap owner");
    assert!(!status.success());

    let boundary = Arc::new(ProcessReceiptBoundary {
        marker,
        starts: AtomicUsize::new(0),
        reconciles: AtomicUsize::new(0),
    });
    let session = Session::open(&path)
        .await
        .expect("reopen after actual process loss");
    let handle = session.handle();
    let request = SnapshotRequest {
        conversation: session.primary_conversation(),
        max_inputs: 16,
        max_entries: 16,
        max_bytes: 1024 * 1024,
    };
    let before = handle.snapshot(request).await.expect("passive inspection");
    let turn = before.unfinished_turn.as_ref().expect("unfinished").id;
    assert_eq!(before.model_attempts.len(), 1);
    assert!(matches!(
        before.model_attempts[0].state,
        ModelAttemptState::IntentCommitted {
            start_receipt: None
        }
    ));
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
    assert_eq!(boundary.reconciles.load(Ordering::SeqCst), 0);
    assert_eq!(
        handle
            .snapshot(request)
            .await
            .expect("still passive")
            .coverage,
        before.coverage
    );

    let boundaries = allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>]);
    assert_eq!(
        handle
            .resume(turn, boundaries)
            .await
            .expect("explicit reconcile"),
        DriveExit::Parked(ParkReason::RecoveryRequired)
    );
    let after = handle.snapshot(request).await.expect("recovered evidence");
    assert!(after.coverage > before.coverage);
    assert_eq!(after.model_attempts.len(), 1);
    assert!(matches!(
        after.model_attempts[0].state,
        ModelAttemptState::Indeterminate {
            start_receipt: Some(_),
            ..
        }
    ));
    assert_eq!(boundary.reconciles.load(Ordering::SeqCst), 1);
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 0);
    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

struct NegativeThenCompleteBoundary {
    starts: AtomicUsize,
    reconciles: AtomicUsize,
}

impl NegativeThenCompleteBoundary {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            starts: AtomicUsize::new(0),
            reconciles: AtomicUsize::new(0),
        })
    }
}

impl ModelBoundary for NegativeThenCompleteBoundary {
    fn identity(&self) -> ModelBoundaryIdentity {
        identity()
    }

    fn fingerprint(
        &self,
        request: &ion_core::SemanticRequest,
        effect_key: &str,
    ) -> Result<ContentDigest, ProviderError> {
        fingerprint(request, effect_key)
    }

    fn start_receipts(&self) -> StartReceiptCapability {
        StartReceiptCapability::Authoritative
    }

    fn start<'a>(
        &'a self,
        _attempt: ion_core::AttemptId,
        _effect_key: String,
        _request: ion_core::SemanticRequest,
        _stop: CancellationToken,
    ) -> BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            let ordinal = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
            if ordinal == 1 {
                return ModelStart::Indeterminate {
                    reason: "first transport outcome unknown".to_owned(),
                    usage: Usage::unknown(),
                    start_receipt: None,
                };
            }
            let response = ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("retry done".to_owned())],
                    provider_replay: None,
                },
                usage: Usage::known(9, 3),
                termination: ResponseTermination::Completed,
            };
            let stream: ion_ai::ModelStream = Box::pin(futures_util::stream::iter([Ok(
                ModelStreamEvent::Completed(response),
            )]));
            ModelStart::Started {
                stream,
                start_receipt: None,
            }
        })
    }

    fn reconcile_start<'a>(
        &'a self,
        _attempt: ion_core::AttemptId,
        _effect_key: String,
    ) -> BoxFuture<'a, StartReconciliation> {
        Box::pin(async move {
            self.reconciles.fetch_add(1, Ordering::SeqCst);
            StartReconciliation::NotStarted {
                reason: "authoritative provider ledger has no start record".to_owned(),
            }
        })
    }
}

#[tokio::test]
async fn authoritative_negative_is_durable_before_retry_attempt() {
    let (dir, path) = database("authoritative-negative");
    let mut cfg = config();
    cfg.providers[0].start_receipts = StartReceiptCapability::Authoritative;
    let created = Session::create(&path, cfg).await.expect("create");
    let session = created.session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;

    let boundary = NegativeThenCompleteBoundary::new();
    let boundaries = allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>]);
    let first = handle.resume(turn, boundaries).await.expect("first resume");
    assert_eq!(first, DriveExit::Parked(ParkReason::RecoveryRequired));
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 1);
    assert_eq!(boundary.reconciles.load(Ordering::SeqCst), 0);

    let established = handle
        .snapshot_and_watch(WatchRequest {
            snapshot: SnapshotRequest {
                conversation: session.primary_conversation(),
                max_inputs: 16,
                max_entries: 16,
                max_bytes: 1024 * 1024,
            },
            queue: WatchQueueLimits {
                max_receipts: 32,
                max_bytes: 1024 * 1024,
            },
        })
        .await
        .expect("watch");
    assert_eq!(established.snapshot.model_attempts.len(), 1);
    let first_attempt = &established.snapshot.model_attempts[0];
    assert_eq!(first_attempt.ordinal, 1);
    assert!(matches!(
        &first_attempt.state,
        ModelAttemptState::Indeterminate { .. }
    ));

    let boundaries = allowed_boundaries([boundary.clone() as Arc<dyn ModelBoundary>]);
    let second = handle
        .resume(turn, boundaries)
        .await
        .expect("second resume");
    assert!(matches!(
        second,
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    assert_eq!(boundary.reconciles.load(Ordering::SeqCst), 1);
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 2);

    let mut not_started_seq = None;
    let mut retry_intent_seq = None;
    loop {
        let receipt = match established.watch.try_recv() {
            Ok(receipt) => receipt,
            Err(ObservationError::Empty) => break,
            Err(error) => panic!("watch failed: {error}"),
        };
        for change in &receipt.update.changes {
            if let SessionChange::ModelAttempt(attempt) = change {
                if attempt.ordinal == 1
                    && matches!(&attempt.state, ModelAttemptState::NotStarted { .. })
                {
                    not_started_seq = Some(receipt.seq);
                }
                if attempt.ordinal == 2
                    && matches!(&attempt.state, ModelAttemptState::IntentCommitted { .. })
                {
                    retry_intent_seq = Some(receipt.seq);
                }
            }
        }
    }
    let not_started_seq = not_started_seq.expect("authoritative negative commit");
    let retry_intent_seq = retry_intent_seq.expect("retry intent commit");
    assert!(
        not_started_seq < retry_intent_seq,
        "NotStarted evidence must commit before a replacement physical attempt"
    );

    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}
