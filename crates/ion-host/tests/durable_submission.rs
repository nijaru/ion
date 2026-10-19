//! Real process loss after a host effect but before its observation is committed.
use anyhow::{Result, ensure};
use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination,
    Role, Script, ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    AuthorityScope, CodingAgent, CodingSession, CodingToolOutput, CodingToolSource, RequestKey,
    SessionEntry, ToolDefinition, ToolExecutor, ToolOutcome, ToolRegistration, TurnEndReason,
};
use ion_host::{
    Application, ApplicationRuntime, ControlUpdate, Delivery, OperationOutcome, RequestAdmission,
    Resources, SessionCatalog, SessionSetup, UserInput,
};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

struct Effect(PathBuf);
impl CodingToolSource for Effect {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "effect".into(),
                description: "Append one host effect.".into(),
                input_schema: serde_json::json!({"type":"object","properties":{}}),
            }),
            self,
        )]
    }
}
impl ToolExecutor for Effect {
    fn execute<'a>(
        &'a self,
        invocation: ion_core::ToolInvocation<'a>,
    ) -> BoxFuture<'a, CodingToolOutput> {
        let stop = invocation.stop().clone();
        Box::pin(async move {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.0)
                .unwrap();
            file.write_all(b"effect\n").unwrap();
            file.sync_all().unwrap();
            println!("EFFECT_OBSERVED");
            // The fixture is killed here: the real effect is independently durable,
            // but no ToolOutput has reached Core. There is no replay suppression.
            stop.cancelled().await;
            CodingToolOutput {
                value: serde_json::json!("effect"),
                images: Vec::new(),
                is_error: false,
            }
        })
    }
}
struct Fixture {
    service: Arc<ScriptedModelService>,
    effect: Arc<Effect>,
}
impl Application for Fixture {
    fn resources(&self, cwd: Option<&Path>) -> Result<Resources> {
        ensure!(cwd.is_none(), "fixture has no workspace");
        Ok(Resources::new("Only explicit fixture tools.".into()))
    }
    fn runtime(
        &self,
        _: Option<&Path>,
        _: Option<&ModelRef>,
        _: Option<Arc<ion_host::QuestionCustody>>,
    ) -> Result<ApplicationRuntime> {
        Ok(ApplicationRuntime::new(Arc::new(
            CodingAgent::new(
                self.service.clone(),
                self.effect.clone(),
                ModelRef {
                    provider: "fixture".into(),
                    model: "fixture".into(),
                },
            )
            .with_recovery_scope(AuthorityScope::new(
                "fixture/effect-owner/model-route/grants-v1",
            )?),
        )))
    }
}
fn response(content: Vec<Content>) -> Script {
    let replay = content
        .iter()
        .any(|part| matches!(part, Content::ToolCall(_)))
        .then(|| ion_ai::ProviderReplay {
            provider: "fixture".into(),
            kind: "signed".into(),
            data: serde_json::json!({"opaque":"fixture"}),
            prefix_bound: true,
        });
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
            provider_replay: replay,
        },
        usage: Usage::unknown(),
        termination: ResponseTermination::Completed,
        returned_model: None,
    })])
}
fn message() -> Message {
    Message::user_input("one delivery".into(), [])
}

