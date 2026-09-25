use ion_ai::*;
use ion_core::ToolResult;
use ion_core::*;
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
#[path = "c1_tools/process_loss.rs"]
mod process_loss;
mod artifacts;
fn id(s: &str) -> SemanticCompatibilityId {
    SemanticCompatibilityId::new(s).unwrap()
}
fn binding() -> ToolBinding {
    let mut b = ToolBinding::new(ToolBindingId::new("read").unwrap(), ToolSpec { name: "read".into(),description: "scripted read".into(),input_schema: json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}) },id("read-v1"),ToolConcurrency::Serial,ToolRecoveryPolicy::RepeatAfterNotStartedOrNoMutation,EgressRealm::Local).unwrap();
    b.start_receipts = StartReceiptCapability::Authoritative;
    b
}
fn config() -> ConversationConfig {
    let p = ProviderBindingId::new("script").unwrap();
    ConversationConfig {
        instructions: "test".into(),
        project_context: vec![],
        providers: vec![ProviderBinding {
            id: p.clone(),
            model: ModelRef {
                provider: "script".into(),
                model: "test".into(),
            },
            adapter: id("v1"),
            request_encoding: id("v1"),
            replay_family: None,
            capabilities: ProviderCapabilities {
                max_input_tokens: 100_000,
                max_output_tokens: 8192,
                tools: true,
                parallel_tool_calls: false,
                structured_output: false,
                replay: false,
                reasoning: false,
            },
            returned_model: ReturnedModelPolicy::Exact,
            start_receipts: StartReceiptCapability::None,
            egress: EgressRealm::Local,
        }],
        default_provider: p,
        fallback_route: vec![],
        compaction_route: vec![],
        tools: vec![binding()],
        initial_tools: vec![binding().id],
        controls: GenerationControls {
            max_output_tokens: 4096,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: false,
        },
        control_ceiling: ControlCeiling {
            max_output_tokens: 8192,
            sampling: false,
            parallel_tool_calls: false,
            allowed_reasoning: vec![Reasoning::ProviderDefault],
        },
        context: ContextPolicy {
            max_request_bytes: 1_000_000,
            max_input_tokens: 100_000,
            max_checkpoint_bytes: 1024,
            max_tail_bytes: 100_000,
        },
        workspace: WorkspaceBinding {
            id: "w".into(),
            canonical_root: "/unused".into(),
            backend: "script-exec-v1".into(),
            object_identity: "script".into(),
        },
        authority: AuthorityCeiling {
            workspace_mutation: false,
            unconfined_execution: false,
            remote_tools: false,
            egress_realms: vec![EgressRealm::Local],
        },
        limits: TurnLimits {
            max_model_steps: 4,
            max_model_attempts_per_step: 2,
            max_tool_invocations: 8,
            max_parallel_read_tools: 2,
            max_response_bytes: 64_000,
            max_tool_preview_bytes: 1024,
            max_cost_microusd: None,
        },
    }
}
struct Model {
    starts: AtomicUsize,
    arguments: serde_json::Value,
    calls: usize,
}
impl ModelBoundary for Model {
    fn identity(&self) -> ModelBoundaryIdentity {
        ModelBoundaryIdentity {
            binding: ProviderBindingId::new("script").unwrap(),
            adapter: id("v1"),
            request_encoding: id("v1"),
            egress: EgressRealm::Local,
        }
    }
    fn fingerprint(&self, r: &SemanticRequest, k: &str) -> Result<ContentDigest, ProviderError> {
        Ok(ContentDigest::of(&(r, k)).unwrap())
    }
    fn start<'a>(
        &'a self,
        _: AttemptId,
        _: String,
        r: SemanticRequest,
        _: CancellationToken,
    ) -> BoxFuture<'a, ModelStart> {
        Box::pin(async move {
            self.starts.fetch_add(1, Ordering::SeqCst);
            let has_result = r.messages.iter().any(|m| m.role == TranscriptRole::Tool);
            let content = if has_result {
                vec![Content::Text("done".into())]
            } else {
                (0..self.calls)
                    .map(|i| {
                        Content::ToolCall(ToolCall {
                            id: if self.arguments == json!({"path":"duplicate-ids"}) {
                                "duplicate".into()
                            } else {
                                format!("call-{i}")
                            },
                            name: r.tools[i % r.tools.len()].name.clone(),
                            arguments: self.arguments.clone(),
                        })
                    })
                    .collect()
            };
            let response = ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content,
                    provider_replay: None,
                },
                usage: Usage::known(10, 5),
                termination: ResponseTermination::Completed,
            };
            ModelStart::Started {
                stream: Box::pin(futures_util::stream::iter([Ok(
                    ModelStreamEvent::Completed(response),
                )])),
                start_receipt: None,
            }
        })
    }
}
struct Tool {
    prepares: AtomicUsize,
    executes: AtomicUsize,
    reconciles: AtomicUsize,
    state: Mutex<ToolAttemptState>,
    recovery: Mutex<ToolAttemptState>,
    wait: bool,
    started: tokio::sync::Notify,
    denied: AtomicBool,
    ask: AtomicBool,
    revoke_after_preflight: AtomicBool,
    fail_prepare: AtomicBool,
    authority: ToolAuthority,
}
fn success() -> ToolAttemptState {
    ToolAttemptState::Settled {
        result: ToolResult {
            value: json!("ok"),
            is_error: false,
            capture: OutputCapture::CompleteInline,
        },
        effect: EffectSummary::NoMutation,
        receipt: None,
        retryable: false,
    }
}
fn unknown() -> ToolAttemptState {
    ToolAttemptState::Indeterminate {
        reason: "owner lost".into(),
        receipt: None,
    }
}
impl Tool {
    fn new(state: ToolAttemptState) -> Self {
        Self {
            prepares: AtomicUsize::new(0),
            executes: AtomicUsize::new(0),
            reconciles: AtomicUsize::new(0),
            state: Mutex::new(state),
            recovery: Mutex::new(unknown()),
            wait: false,
            started: tokio::sync::Notify::new(),
            denied: AtomicBool::new(false),
            ask: AtomicBool::new(false),
            revoke_after_preflight: AtomicBool::new(false),
            fail_prepare: AtomicBool::new(false),
            authority: ToolAuthority::ReadOnly,
        }
    }
}
impl ToolBoundary for Tool {
    fn binding(&self) -> ToolBinding {
        let mut binding = binding();
        if self.authority != ToolAuthority::ReadOnly {
            binding.concurrency = ToolConcurrency::Serial;
        }
        binding
    }
    fn executor(&self) -> SemanticCompatibilityId {
        id("script-exec-v1")
    }
    fn prepare(&self, arguments: serde_json::Value) -> Result<PreparedAction, ToolBoundaryError> {
        self.prepares.fetch_add(1, Ordering::SeqCst);
        if self.fail_prepare.load(Ordering::SeqCst) {
            return Err(ToolBoundaryError::InvalidAction);
        }
        Ok(PreparedAction::new(
            binding().id,
            arguments,
            EgressRealm::Local,
            self.authority,
            None,
            vec![],
        )
        .unwrap())
    }
    fn permits_retry(&self) -> bool {
        true
    }
    fn live_authority(&self, _: &PreparedAction, _: &WorkspaceBinding) -> LiveToolAuthority {
        let policy = if self.denied.load(Ordering::SeqCst) {
            LiveToolAuthority::Deny
        } else if self.ask.load(Ordering::SeqCst) {
            LiveToolAuthority::Ask
        } else {
            LiveToolAuthority::Allow
        };
        if policy != LiveToolAuthority::Deny
            && self.revoke_after_preflight.swap(false, Ordering::SeqCst)
        {
            self.denied.store(true, Ordering::SeqCst);
        }
        policy
    }
    fn execute<'a>(
        &'a self,
        execution: ToolExecution,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move {
            let approved = execution.approval.permits(
                &execution.action,
                &execution.binding.implementation,
                &self.executor(),
                &execution.workspace,
                i64::try_from(
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis(),
                )
                .unwrap(),
            );
            if self.denied.load(Ordering::SeqCst)
                || (self.ask.load(Ordering::SeqCst) && !approved)
                || stop.is_cancelled()
            {
                return ToolAttemptState::NotStarted {
                    reason: "live policy denied".into(),
                };
            }
            self.executes.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            if self.wait {
                stop.cancelled().await;
            }
            self.state.lock().unwrap().clone()
        })
    }
    fn reconcile<'a>(
        &'a self,
        _: ToolExecution,
        _: ToolAttempt,
    ) -> BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move {
            self.reconciles.fetch_add(1, Ordering::SeqCst);
            self.recovery.lock().unwrap().clone()
        })
    }
}
fn alternate_binding() -> ToolBinding {
    let mut b = binding();
    b.id = ToolBindingId::new("other").unwrap();
    b.spec.name = "other".into();
    b.implementation = id("other-v1");
    b
}
struct AlternateTool;
impl ToolBoundary for AlternateTool {
    fn binding(&self) -> ToolBinding {
        alternate_binding()
    }
    fn executor(&self) -> SemanticCompatibilityId {
        id("script-exec-v1")
    }
    fn prepare(&self, arguments: serde_json::Value) -> Result<PreparedAction, ToolBoundaryError> {
        Ok(PreparedAction::new(
            alternate_binding().id,
            arguments,
            EgressRealm::Local,
            ToolAuthority::ReadOnly,
            None,
            vec![],
        )
        .unwrap())
    }
    fn live_authority(&self, _: &PreparedAction, _: &WorkspaceBinding) -> LiveToolAuthority {
        LiveToolAuthority::Allow
    }
    fn execute<'a>(
        &'a self,
        _: ToolExecution,
        _: CancellationToken,
    ) -> BoxFuture<'a, ToolAttemptState> {
        Box::pin(async { panic!("alternate backend must disappear before selection") })
    }
}
fn models(m: &Arc<Model>) -> ModelBoundaries {
    ModelBoundaries::new(
        [Arc::clone(m) as Arc<dyn ModelBoundary>],
        Arc::new(|_: &ProviderBinding| Ok(())),
    )
    .unwrap()
}
fn tools(t: &Arc<Tool>) -> ToolBoundaries {
    ToolBoundaries::new([Arc::clone(t) as Arc<dyn ToolBoundary>]).unwrap()
}
async fn setup(c: ConversationConfig) -> (Session, PathBuf, TurnId) {
    let path = std::env::temp_dir().join(format!("ion-r1c-{}.sqlite", SessionId::new()));
    let created = Session::create(&path, c).await.unwrap();
    let session = created.session;
    let h = session.handle();
    let conversation = session.primary_conversation();
    let admission = h
        .admit_input(
            conversation,
            AdmitInputRequest {
                sender: InputSender::User,
                mode: InputMode::Submit,
                request_key: None,
                body: InputBody::Text("read".into()),
            },
        )
        .await
        .unwrap();
    let turn = h
        .start_turn(StartTurnRequest {
            conversation,
            input: admission.input().id,
            admitted_at_unix_ms: 0,
            wall_deadline_unix_ms: None,
        })
        .await
        .unwrap()
        .turn
        .id;
    (session, path, turn)
}
fn model() -> Arc<Model> {
    Arc::new(Model {
        starts: AtomicUsize::new(0),
        arguments: json!({"path":"x"}),
        calls: 1,
    })
}
async fn tool_step(s: &Session) -> StepId {
    let entries = s
        .handle()
        .page_entries(s.primary_conversation(), None, 100)
        .await
        .unwrap();
    entries
        .entries
        .iter()
        .find_map(|e| match e.data {
            EntryData::Assistant { step } => Some(step),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn native_read_settles_a_durable_tool_exchange_and_reopens_without_rereading() {
    let dir = std::env::temp_dir().join(format!("ion-native-exchange-{}", SessionId::new()));
    let workspace = dir.join("workspace");
    let host_state = dir.join("host");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&host_state).unwrap();
    std::fs::write(workspace.join("x"), "native data").unwrap();
    let mut registry = workspace_registry::WorkspaceRegistry::open(&host_state).unwrap();
    let binding = registry.bind("w", &workspace, "native-v1").unwrap();
    let reader = Arc::new(NativeReadBoundary::new(&registry, binding.clone(), 64).unwrap());
    reader.set_live_authority(LiveToolAuthority::Allow);
    let mut cfg = config();
    cfg.workspace = binding;
    cfg.tools = vec![reader.tool_binding().clone()];
    cfg.initial_tools = vec![reader.tool_binding().id.clone()];
    let (session, db, turn) = setup(cfg).await;
    let model = model();
    let tools = ToolBoundaries::new([reader as Arc<dyn ToolBoundary>]).unwrap();
    assert!(matches!(
        session
            .handle()
            .resume_with_tools(turn, models(&model), tools.clone(), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    let step = tool_step(&session).await;
    let before = session.handle().tool_records(step).await.unwrap();
    assert_eq!(before.attempts.len(), 1);
    match &before.attempts[0].state {
        ToolAttemptState::Settled { result, effect, .. } => {
            assert_eq!(result.value["content"], "native data");
            assert!(matches!(result.capture, OutputCapture::CompleteInline));
            assert_eq!(*effect, EffectSummary::NoMutation);
        }
        other => panic!("native read not settled: {other:?}"),
    }
    session.close().await.unwrap();
    std::fs::remove_file(workspace.join("x")).unwrap();
    let reopened = Session::open(&db).await.unwrap();
    assert_eq!(reopened.handle().tool_records(step).await.unwrap(), before);
    assert!(matches!(
        reopened
            .handle()
            .resume(turn, models(&model))
            .await
            .unwrap(),
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    reopened.close().await.unwrap();
    drop(registry);
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_file(db).unwrap();
}

#[tokio::test]
async fn authoritative_negative_recovery_creates_distinct_attempt_without_repreparing() {
    let (s, path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(unknown()));
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::RecoveryRequired)
    );
    let step = tool_step(&s).await;
    let first = s.handle().tool_records(step).await.unwrap().attempts[0].clone();
    s.close().await.unwrap();
    *t.recovery.lock().unwrap() = ToolAttemptState::NotStarted {
        reason: "authoritative negative".into(),
    };
    *t.state.lock().unwrap() = success();
    let s = Session::open(&path).await.unwrap();
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(_)
    ));
    let records = s.handle().tool_records(step).await.unwrap();
    assert_eq!(records.attempts.len(), 2);
    assert_eq!(records.attempts[0].id, first.id);
    assert_ne!(records.attempts[0].id, records.attempts[1].id);
    assert!(matches!(
        records.attempts[0].state,
        ToolAttemptState::NotStarted { .. }
    ));
    assert_eq!(t.prepares.load(Ordering::SeqCst), 1);
    assert_eq!(t.executes.load(Ordering::SeqCst), 2);
    s.close().await.unwrap();
}

#[tokio::test]
async fn frozen_authority_blocks_execution_intent_across_reopen() {
    for authority in [
        ToolAuthority::WorkspaceMutation,
        ToolAuthority::UnconfinedExecution,
    ] {
        let mut cfg = config();
        cfg.tools[0].concurrency = ToolConcurrency::Serial;
        cfg.authority.workspace_mutation = authority == ToolAuthority::UnconfinedExecution;
        cfg.authority.unconfined_execution = false;
        let (s, path, turn) = setup(cfg).await;
        let mut tool = Tool::new(success());
        tool.authority = authority;
        let t = Arc::new(tool);
        let m = model();
        assert_eq!(
            s.handle()
                .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
                .await
                .unwrap(),
            DriveExit::Parked(ParkReason::AuthorityDenied)
        );
        let step = tool_step(&s).await;
        let before = s.handle().tool_records(step).await.unwrap();
        assert!(before.attempts.is_empty());
        assert_eq!(
            before.invocations[0].preparation.ready().unwrap().authority,
            authority
        );
        s.close().await.unwrap();
        let s = Session::open(&path).await.unwrap();
        assert_eq!(
            s.handle()
                .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
                .await
                .unwrap(),
            DriveExit::Parked(ParkReason::AuthorityDenied)
        );
        assert!(
            s.handle()
                .tool_records(step)
                .await
                .unwrap()
                .attempts
                .is_empty()
        );
        assert_eq!(t.prepares.load(Ordering::SeqCst), 1);
        assert_eq!(t.executes.load(Ordering::SeqCst), 0);
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn revoked_live_authority_uses_no_attempt_until_restored() {
    let (s, path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(success()));
    t.denied.store(true, Ordering::SeqCst);
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::AuthorityDenied)
    );
    let step = tool_step(&s).await;
    assert!(
        s.handle()
            .tool_records(step)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);
    s.close().await.unwrap();

    // A passive reopen neither authorizes work nor re-prepares the saved action.
    let s = Session::open(&path).await.unwrap();
    assert!(
        s.handle()
            .tool_records(step)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    t.denied.store(false, Ordering::SeqCst);
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(_)
    ));
    assert_eq!(t.prepares.load(Ordering::SeqCst), 1);
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    assert_eq!(
        s.handle().tool_records(step).await.unwrap().attempts.len(),
        1
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn revocation_after_preflight_does_not_burn_retry_attempts() {
    let (s, _path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(success()));
    t.revoke_after_preflight.store(true, Ordering::SeqCst);
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::AuthorityDenied)
    );
    let step = tool_step(&s).await;
    let records = s.handle().tool_records(step).await.unwrap();
    assert_eq!(records.attempts.len(), 1);
    assert!(matches!(
        records.attempts[0].state,
        ToolAttemptState::NotStarted { .. }
    ));
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);

    t.denied.store(false, Ordering::SeqCst);
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(_)
    ));
    assert_eq!(
        s.handle().tool_records(step).await.unwrap().attempts.len(),
        2
    );
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    s.close().await.unwrap();
}

