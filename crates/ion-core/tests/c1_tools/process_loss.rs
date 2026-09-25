//! Actual owner death across the Session / host registry / external effect boundary.
use super::*;
use ion_core::workspace_registry::{
    ClaimKey, RegistryReceipt, TerminalEvidence, WorkspaceRegistry, WorkspaceResources,
    WorkspaceRevision,
};
use std::{fs, io::Write, path::Path, process::Stdio, time::Duration};

fn mutation_binding() -> ToolBinding {
    let mut binding = ToolBinding::new(
        ToolBindingId::new("mutate").unwrap(),
        ToolSpec {
            name: "mutate".into(),
            description: "scripted unconfined fixture mutation".into(),
            input_schema: json!({"type":"object","properties":{"path":{"const":"changed.txt"}},"required":["path"],"additionalProperties":false}),
        },
        id("registry-mutation-v1"),
        ToolConcurrency::Serial,
        ToolRecoveryPolicy::NeverRepeat,
        EgressRealm::Local,
    ).unwrap();
    binding.start_receipts = StartReceiptCapability::Authoritative;
    binding
}

struct RegistryTool {
    host: PathBuf,
    starts: AtomicUsize,
    reconciles: AtomicUsize,
}

impl RegistryTool {
    fn new(host: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            host,
            starts: AtomicUsize::new(0),
            reconciles: AtomicUsize::new(0),
        })
    }
}

fn claim_key(execution: &ToolExecution) -> ClaimKey {
    ClaimKey {
        session: execution.session,
        invocation: execution.invocation,
        attempt: execution.attempt,
    }
}

impl ToolBoundary for RegistryTool {
    fn binding(&self) -> ToolBinding {
        mutation_binding()
    }
    fn executor(&self) -> SemanticCompatibilityId {
        id("script-exec-v1")
    }
    fn prepare(&self, arguments: serde_json::Value) -> Result<PreparedAction, ToolBoundaryError> {
        Ok(PreparedAction::new(
            mutation_binding().id,
            arguments,
            EgressRealm::Local,
            ToolAuthority::UnconfinedExecution,
            Some(0),
            vec![],
        )
        .unwrap())
    }
    fn live_authority(&self, _: &PreparedAction, _: &WorkspaceBinding) -> LiveToolAuthority {
        LiveToolAuthority::Allow
    }
    fn execute<'a>(
        &'a self,
        execution: ToolExecution,
        stop: CancellationToken,
    ) -> BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move {
            assert!(!stop.is_cancelled());
            assert!(execution.ceiling.workspace_mutation);
            self.starts.fetch_add(1, Ordering::SeqCst);
            let key = claim_key(&execution);
            let mut registry = WorkspaceRegistry::open(&self.host).unwrap();
            registry
                .admit(
                    &execution.workspace,
                    key,
                    WorkspaceResources::Files,
                    WorkspaceRevision {
                        files: execution.action.workspace_revision.unwrap(),
                        repository: 0,
                    },
                )
                .unwrap();
            registry
                .record_start(
                    key,
                    RegistryReceipt {
                        backend: execution.workspace.backend.clone(),
                        identity: execution.attempt.to_string(),
                    },
                )
                .unwrap();
            let relative = execution.action.arguments["path"].as_str().unwrap();
            assert_eq!(relative, "changed.txt");
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(Path::new(&execution.workspace.canonical_root).join(relative))
                .unwrap();
            output.write_all(b"one physical mutation").unwrap();
            output.sync_all().unwrap();
            // This trusted fixture's terminal ledger records successful synchronous
            // completion outside Session state. The ready record is written only
            // after the effect and host start claim are durable.
            let mut evidence = fs::File::create(self.host.join("terminal.json")).unwrap();
            evidence
                .write_all(&serde_json::to_vec(&key).unwrap())
                .unwrap();
            evidence.sync_all().unwrap();
            // Kill here: no ToolAttempt evidence has returned to Session SQLite.
            std::future::pending().await
        })
    }
    fn reconcile<'a>(
        &'a self,
        execution: ToolExecution,
        _: ToolAttempt,
    ) -> BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move {
            self.reconciles.fetch_add(1, Ordering::SeqCst);
            let key = claim_key(&execution);
            let terminal: ClaimKey =
                serde_json::from_slice(&fs::read(self.host.join("terminal.json")).unwrap())
                    .unwrap();
            assert_eq!(terminal, key);
            let mut registry = WorkspaceRegistry::open(&self.host).unwrap();
            let claim = registry.claim(key).unwrap();
            assert_eq!(claim.binding, execution.workspace);
            let start = claim
                .start
                .expect("host start receipt survives process loss");
            let effect = EffectSummary::KnownChanges {
                paths: vec!["changed.txt".into()],
            };
            registry
                .resolve(
                    key,
                    TerminalEvidence {
                        receipt: start.clone(),
                        effect: effect.clone(),
                    },
                )
                .unwrap();
            ToolAttemptState::Settled {
                result: ToolResult {
                    value: json!("mutation completed"),
                    is_error: false,
                    capture: OutputCapture::CompleteInline,
                },
                effect,
                retryable: false,
                receipt: Some(StartReceipt {
                    kind: "registry-mutation-v1".into(),
                    data: serde_json::to_value(start).unwrap(),
                }),
            }
        })
    }
}