async fn worker(root: &Path, mode: &str) {
    let path = root.join("session.db");
    let session = Arc::new(if mode == "effect" {
        CodingSession::create(&path, None).unwrap()
    } else {
        CodingSession::open(&path).unwrap()
    });
    let scripts = if mode == "effect" {
        vec![response(vec![Content::ToolCall(ToolCall {
            id: "host-effect".into(),
            name: "effect".into(),
            arguments: serde_json::json!({}),
            raw_arguments: None,
        })])]
    } else if mode == "prefix" {
        vec![Script::OpenError(ion_ai::ProviderError {
            kind: ion_ai::ProviderErrorKind::ReplayContextChanged,
            message: "changed signed prefix".into(),
            retry_after_ms: None,
        })]
    } else {
        vec![response(vec![Content::Text("fresh work".into())])]
    };
    let service = Arc::new(ScriptedModelService::new(scripts));
    let application = Arc::new(Fixture {
        service: service.clone(),
        effect: Arc::new(Effect(root.join("effects"))),
    });
    let key = RequestKey::new("authorized-inbox".into(), "delivery-1".into()).unwrap();
    let mut control = SessionSetup::new(
        session.clone(),
        SessionCatalog::new(root.to_path_buf(), None),
        application,
        None,
        None,
        None,
    )
    .unwrap()
    .into_control::<String>();
    if mode == "effect" {
        let RequestAdmission::Started { receipt, .. } = control
            .submit_request(&key, message(), "original".into())
            .unwrap()
        else {
            panic!("first admission")
        };
        assert_eq!(receipt, session.request_receipt(&key).unwrap().unwrap());
        assert_eq!(receipt.session, session.id());
        assert!(receipt.end.is_none());
        // Busy redelivery returns its own metadata, not new work or originals.
        let RequestAdmission::Existing { metadata, .. } = control
            .submit_request(&key, message(), "busy retry".into())
            .unwrap()
        else {
            panic!("duplicate dispatch")
        };
        assert_eq!(metadata, "busy retry");
    } else {
        let before = session.view().unwrap().entries;
        let RequestAdmission::Existing { receipt, metadata } = control
            .submit_request(&key, message(), "redelivery".into())
            .unwrap()
        else {
            panic!("replayed accepted work")
        };
        assert_eq!(metadata, "redelivery");
        assert_eq!(receipt.end, None);
        let conflict = control
            .submit_request(
                &key,
                Message::user_input("changed".into(), []),
                "conflict".into(),
            )
            .err()
            .unwrap();
        assert_eq!(conflict.metadata, "conflict");
        assert!(conflict.reason.to_string().contains("different input"));
        assert_eq!(
            session.view().unwrap().entries,
            before,
            "reconciliation is passive"
        );
        assert!(service.requests().is_empty());
        if mode != "recover" {
            let original = session.model_preference().unwrap().unwrap();
            session
                .select_model(ModelRef {
                    provider: "fixture".into(),
                    model: "changed".into(),
                })
                .unwrap();
            let changed = session.view().unwrap().entries;
            let point = session.recovery_state().unwrap().unwrap().point;
            assert!(
                control
                    .resume_turn(point, "selected model changed".into())
                    .is_err()
            );
            assert_eq!(session.view().unwrap().entries, changed);
            assert!(service.requests().is_empty());
            session.select_model(original).unwrap();
            let before = session.view().unwrap().entries;
            let saved = session.recovery_state().unwrap().unwrap();
            let agent = control.agent().as_ref().clone();
            let limits = ion_core::AgentLimits {
                max_output_tokens: agent.limits().max_output_tokens - 1,
                ..agent.limits()
            };
            for (candidate, instructions) in [
                (
                    agent.clone().with_recovery_scope(
                        AuthorityScope::new("changed-principal/route/grants").unwrap(),
                    ),
                    control.instructions().to_owned(),
                ),
                (agent.clone(), "changed instructions".to_owned()),
                (agent.with_limits(limits), control.instructions().to_owned()),
            ] {
                assert!(
                    candidate
                        .resume_turn::<_, ()>(
                            session.clone(),
                            saved.point,
                            instructions,
                            CancellationToken::new(),
                            None,
                            |_| {}
                        )
                        .is_err()
                );
                assert_eq!(session.view().unwrap().entries, before);
                assert!(service.requests().is_empty());
            }
            let rejected = control
                .resume_turn(
                    ion_core::RecoveryPoint {
                        checkpoint: saved.point.checkpoint - 1,
                        ..saved.point
                    },
                    "stale".into(),
                )
                .unwrap_err();
            assert_eq!(rejected.0, "stale");
            assert_eq!(session.view().unwrap().entries, before);
            let resumed = control
                .resume_turn(saved.point, "continuation".into())
                .unwrap();
            let snapshot = control.snapshot(String::clone).unwrap();
            assert_eq!(
                snapshot.operation_metadata,
                Some((resumed.operation, "continuation".into()))
            );
            assert_eq!(snapshot.state.operation.unwrap().turn, Some(1));
            assert_eq!(resumed.point.turn, 1);
            assert!(resumed.point.checkpoint > saved.point.checkpoint);
            assert!(
                control
                    .resume_turn(saved.point, "duplicate".into())
                    .is_err()
            );
            if mode == "cancel" {
                control.request_stop();
            }
            if mode == "resume" {
                control
                    .submit(
                        Delivery::Steer(resumed.operation),
                        UserInput::Message(Message::user_input(
                            "steering after recovery".into(),
                            [],
                        )),
                        "steering".into(),
                    )
                    .unwrap();
            }
        } else {
            let next = RequestKey::new("authorized-inbox".into(), "delivery-2".into()).unwrap();
            assert!(matches!(
                control
                    .submit_request(
                        &next,
                        Message::user_input("fresh authority".into(), []),
                        "new".into()
                    )
                    .unwrap(),
                RequestAdmission::Started { .. }
            ));
        }
    }
    loop {
        if let ControlUpdate::Settled(settlement) = control.next().await {
            match settlement.outcome {
                OperationOutcome::Turn(result) => {
                    if mode == "cancel" {
                        assert!(matches!(result, Err(ion_core::CodingAgentError::Cancelled)));
                    } else if mode == "prefix" {
                        assert!(matches!(
                            result,
                            Err(ion_core::CodingAgentError::ReplayContextChanged)
                        ));
                    } else {
                        result.unwrap();
                    }
                }
                _ => panic!("unexpected work"),
            }
            if mode == "resume" {
                assert_eq!(settlement.metadata.as_deref(), Some("continuation"));
                assert_eq!(settlement.accepted.len(), 1);
                assert_eq!(settlement.accepted[0].metadata, "steering");
            }
            break;
        }
    }
    let old = session.request_receipt(&key).unwrap().unwrap();
    assert_eq!(
        old.end,
        Some(if mode == "resume" {
            TurnEndReason::Completed
        } else if mode == "cancel" {
            TurnEndReason::Cancelled
        } else if mode == "prefix" {
            TurnEndReason::Failed("replay_context_changed".into())
        } else {
            TurnEndReason::Interrupted
        })
    );
    let view = session.view().unwrap();
    if mode != "recover" {
        assert!(
            !view
                .entries
                .iter()
                .any(|entry| matches!(entry, SessionEntry::ProviderReplayRebased { .. }))
        );
        assert_eq!(
            view.entries
                .iter()
                .filter(|entry| matches!(entry, SessionEntry::TurnStarted { .. }))
                .count(),
            1
        );
        assert_eq!(
            view.entries
                .iter()
                .filter(|entry| matches!(entry, SessionEntry::TurnResumed { .. }))
                .count(),
            1
        );
        if mode != "cancel" {
            let request = &service.requests()[0];
            assert_eq!(request.route.reason, ion_ai::ModelRouteReason::Recovery);
            assert!(
                request
                    .messages
                    .iter()
                    .flat_map(|message| &message.content)
                    .any(|part| matches!(part, Content::ToolResult(result) if result.is_error))
            );
        }
    }
    assert!(session.view().unwrap().entries.iter().any(|entry| matches!(entry, SessionEntry::ToolResult { result, .. } if matches!(result.outcome, ToolOutcome::Unknown))));
    assert_eq!(fs::read(root.join("effects")).unwrap(), b"effect\n");
    assert_eq!(service.requests().len(), usize::from(mode != "cancel"));
    let report = control.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    // Closed execution still permits reconciliation, not a fresh admission.
    assert!(matches!(
        control
            .submit_request(&key, message(), "closed retry".into())
            .unwrap(),
        RequestAdmission::Existing { .. }
    ));
    assert!(
        control
            .submit_request(
                &RequestKey::new("scope".into(), "new".into()).unwrap(),
                message(),
                "closed".into()
            )
            .is_err()
    );
}

struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn child(root: &Path, mode: &str) -> OwnedChild {
    OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "durable_submission_survives_process_loss",
                "--nocapture",
            ])
            .env("ION_SUBMISSION_FAULT_ROOT", root)
            .env("ION_SUBMISSION_FAULT_MODE", mode)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}
#[test]
fn durable_submission_survives_process_loss() {
    if let Some(root) = std::env::var_os("ION_SUBMISSION_FAULT_ROOT") {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(worker(
            Path::new(&root),
            &std::env::var("ION_SUBMISSION_FAULT_MODE").unwrap(),
        ));
        return;
    }
    let root = std::env::temp_dir().join(format!("ion-submission-fault-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    for mode in ["recover", "resume", "prefix", "cancel"] {
        let case = root.join(mode);
        fs::create_dir(&case).unwrap();
        let root = &case;
        let mut first = child(root, "effect");
        let stdout = first.0.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let line = line.unwrap();
                if line == "EFFECT_OBSERVED" {
                    tx.send(()).unwrap();
                    break;
                }
            }
        });
        rx.recv_timeout(Duration::from_secs(15))
            .expect("effect must become independently observable");
        assert_eq!(fs::read(root.join("effects")).unwrap(), b"effect\n");
        first.0.kill().unwrap();
        assert!(!first.0.wait().unwrap().success());
        reader.join().unwrap();
        let view = CodingSession::inspect(root.join("session.db")).unwrap();
        assert_eq!(view.unfinished_turn, Some(1));
        assert!(
            !view
                .entries
                .iter()
                .any(|entry| matches!(entry, SessionEntry::ToolResult { .. }))
        );
        let mut second = child(root, mode);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = second.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "recovery child timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fs::read(root.join("effects")).unwrap(), b"effect\n");
    }
    fs::remove_dir_all(root).unwrap();
}