#[tokio::test]
async fn exact_approval_survives_passive_reopen_and_rejects_stale_decisions() {
    let (s, path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(success()));
    t.ask.store(true, Ordering::SeqCst);
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::AwaitingApproval)
    );
    let step = tool_step(&s).await;
    let before = s.handle().tool_records(step).await.unwrap();
    let call = &before.invocations[0];
    assert_eq!(call.approval, ApprovalState::Pending);
    assert!(before.attempts.is_empty());
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);
    let digest = call.preparation.ready().unwrap().digest;
    let invocation = call.id;
    s.close().await.unwrap();

    let s = Session::open(&path).await.unwrap();
    assert_eq!(s.handle().tool_records(step).await.unwrap(), before);
    let watch = s
        .handle()
        .snapshot_and_watch(WatchRequest {
            snapshot: SnapshotRequest {
                conversation: s.primary_conversation(),
                max_inputs: 0,
                max_entries: 0,
                max_bytes: 1024 * 1024,
            },
            queue: WatchQueueLimits {
                max_receipts: 8,
                max_bytes: 1024 * 1024,
            },
        })
        .await
        .unwrap();
    assert_eq!(
        watch.snapshot.tool_invocations[0].approval,
        ApprovalState::Pending
    );
    let expiry = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
        + 60_000;
    assert!(
        s.handle()
            .decide_tool_approval(
                step,
                invocation,
                ContentDigest::of(&"different action").unwrap(),
                ApprovalDecision::Approve {
                    expires_at_unix_ms: expiry
                },
                t.executor(),
            )
            .await
            .is_err()
    );
    assert_eq!(s.handle().tool_records(step).await.unwrap(), before);
    assert!(
        s.handle()
            .decide_tool_approval(
                step,
                invocation,
                digest,
                ApprovalDecision::Approve {
                    expires_at_unix_ms: 0
                },
                t.executor(),
            )
            .await
            .is_err()
    );
    assert_eq!(s.handle().tool_records(step).await.unwrap(), before);
    assert!(
        s.handle()
            .decide_tool_approval(
                step,
                invocation,
                digest,
                ApprovalDecision::Approve {
                    expires_at_unix_ms: expiry
                },
                id("different-executor")
            )
            .await
            .is_err()
    );
    assert_eq!(s.handle().tool_records(step).await.unwrap(), before);
    let grant = ApprovalDecision::Approve {
        expires_at_unix_ms: expiry,
    };
    let receipt = s
        .handle()
        .decide_tool_approval(step, invocation, digest, grant.clone(), t.executor())
        .await
        .unwrap()
        .expect("new approval commit");
    assert!(receipt.seq > watch.snapshot.coverage);
    assert_eq!(watch.watch.try_recv().unwrap(), receipt);
    assert!(
        s.handle()
            .decide_tool_approval(step, invocation, digest, grant.clone(), t.executor())
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        watch.watch.try_recv(),
        Err(ObservationError::Empty)
    ));
    assert!(
        s.handle()
            .decide_tool_approval(
                step,
                invocation,
                digest,
                ApprovalDecision::Deny {
                    reason: "conflict".into()
                },
                t.executor()
            )
            .await
            .is_err()
    );
    assert!(matches!(
        s.handle().tool_records(step).await.unwrap().invocations[0].approval,
        ApprovalState::Approved { .. }
    ));
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(_)
    ));
    assert_eq!(t.prepares.load(Ordering::SeqCst), 1);
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    assert_eq!(
        s.handle().tool_records(step).await.unwrap().attempts.len(),
        1
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn expired_approval_reopens_pending_without_an_attempt() {
    let (s, path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(success()));
    t.ask.store(true, Ordering::SeqCst);
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::AwaitingApproval)
    );
    let step = tool_step(&s).await;
    let call = s.handle().tool_records(step).await.unwrap().invocations[0].clone();
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    s.handle()
        .decide_tool_approval(
            step,
            call.id,
            call.preparation.ready().unwrap().digest,
            ApprovalDecision::Approve {
                expires_at_unix_ms: now + 60_000,
            },
            t.executor(),
        )
        .await
        .unwrap();
    s.close().await.unwrap();

    // Exact persisted pre-state for an expiry after the approving process exits.
    let db = rusqlite::Connection::open(&path).unwrap();
    let raw: String = db
        .query_row(
            "SELECT approval FROM tool_invocations WHERE id=?1",
            [call.id.get()],
            |row| row.get(0),
        )
        .unwrap();
    let mut saved: serde_json::Value = serde_json::from_str(&raw).unwrap();
    saved["Approved"]["expires_at_unix_ms"] = json!(0);
    db.execute(
        "UPDATE tool_invocations SET approval=?2 WHERE id=?1",
        rusqlite::params![call.id.get(), saved.to_string()],
    )
    .unwrap();
    drop(db);

    let s = Session::open(&path).await.unwrap();
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::AwaitingApproval)
    );
    let after = s.handle().tool_records(step).await.unwrap();
    assert!(after.attempts.is_empty());
    assert_eq!(after.invocations[0].approval, ApprovalState::Pending);
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);
    s.close().await.unwrap();
}