#[tokio::test]
async fn owner_child() {
    let Some(home) = std::env::var_os("ION_TOOL_PROCESS_LOSS_CHILD") else {
        return;
    };
    let home = PathBuf::from(home);
    let mut registry = WorkspaceRegistry::open(home.join("host")).unwrap();
    let workspace = registry
        .bind("workspace", home.join("checkout"), "script-exec-v1")
        .unwrap();
    let mut cfg = config();
    cfg.workspace = workspace;
    cfg.authority.workspace_mutation = true;
    cfg.authority.unconfined_execution = true;
    cfg.tools = vec![mutation_binding()];
    cfg.initial_tools = vec![mutation_binding().id];
    let session = Session::create(home.join("session.sqlite"), cfg)
        .await
        .unwrap()
        .session;
    let h = session.handle();
    let conversation = session.primary_conversation();
    let input = h
        .admit_input(
            conversation,
            AdmitInputRequest {
                sender: InputSender::User,
                mode: InputMode::Submit,
                request_key: None,
                body: InputBody::Text("mutate once".into()),
            },
        )
        .await
        .unwrap();
    let turn = h
        .start_turn(StartTurnRequest {
            conversation,
            input: input.input().id,
            admitted_at_unix_ms: 0,
            wall_deadline_unix_ms: None,
        })
        .await
        .unwrap()
        .turn
        .id;
    let provider = Arc::new(Model {
        starts: AtomicUsize::new(0),
        arguments: json!({"path":"changed.txt"}),
        calls: 1,
    });
    let backend = RegistryTool::new(home.join("host"));
    let tools = ToolBoundaries::new([backend as Arc<dyn ToolBoundary>]).unwrap();
    let _ = h
        .resume_with_tools(turn, models(&provider), tools, DrivePolicy::default())
        .await;
    panic!("parent must kill the owner before evidence returns");
}

#[tokio::test]
async fn killed_tool_owner_reconciles_registry_receipt_without_reexecution() {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let home = std::env::temp_dir().join(format!("ion-tool-process-loss-{}", SessionId::new()));
    fs::create_dir_all(home.join("checkout")).unwrap();
    let mut child = Child(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "process_loss::owner_child", "--nocapture"])
            .env("ION_TOOL_PROCESS_LOSS_CHILD", &home)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if fs::read(home.join("host/terminal.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<ClaimKey>(&bytes).ok())
                .is_some()
            {
                break;
            }
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "owner exited before mutation"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("external mutation deadline");
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    let registry = WorkspaceRegistry::open(home.join("host")).unwrap();
    let claims = registry.unresolved(None, 4).unwrap();
    assert_eq!(claims.len(), 1);
    assert_eq!(
        fs::read(home.join("checkout/changed.txt")).unwrap(),
        b"one physical mutation"
    );

    let session = Session::open(home.join("session.sqlite")).await.unwrap();
    let step = tool_step(&session).await;
    let before = session.handle().tool_records(step).await.unwrap();
    assert_eq!(before.attempts.len(), 1);
    assert!(matches!(
        before.attempts[0].state,
        ToolAttemptState::IntentCommitted {
            start_receipt: None
        }
    ));
    assert_eq!(
        registry.unresolved(None, 4).unwrap(),
        claims,
        "passive open leaves quarantine untouched"
    );
    let backend = RegistryTool::new(home.join("host"));
    let tools = ToolBoundaries::new([backend.clone() as Arc<dyn ToolBoundary>]).unwrap();
    assert!(matches!(
        session
            .handle()
            .resume_with_tools(
                before.turn.id,
                models(&model()),
                tools,
                DrivePolicy::default()
            )
            .await
            .unwrap(),
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    let after = session.handle().tool_records(step).await.unwrap();
    assert_eq!(after.attempts.len(), 1);
    assert_eq!(after.attempts[0].id, before.attempts[0].id);
    assert!(matches!(
        after.attempts[0].state,
        ToolAttemptState::Settled {
            receipt: Some(_),
            ..
        }
    ));
    assert_eq!(backend.starts.load(Ordering::SeqCst), 0);
    assert_eq!(backend.reconciles.load(Ordering::SeqCst), 1);
    assert!(registry.unresolved(None, 4).unwrap().is_empty());
    assert_eq!(
        registry
            .revision(&before.turn.environment.workspace)
            .unwrap()
            .files,
        1
    );
    session.close().await.unwrap();
    drop(registry);
    fs::remove_dir_all(home).unwrap();
}
