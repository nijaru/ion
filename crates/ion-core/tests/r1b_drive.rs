use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ion_ai::{
    BoxFuture, Content, GenerationControls, Message, ModelRef, ModelResponse, ModelStreamEvent,
    ProviderError, ProviderErrorKind, Reasoning, ResponseTermination, Role, ToolChoice, Usage,
};
use ion_core::{
    AdmitInputRequest, AuthorityCeiling, ContentDigest, ContextPolicy, ControlCeiling,
    ConversationConfig, DriveExit, EgressRealm, InputBody, InputMode, InputSender, ModelBoundaries,
    ModelBoundary, ModelBoundaryIdentity, ModelStart, ParkReason, ProviderBinding,
    ProviderBindingId, ProviderCapabilities, RequestKey, ReturnedModelPolicy,
    SemanticCompatibilityId, Session, SessionId, SnapshotRequest, StartReceiptCapability,
    StartTurnRequest, TurnLimits, TurnOutcome, WorkspaceBinding,
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
    }
}

fn fingerprint(request: &ion_core::SemanticRequest) -> Result<ContentDigest, ProviderError> {
    ContentDigest::of(request).map_err(|error| ProviderError {
        kind: ProviderErrorKind::InvalidRequest,
        message: error.to_string(),
    })
}

struct CompleteBoundary {
    starts: AtomicUsize,
}

impl CompleteBoundary {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            starts: AtomicUsize::new(0),
        })
    }
}

impl ModelBoundary for CompleteBoundary {
    fn identity(&self) -> ModelBoundaryIdentity {
        identity()
    }

    fn fingerprint(
        &self,
        request: &ion_core::SemanticRequest,
    ) -> Result<ContentDigest, ProviderError> {
        fingerprint(request)
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
    ) -> Result<ContentDigest, ProviderError> {
        fingerprint(request)
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
async fn resume_dispatches_and_selects_one_final_response() {
    let (dir, path) = database("final");
    let created = Session::create(&path, config()).await.expect("create");
    let session = created.session;
    let (handle, turn) = started_turn(&session, "one", "hello").await;
    let boundary = CompleteBoundary::new();
    let boundaries =
        ModelBoundaries::new([boundary.clone() as Arc<dyn ModelBoundary>]).expect("boundaries");

    let exit = handle.resume(turn, boundaries).await.expect("resume");
    assert!(matches!(
        exit,
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    assert_eq!(boundary.starts.load(Ordering::SeqCst), 1);

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
    let boundaries =
        ModelBoundaries::new([boundary.clone() as Arc<dyn ModelBoundary>]).expect("boundaries");

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
    let boundaries =
        ModelBoundaries::new([boundary.clone() as Arc<dyn ModelBoundary>]).expect("boundaries");

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