#[tokio::test]
async fn cancellation_and_live_denial_override_a_saved_grant() {
    let (s, _path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(success()));
    t.ask.store(true, Ordering::SeqCst);
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::AwaitingApproval)
    );
    let step = tool_step(&s).await;
    let call = s.handle().tool_records(step).await.unwrap().invocations[0].clone();
    let expiry = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
        + 60_000;
    let grant = ApprovalDecision::Approve {
        expires_at_unix_ms: expiry,
    };
    s.handle()
        .decide_tool_approval(
            step,
            call.id,
            call.preparation.ready().unwrap().digest,
            grant.clone(),
            t.executor(),
        )
        .await
        .unwrap();
    t.denied.store(true, Ordering::SeqCst);
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::AuthorityDenied)
    );
    assert!(
        s.handle()
            .tool_records(step)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);
    s.handle().cancel_turn(turn).await.unwrap();
    assert!(
        s.handle()
            .decide_tool_approval(
                step,
                call.id,
                call.preparation.ready().unwrap().digest,
                ApprovalDecision::Deny {
                    reason: "too late".into()
                },
                t.executor()
            )
            .await
            .is_err()
    );
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(TurnOutcome::Cancelled { .. })
    ));
    assert!(
        s.handle()
            .tool_records(step)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn approval_commit_failure_publishes_nothing_and_fences() {
    let (s, path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(success()));
    t.ask.store(true, Ordering::SeqCst);
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::AwaitingApproval)
    );
    let step = tool_step(&s).await;
    let call = s.handle().tool_records(step).await.unwrap().invocations[0].clone();
    let view = s
        .handle()
        .snapshot_and_watch(WatchRequest {
            snapshot: SnapshotRequest {
                conversation: s.primary_conversation(),
                max_inputs: 0,
                max_entries: 0,
                max_bytes: 1024 * 1024,
            },
            queue: WatchQueueLimits {
                max_receipts: 8,
                max_bytes: 1024 * 1024,
            },
        })
        .await
        .unwrap();
    let injector = rusqlite::Connection::open(&path).unwrap();
    injector
        .execute_batch(
            "CREATE TRIGGER fail_approval BEFORE UPDATE OF approval ON tool_invocations
        BEGIN SELECT RAISE(ABORT, 'injected approval failure'); END;",
        )
        .unwrap();
    let expiry = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
        + 60_000;
    assert!(matches!(
        s.handle()
            .decide_tool_approval(
                step,
                call.id,
                call.preparation.ready().unwrap().digest,
                ApprovalDecision::Approve {
                    expires_at_unix_ms: expiry
                },
                t.executor()
            )
            .await,
        Err(SessionError::Fenced(_))
    ));
    assert_eq!(s.handle().health(), SessionHealth::Fenced);
    assert_eq!(
        s.handle().tool_records(step).await.unwrap().invocations[0].approval,
        ApprovalState::Pending
    );
    assert!(matches!(
        view.watch.try_recv(),
        Err(ObservationError::Empty)
    ));
    assert!(
        s.handle()
            .tool_records(step)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    drop(injector);
    s.close().await.unwrap();
}

#[tokio::test]
async fn denial_stages_one_source_order_result_without_a_tool_attempt() {
    let (s, _path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(success()));
    t.ask.store(true, Ordering::SeqCst);
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::AwaitingApproval)
    );
    let step = tool_step(&s).await;
    let call = &s.handle().tool_records(step).await.unwrap().invocations[0];
    let decision = ApprovalDecision::Deny {
        reason: "no".into(),
    };
    assert!(
        s.handle()
            .decide_tool_approval(
                step,
                call.id,
                call.preparation.ready().unwrap().digest,
                decision.clone(),
                t.executor()
            )
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        s.handle()
            .decide_tool_approval(
                step,
                call.id,
                call.preparation.ready().unwrap().digest,
                decision,
                t.executor()
            )
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        s.handle().tool_records(step).await.unwrap().invocations[0].exchange,
        ToolExchangeState::OutcomeReady {
            source: OutcomeSource::DeniedApproval,
            ..
        }
    ));
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(_)
    ));
    let records = s.handle().tool_records(step).await.unwrap();
    assert!(records.attempts.is_empty());
    assert!(matches!(
        records.invocations[0].exchange,
        ToolExchangeState::Materialized {
            source: OutcomeSource::DeniedApproval,
            ..
        }
    ));
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);
    s.close().await.unwrap();
}

#[tokio::test]
async fn known_mutation_never_retries_even_when_error_is_classified_retryable() {
    let (s, _path, turn) = setup(config()).await;
    let m = model();
    let mut state = success();
    if let ToolAttemptState::Settled {
        result,
        effect,
        retryable,
        ..
    } = &mut state
    {
        result.is_error = true;
        *effect = EffectSummary::MayHaveMutated;
        *retryable = true;
    }
    let t = Arc::new(Tool::new(state));
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(_)
    ));
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    s.close().await.unwrap();
}

#[tokio::test]
async fn oversized_backend_output_preserves_terminal_effect_without_reexecution() {
    let (s, path, turn) = setup(config()).await;
    let m = model();
    let mut state = success();
    if let ToolAttemptState::Settled { result, .. } = &mut state {
        result.value = json!("x".repeat(2048));
    }
    let t = Arc::new(Tool::new(state));
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(_)
    ));
    let step = tool_step(&s).await;
    let records = s.handle().tool_records(step).await.unwrap();
    assert_eq!(records.attempts.len(), 1);
    assert!(matches!(
        &records.attempts[0].state,
        ToolAttemptState::Settled {
            result: ToolResult {
                capture: OutputCapture::Incomplete {
                    reason: OutputLoss::BackendCapacity,
                    ..
                },
                ..
            },
            effect: EffectSummary::NoMutation,
            retryable: false,
            ..
        }
    ));
    assert!(matches!(
        records.invocations[0].exchange,
        ToolExchangeState::Materialized { .. }
    ));
    s.close().await.unwrap();
    let s = Session::open(&path).await.unwrap();
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(_)
    ));
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    s.close().await.unwrap();
}

#[tokio::test]
async fn terminal_evidence_materializes_without_an_executor_but_unknown_stays_pending() {
    for state in [
        success(),
        ToolAttemptState::NotStarted {
            reason: "authoritative negative".into(),
        },
        unknown(),
    ] {
        let terminal = matches!(
            state,
            ToolAttemptState::Settled { .. } | ToolAttemptState::NotStarted { .. }
        );
        let (s, path, turn) = setup(config()).await;
        let m = model();
        let t = Arc::new(Tool::new(unknown()));
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap();
        let step = tool_step(&s).await;
        let before = s.handle().tool_records(step).await.unwrap();
        s.close().await.unwrap();
        // Crash-window pre-state: evidence committed, outcome not yet staged.
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute(
            "UPDATE tool_attempts SET state=?1 WHERE id=?2",
            rusqlite::params![
                serde_json::to_string(&state).unwrap(),
                before.attempts[0].id.to_string().parse::<i64>().unwrap()
            ],
        )
        .unwrap();
        drop(db);
        let s = Session::open(&path).await.unwrap();
        assert_eq!(
            s.handle()
                .resume_with_tools(
                    turn,
                    models(&m),
                    ToolBoundaries::default(),
                    DrivePolicy::default()
                )
                .await
                .unwrap(),
            DriveExit::Parked(ParkReason::ToolUnavailable)
        );
        let after = s.handle().tool_records(step).await.unwrap();
        assert_eq!(after.attempts.len(), 1);
        assert_eq!(after.attempts[0].id, before.attempts[0].id);
        assert_eq!(after.attempts[0].state, state);
        assert_eq!(
            matches!(
                after.invocations[0].exchange,
                ToolExchangeState::Materialized { .. }
            ),
            terminal
        );
        assert_eq!(t.executes.load(Ordering::SeqCst), 1);
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn staged_b_c_survive_reopen_without_execution_and_materialize_after_a() {
    let (s, path, turn) = setup(config()).await;
    let m = Arc::new(Model {
        starts: AtomicUsize::new(0),
        arguments: json!({"path":"x"}),
        calls: 3,
    });
    let t = Arc::new(Tool::new(unknown()));
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::RecoveryRequired)
    );
    let step = tool_step(&s).await;
    let records = s.handle().tool_records(step).await.unwrap();
    s.close().await.unwrap();
    // Durable pre-state fixture: a future bounded parallel executor completed B/C
    // while A remained indeterminate. This does not claim parallel dispatch coverage.
    let mut db = rusqlite::Connection::open(&path).unwrap();
    let tx = db.transaction().unwrap();
    let mut seq: i64 = tx
        .query_row("SELECT last_seq FROM session_meta", [], |r| r.get(0))
        .unwrap();
    for call in &records.invocations[1..] {
        seq += 1;
        let attempt: AttemptId = serde_json::from_value(json!(seq)).unwrap();
        let state = success();
        tx.execute("INSERT INTO tool_attempts (id,invocation_id,ordinal,generation,executor,state) VALUES (?1,?2,1,0,?3,?4)",rusqlite::params![seq,call.id.to_string().parse::<i64>().unwrap(),serde_json::to_string(&id("script-exec-v1")).unwrap(),serde_json::to_string(&state).unwrap()]).unwrap();
        let ToolAttemptState::Settled { result, .. } = state else {
            unreachable!()
        };
        let exchange = ToolExchangeState::OutcomeReady {
            source: OutcomeSource::Attempt(attempt),
            result,
        };
        tx.execute(
            "UPDATE tool_invocations SET exchange_state=?2 WHERE id=?1",
            rusqlite::params![
                call.id.to_string().parse::<i64>().unwrap(),
                serde_json::to_string(&exchange).unwrap()
            ],
        )
        .unwrap();
    }
    tx.execute("UPDATE session_meta SET last_seq=?1", [seq])
        .unwrap();
    tx.commit().unwrap();
    drop(db);
    let s = Session::open(&path).await.unwrap();
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::RecoveryRequired)
    );
    let staged = s.handle().tool_records(step).await.unwrap();
    assert!(
        staged.invocations[1..]
            .iter()
            .all(|c| matches!(c.exchange, ToolExchangeState::OutcomeReady { .. }))
    );
    *t.recovery.lock().unwrap() = success();
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(_)
    ));
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    let entries = s
        .handle()
        .page_entries(s.primary_conversation(), None, 100)
        .await
        .unwrap();
    let actual: Vec<_> = entries
        .entries
        .iter()
        .filter_map(|e| match e.data {
            EntryData::ToolResult { invocation } => Some(invocation),
            _ => None,
        })
        .collect();
    assert_eq!(
        actual,
        records.invocations.iter().map(|c| c.id).collect::<Vec<_>>()
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn cancellation_closes_pending_known_and_staged_results_without_backends() {
    for checkpoint in 0..3 {
        let (s, path, turn) = setup(config()).await;
        let m = Arc::new(Model {
            starts: AtomicUsize::new(0),
            arguments: json!({"path":"x"}),
            calls: 3,
        });
        let t = Arc::new(Tool::new(unknown()));
        assert_eq!(
            s.handle()
                .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
                .await
                .unwrap(),
            DriveExit::Parked(ParkReason::RecoveryRequired)
        );
        let step = tool_step(&s).await;
        let before = s.handle().tool_records(step).await.unwrap();
        if checkpoint == 2 {
            s.handle()
                .accept_tool_unknown(step, before.invocations[0].id)
                .await
                .unwrap();
        }
        s.close().await.unwrap();
        if checkpoint == 1 {
            // Crash window: terminal evidence committed, not yet staged.
            let db = rusqlite::Connection::open(&path).unwrap();
            db.execute(
                "UPDATE tool_attempts SET state=?2 WHERE id=?1",
                rusqlite::params![
                    before.attempts[0].id.get(),
                    serde_json::to_string(&success()).unwrap(),
                ],
            )
            .unwrap();
        }
        let s = Session::open(&path).await.unwrap();
        s.handle().cancel_turn(turn).await.unwrap();
        assert!(matches!(
            s.handle()
                .resume(turn, ModelBoundaries::default())
                .await
                .unwrap(),
            DriveExit::Settled(TurnOutcome::Cancelled { .. })
        ));
        let closed = s.handle().tool_records(step).await.unwrap();
        assert!(
            closed
                .invocations
                .iter()
                .all(|call| matches!(call.exchange, ToolExchangeState::Materialized { .. }))
        );
        assert_eq!(closed.attempts.len(), 1);
        assert_eq!(
            closed.attempts[0].state,
            if checkpoint == 1 {
                success()
            } else {
                unknown()
            }
        );
        assert!(closed.invocations[1..].iter().all(|call| matches!(
            call.exchange,
            ToolExchangeState::Materialized {
                source: OutcomeSource::CancelledBeforeStart,
                ..
            }
        )));
        assert_eq!(t.executes.load(Ordering::SeqCst), 1);
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn complete_tool_exchange_prepares_once_and_continues() {
    let (s, _path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(success()));
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    let records = s.handle().tool_records(tool_step(&s).await).await.unwrap();
    assert_eq!(records.attempts.len(), 1);
    assert!(matches!(
        records.invocations[0].exchange,
        ToolExchangeState::Materialized {
            source: OutcomeSource::Attempt(_),
            ..
        }
    ));
    assert_eq!(t.prepares.load(Ordering::SeqCst), 1);
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    assert_eq!(m.starts.load(Ordering::SeqCst), 2);
    s.close().await.unwrap();
}

#[tokio::test]
async fn response_ready_settles_mixed_unavailable_calls_without_inventing_actions() {
    let mut c = config();
    c.tools.push(alternate_binding());
    c.initial_tools.push(alternate_binding().id);
    let (s, path, turn) = setup(c).await;
    let m = Arc::new(Model {
        starts: AtomicUsize::new(0),
        arguments: json!({"path":"x"}),
        calls: 2,
    });
    let t = Arc::new(Tool::new(success()));
    t.fail_prepare.store(true, Ordering::SeqCst);
    let both = ToolBoundaries::new([
        Arc::clone(&t) as Arc<dyn ToolBoundary>,
        Arc::new(AlternateTool) as Arc<dyn ToolBoundary>,
    ])
    .unwrap();
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), both, DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Faulted { .. }
    ));
    assert_eq!(m.starts.load(Ordering::SeqCst), 1);
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);
    s.close().await.unwrap();

    t.fail_prepare.store(false, Ordering::SeqCst);
    let s = Session::open(&path).await.unwrap();
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::ToolUnavailable)
    );
    let step = tool_step(&s).await;
    let records = s.handle().tool_records(step).await.unwrap();
    assert_eq!(records.invocations.len(), 2);
    assert!(matches!(
        records.invocations[0].preparation,
        ToolPreparation::Ready(_)
    ));
    assert_eq!(
        records.invocations[1].preparation,
        ToolPreparation::Unavailable
    );
    assert_eq!(records.attempts.len(), 1);
    assert_eq!(records.attempts[0].invocation, records.invocations[0].id);
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    assert_eq!(m.starts.load(Ordering::SeqCst), 1);
    assert!(matches!(
        records.invocations[0].exchange,
        ToolExchangeState::Materialized {
            source: OutcomeSource::Attempt(_),
            ..
        }
    ));
    assert!(matches!(
        records.invocations[1].exchange,
        ToolExchangeState::Materialized {
            source: OutcomeSource::Unavailable,
            ..
        }
    ));
    let entries = s
        .handle()
        .page_entries(s.primary_conversation(), None, 100)
        .await
        .unwrap();
    let results: Vec<_> = entries
        .entries
        .iter()
        .filter_map(|e| match e.data {
            EntryData::ToolResult { invocation } => Some(invocation),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        records.invocations.iter().map(|c| c.id).collect::<Vec<_>>()
    );
    s.close().await.unwrap();

    // A second passive reopen does not resurrect, prepare or execute the missing action.
    let s = Session::open(&path).await.unwrap();
    assert_eq!(s.handle().tool_records(step).await.unwrap(), records);
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::ToolUnavailable)
    );
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    s.close().await.unwrap();
}

#[tokio::test]
async fn unavailable_admission_fault_rolls_back_response_selection_and_can_recover() {
    let (s, path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(success()));
    t.fail_prepare.store(true, Ordering::SeqCst);
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Faulted { .. }
    ));
    s.close().await.unwrap();
    let injector = rusqlite::Connection::open(&path).unwrap();
    injector
        .execute_batch(
            "CREATE TRIGGER fail_unavailable BEFORE INSERT ON tool_invocations
        BEGIN SELECT RAISE(ABORT, 'injected unavailable admission fault'); END;",
        )
        .unwrap();
    let s = Session::open(&path).await.unwrap();
    assert!(matches!(
        s.handle()
            .resume_with_tools(
                turn,
                models(&m),
                ToolBoundaries::default(),
                DrivePolicy::default()
            )
            .await
            .unwrap(),
        DriveExit::Faulted { .. }
    ));
    assert_eq!(s.handle().health(), SessionHealth::Fenced);
    assert_eq!(m.starts.load(Ordering::SeqCst), 1);
    assert!(
        s.handle()
            .page_entries(s.primary_conversation(), None, 100)
            .await
            .unwrap()
            .entries
            .iter()
            .all(|e| !matches!(e.data, EntryData::Assistant { .. }))
    );
    s.close().await.unwrap();
    injector
        .execute_batch("DROP TRIGGER fail_unavailable;")
        .unwrap();
    drop(injector);
    let s = Session::open(&path).await.unwrap();
    assert_eq!(
        s.handle()
            .resume_with_tools(
                turn,
                models(&m),
                ToolBoundaries::default(),
                DrivePolicy::default()
            )
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::ToolUnavailable)
    );
    assert_eq!(m.starts.load(Ordering::SeqCst), 1);
    assert!(
        s.handle()
            .tool_records(tool_step(&s).await)
            .await
            .unwrap()
            .attempts
            .is_empty()
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn duplicate_provider_tool_ids_cannot_admit_or_execute_a_batch() {
    let (s, _path, turn) = setup(config()).await;
    let m = Arc::new(Model {
        starts: AtomicUsize::new(0),
        arguments: json!({"path":"duplicate-ids"}),
        calls: 2,
    });
    let t = Arc::new(Tool::new(success()));
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Faulted { .. }
    ));
    assert_eq!(m.starts.load(Ordering::SeqCst), 1);
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);
    assert!(
        s.handle()
            .page_entries(s.primary_conversation(), None, 100)
            .await
            .unwrap()
            .entries
            .iter()
            .all(|e| !matches!(e.data, EntryData::Assistant { .. }))
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn response_ready_without_any_backend_stages_only_unavailable_results() {
    let (s, path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(success()));
    t.fail_prepare.store(true, Ordering::SeqCst);
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Faulted { .. }
    ));
    s.close().await.unwrap();
    let s = Session::open(&path).await.unwrap();
    assert_eq!(
        s.handle()
            .resume_with_tools(
                turn,
                models(&m),
                ToolBoundaries::default(),
                DrivePolicy::default()
            )
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::ToolUnavailable)
    );
    let records = s.handle().tool_records(tool_step(&s).await).await.unwrap();
    assert_eq!(
        records.invocations[0].preparation,
        ToolPreparation::Unavailable
    );
    assert!(records.attempts.is_empty());
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);
    s.close().await.unwrap();
}

#[tokio::test]
async fn missing_frozen_tool_blocks_provider_and_schema_error_blocks_execution() {
    let (s, _path, turn) = setup(config()).await;
    let m = model();
    assert_eq!(
        s.handle().resume(turn, models(&m)).await.unwrap(),
        DriveExit::Parked(ParkReason::ToolUnavailable)
    );
    assert_eq!(m.starts.load(Ordering::SeqCst), 0);
    let bad = Arc::new(Model {
        starts: AtomicUsize::new(0),
        arguments: json!({"path":42}),
        calls: 1,
    });
    let t = Arc::new(Tool::new(success()));
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&bad), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Faulted { .. }
    ));
    assert_eq!(t.prepares.load(Ordering::SeqCst), 0);
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);
    s.close().await.unwrap();
}

#[tokio::test]
async fn unknown_survives_passive_reopen_and_late_evidence_does_not_rewrite_result() {
    let (s, path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(unknown()));
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::RecoveryRequired)
    );
    let step = tool_step(&s).await;
    let before = s.handle().tool_records(step).await.unwrap();
    s.close().await.unwrap();
    let reconciles = t.reconciles.load(Ordering::SeqCst);
    let s = Session::open(&path).await.unwrap();
    assert_eq!(t.reconciles.load(Ordering::SeqCst), reconciles);
    assert_eq!(s.handle().tool_records(step).await.unwrap(), before);
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::RecoveryRequired)
    );
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    assert_eq!(t.prepares.load(Ordering::SeqCst), 1);
    s.handle()
        .accept_tool_unknown(step, before.invocations[0].id)
        .await
        .unwrap();
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Settled(_)
    ));
    let settled = s.handle().tool_records(step).await.unwrap();
    *t.recovery.lock().unwrap() = success();
    s.handle().reconcile_tools(step, tools(&t)).await.unwrap();
    let late = s.handle().tool_records(step).await.unwrap();
    assert_eq!(late.turn, settled.turn);
    assert_eq!(late.invocations, settled.invocations);
    assert!(matches!(
        late.attempts[0].state,
        ToolAttemptState::Settled { .. }
    ));
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    s.close().await.unwrap();
}

#[tokio::test]
async fn active_tool_snapshot_and_watch_share_exact_commit_coverage() {
    let (s, _path, turn) = setup(config()).await;
    let m = model();
    let t = Arc::new(Tool::new(unknown()));
    s.handle()
        .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
        .await
        .unwrap();
    let step = tool_step(&s).await;
    let records = s.handle().tool_records(step).await.unwrap();
    let view = s
        .handle()
        .snapshot_and_watch(WatchRequest {
            snapshot: SnapshotRequest {
                conversation: s.primary_conversation(),
                max_inputs: 0,
                max_entries: 0,
                max_bytes: 1024 * 1024,
            },
            queue: WatchQueueLimits {
                max_receipts: 32,
                max_bytes: 1024 * 1024,
            },
        })
        .await
        .unwrap();
    let projected = serde_json::to_value(&view.snapshot).unwrap();
    assert_eq!(
        projected["tool_invocations"],
        serde_json::to_value(&records.invocations).unwrap()
    );
    assert_eq!(
        projected["tool_attempts"],
        serde_json::to_value(&records.attempts).unwrap()
    );
    let receipt = s
        .handle()
        .accept_tool_unknown(step, records.invocations[0].id)
        .await
        .unwrap();
    assert!(receipt.seq > view.snapshot.coverage);
    assert_eq!(view.watch.try_recv().unwrap(), receipt);
    s.close().await.unwrap();
}

#[tokio::test]
async fn actual_batch_closure_refuses_before_any_tool_attempt() {
    let mut c = config();
    c.context.max_input_tokens = 2000;
    c.providers[0].capabilities.max_input_tokens = 2000;
    let (s, _path, turn) = setup(c).await;
    let m = Arc::new(Model {
        starts: AtomicUsize::new(0),
        arguments: json!({"path":"x"}),
        calls: 3,
    });
    let t = Arc::new(Tool::new(success()));
    assert!(matches!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::Capacity)
    ));
    assert_eq!(t.executes.load(Ordering::SeqCst), 0);
    let entries = s
        .handle()
        .page_entries(s.primary_conversation(), None, 100)
        .await
        .unwrap();
    assert_eq!(entries.entries.len(), 1);
    s.close().await.unwrap();
}

#[tokio::test]
async fn batch_reserves_durable_attempt_and_staging_capacity_before_effects() {
    let mut cfg = config();
    cfg.context.max_request_bytes = 32 * 1024 * 1024;
    cfg.context.max_input_tokens = 32 * 1024 * 1024;
    cfg.providers[0].capabilities.max_input_tokens = 32 * 1024 * 1024;
    cfg.limits.max_response_bytes = 4 * 1024 * 1024;
    cfg.limits.max_tool_invocations = 64;
    cfg.limits.max_tool_preview_bytes = 64 * 1024;
    let (s, _path, turn) = setup(cfg).await;
    let m = Arc::new(Model {
        starts: AtomicUsize::new(0),
        arguments: json!({"path": "x".repeat(30_000)}),
        calls: 64,
    });
    let t = Arc::new(Tool::new(success()));
    assert_eq!(
        s.handle()
            .resume_with_tools(turn, models(&m), tools(&t), DrivePolicy::default())
            .await
            .unwrap(),
        DriveExit::Parked(ParkReason::Capacity)
    );
    assert_eq!(
        t.executes.load(Ordering::SeqCst),
        0,
        "prepared actions, physical attempts and staged result copies must all remain readable"
    );
    assert_eq!(
        s.handle()
            .page_entries(s.primary_conversation(), None, 100)
            .await
            .unwrap()
            .entries
            .len(),
        1
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn cancellation_signals_and_joins_execution_without_fabricating_termination() {
    let (s, _path, turn) = setup(config()).await;
    let m = model();
    let mut tool = Tool::new(unknown());
    tool.wait = true;
    let t = Arc::new(tool);
    let h = s.handle();
    let mt = models(&m);
    let tt = tools(&t);
    let join = tokio::spawn(async move {
        h.resume_with_tools(turn, mt, tt, DrivePolicy::default())
            .await
            .unwrap()
    });
    t.started.notified().await;
    s.handle().cancel_turn(turn).await.unwrap();
    assert!(
        matches!(join.await.unwrap(),DriveExit::Settled(TurnOutcome::Cancelled { unresolved_attempts }) if unresolved_attempts.len()==1)
    );
    let records = s.handle().tool_records(tool_step(&s).await).await.unwrap();
    assert_eq!(records.attempts.len(), 1);
    assert!(matches!(
        records.attempts[0].state,
        ToolAttemptState::Indeterminate { .. }
    ));
    let h = s.handle();
    let conversation = s.primary_conversation();
    let admitted = h
        .admit_input(
            conversation,
            AdmitInputRequest {
                sender: InputSender::User,
                mode: InputMode::Submit,
                request_key: None,
                body: InputBody::Text("continue after cancellation".into()),
            },
        )
        .await
        .unwrap();
    let successor = h
        .start_turn(StartTurnRequest {
            conversation,
            input: admitted.input().id,
            admitted_at_unix_ms: 1,
            wall_deadline_unix_ms: None,
        })
        .await
        .unwrap()
        .turn
        .id;
    assert!(
        matches!(
            h.resume_with_tools(successor, models(&m), tools(&t), DrivePolicy::default())
                .await
                .unwrap(),
            DriveExit::Settled(TurnOutcome::Completed { .. })
        ),
        "cancellation must close the prior exchange truthfully"
    );
    assert_eq!(t.executes.load(Ordering::SeqCst), 1);
    s.close().await.unwrap();
}
