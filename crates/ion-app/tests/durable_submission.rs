//! Real process loss at the observation boundary of an independently owned HTTP effect.
use anyhow::{Result, ensure};
use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination,
    Role, Script, ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_app::{
    Application, ApplicationRuntime, ControlUpdate, Delivery, OperationOutcome, RequestAdmission,
    Resources, SessionCatalog, SessionSetup, UserInput,
};
use ion_core::{
    Agent, AuthorityScope, RequestKey, Session, SessionEntry, ToolDefinition, ToolExecutor,
    ToolOutcome, ToolOutput, ToolRegistration, ToolSource, TurnEndReason,
};
use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[path = "durable_submission/wake.rs"]
mod wake;

const REMOTE_RESULT: &str = "remote effect accepted";

struct Effect {
    endpoint: String,
    client: reqwest::Client,
    ready: tokio::sync::Semaphore,
}
impl ToolSource for Effect {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "effect".into(),
                description: "Submit one independently owned HTTP mutation.".into(),
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
    ) -> BoxFuture<'a, ToolOutput> {
        let stop = invocation.stop().clone();
        Box::pin(async move {
            let _dispatch = self.ready.acquire().await.unwrap();
            tokio::select! {
                () = stop.cancelled() => ToolOutput {
                    value: serde_json::json!("effect wait stopped"),
                    artifacts: Vec::new(),
                    images: Vec::new(),
                    is_error: true,
                },
                result = async {
                    self.client.post(&self.endpoint).send().await?.error_for_status()?.text().await
                } => ToolOutput::success(serde_json::json!(result.expect("loopback effect response"))),
            }
        })
    }
}
struct FixtureService {
    inner: ScriptedModelService,
    calls: AtomicUsize,
    hold_at: Option<usize>,
    provisional_stream: bool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
impl FixtureService {
    fn requests(&self) -> Vec<ion_ai::ModelRequest> {
        self.inner.requests()
    }
}
impl ion_ai::ModelService for FixtureService {
    fn validate_access(&self) -> BoxFuture<'_, Result<(), ion_ai::ProviderError>> {
        ion_ai::ModelService::validate_access(&self.inner)
    }

    fn stream<'a>(
        &'a self,
        request: ion_ai::ModelRequest,
    ) -> BoxFuture<'a, Result<ion_ai::ModelStream, ion_ai::ProviderError>> {
        Box::pin(async move {
            let index = self.calls.fetch_add(1, Ordering::Relaxed);
            if index == 0 && self.provisional_stream {
                use futures_util::{StreamExt, stream};
                let delta = Ok(ModelStreamEvent::TextDelta {
                    block: 0,
                    text: "uncommitted draft".into(),
                });
                return Ok(
                    Box::pin(stream::iter([delta]).chain(stream::pending())) as ion_ai::ModelStream
                );
            }
            if self.hold_at == Some(index) {
                if index == 1 {
                    println!("AFTER_OBSERVATION");
                }
                self.entered.notify_one();
                self.release.acquire().await.unwrap().forget();
            }
            ion_ai::ModelService::stream(&self.inner, request).await
        })
    }
}
struct Fixture {
    service: Arc<FixtureService>,
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
        _: Option<Arc<ion_app::QuestionCustody>>,
    ) -> Result<ApplicationRuntime> {
        Ok(ApplicationRuntime::new(Arc::new(
            Agent::new(
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

async fn worker(root: &Path, mode: &str, endpoint: String, observed: bool) {
    if mode.starts_with("wake-") {
        return wake::worker(root, mode, endpoint).await;
    }
    let path = root.join("session.db");
    let session = Arc::new(if mode == "effect" {
        Session::create(&path, None).unwrap()
    } else {
        Session::open(&path).unwrap()
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
    } else if mode == "resume" {
        // Steering is deliberately admitted after the first request freezes.
        // Its answer therefore needs a second request, in the same Turn.
        vec![
            response(vec![Content::Text("resumed work".into())]),
            response(vec![Content::Text("steering addressed".into())]),
        ]
    } else {
        vec![response(vec![Content::Text("fresh work".into())])]
    };
    let service = Arc::new(FixtureService {
        inner: ScriptedModelService::new(scripts),
        calls: AtomicUsize::new(0),
        provisional_stream: false,
        hold_at: match mode {
            "resume" | "cancel" => Some(0),
            "effect" => Some(1), // freeze after Core commits the HTTP observation
            _ => None,
        },
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
    });
    let effect = Arc::new(Effect {
        endpoint,
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
        ready: tokio::sync::Semaphore::new(usize::from(mode != "effect")),
    });
    let application = Arc::new(Fixture {
        service: service.clone(),
        effect: effect.clone(),
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
            .await
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
            .await
            .unwrap()
        else {
            panic!("duplicate dispatch")
        };
        assert_eq!(metadata, "busy retry");
        // Crash only after the busy reconciliation assertions have run.
        effect.ready.add_permits(1);
    } else {
        let before = session.view().unwrap().entries;
        let RequestAdmission::Existing { receipt, metadata } = control
            .submit_request(&key, message(), "redelivery".into())
            .await
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
            .await
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
                    .await
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
                        .await
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
                .await
                .unwrap_err();
            assert_eq!(rejected.metadata, "stale");
            assert_eq!(rejected.kind, ion_app::AdmissionFailureKind::NotAccepted);
            assert_eq!(session.view().unwrap().entries, before);
            let resumed = control
                .resume_turn(saved.point, "continuation".into())
                .await
                .unwrap();
            if matches!(mode, "resume" | "cancel") {
                tokio::time::timeout(Duration::from_secs(5), service.entered.notified())
                    .await
                    .unwrap();
            }
            let snapshot = control.snapshot(String::clone).await.unwrap();
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
                    .await
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
                service.release.add_permits(1);
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
                    .await
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
                        assert!(matches!(result, Err(ion_core::AgentError::Cancelled)));
                    } else if mode == "prefix" {
                        assert!(matches!(
                            result,
                            Err(ion_core::AgentError::ReplayContextChanged)
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
                    .any(
                        |part| matches!(part, Content::ToolResult(result) if if observed {
                            !result.is_error && result.result == REMOTE_RESULT
                        } else {
                            result.is_error
                        })
                    )
            );
        }
    }
    let outcomes = view
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ToolResult { result, .. } => Some(&result.outcome),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        match outcomes.as_slice() {
            [ToolOutcome::Observed { output, .. }] => observed && output.value == REMOTE_RESULT,
            [ToolOutcome::Unknown] => !observed,
            _ => false,
        },
        "recovery changed or duplicated the original HTTP observation: {outcomes:?}"
    );
    assert_eq!(fs::read(root.join("effects")).unwrap(), b"effect\n");
    let requests = service.requests();
    assert_eq!(
        requests.len(),
        match mode {
            "cancel" => 0,
            "resume" => 2,
            _ => 1,
        }
    );
    if mode == "resume" {
        assert!(requests[1].messages.iter().any(|message| {
            message.content.iter().any(
                |part| matches!(part, Content::Text(text) if text == "steering after recovery"),
            )
        }));
    }
    let report = control.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    // Closed execution still permits reconciliation, not a fresh admission.
    assert!(matches!(
        control
            .submit_request(&key, message(), "closed retry".into())
            .await
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
            .await
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
fn child(root: &Path, mode: &str, http: &HttpEffect, observed: bool) -> OwnedChild {
    OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "durable_submission_survives_process_loss",
                "--nocapture",
            ])
            .env("ION_SUBMISSION_FAULT_ROOT", root)
            .env("ION_SUBMISSION_FAULT_MODE", mode)
            .env("ION_SUBMISSION_FAULT_ENDPOINT", &http.endpoint)
            .env(
                "ION_SUBMISSION_FAULT_OBSERVED",
                if observed { "1" } else { "0" },
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    )
}
/// The effect owner lives outside both Session workers. It logs every POST,
/// including a replay, and never deduplicates a request on the agent's behalf.
struct HttpEffect {
    endpoint: String,
    effect: mpsc::Receiver<()>,
    stop: mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl HttpEffect {
    fn start(root: &Path, observed: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/effect", listener.local_addr().unwrap());
        let (notify, effect) = mpsc::channel();
        let (stop, shutdown) = mpsc::channel();
        let effects = root.join("effects");
        let thread = std::thread::spawn(move || {
            let mut first = true;
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        read_post(&mut stream);
                        let mut file = OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&effects)
                            .unwrap();
                        file.write_all(b"effect\n").unwrap();
                        file.sync_all().unwrap();
                        notify.send(()).unwrap();
                        if first && !observed {
                            // No response bytes before the process dies. Do not
                            // hold the listener during the continuation attempt.
                            let mut byte = [0];
                            let _ = stream.read(&mut byte);
                        } else {
                            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{REMOTE_RESULT}", REMOTE_RESULT.len()).unwrap();
                        }
                        first = false;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if !matches!(
                            shutdown.recv_timeout(Duration::from_millis(10)),
                            Err(mpsc::RecvTimeoutError::Timeout)
                        ) {
                            break;
                        }
                    }
                    Err(error) => panic!("effect listener: {error}"),
                }
            }
        });
        Self {
            endpoint,
            effect,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for HttpEffect {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        self.thread.take().unwrap().join().unwrap();
    }
}

fn read_post(stream: &mut TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "POST /effect HTTP/1.1\r\n");
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            break;
        }
    }
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
            std::env::var("ION_SUBMISSION_FAULT_ENDPOINT").unwrap(),
            std::env::var("ION_SUBMISSION_FAULT_OBSERVED").unwrap() == "1",
        ));
        return;
    }
    let root = std::env::temp_dir().join(format!("ion-submission-fault-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    for (mode, observed) in [
        ("recover", false),
        ("resume", false),
        ("prefix", false),
        ("cancel", false),
        ("resume", true),
    ] {
        let case = root.join(format!("{mode}-{observed}"));
        fs::create_dir(&case).unwrap();
        let root = &case;
        let http = HttpEffect::start(root, observed);
        let mut first = child(root, "effect", &http, observed);
        let marker = observed.then(|| {
            let stdout = first.0.stdout.take().unwrap();
            let (tx, rx) = mpsc::channel();
            let reader = std::thread::spawn(move || {
                let found = BufReader::new(stdout)
                    .lines()
                    .any(|line| line.unwrap() == "AFTER_OBSERVATION");
                let _ = tx.send(found);
            });
            (rx, reader)
        });
        http.effect
            .recv_timeout(Duration::from_secs(15))
            .expect("effect must become independently observable");
        if let Some((rx, _)) = &marker {
            assert!(
                rx.recv_timeout(Duration::from_secs(15))
                    .expect("next provider request did not start")
            );
        }
        assert_eq!(fs::read(root.join("effects")).unwrap(), b"effect\n");
        first.0.kill().unwrap();
        assert!(!first.0.wait().unwrap().success());
        if let Some((_, reader)) = marker {
            reader.join().unwrap();
        }
        let view = Session::inspect(root.join("session.db")).unwrap();
        assert_eq!(view.unfinished_turn, Some(1));
        assert_eq!(
            view.entries
                .iter()
                .filter(|entry| matches!(entry, SessionEntry::ToolResult { .. }))
                .count(),
            usize::from(observed)
        );
        let mut second = child(root, mode, &http, observed);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = second.0.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            assert!(Instant::now() < deadline, "recovery child timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(http); // Finish draining independently issued HTTP requests before checking for replay.
        assert_eq!(fs::read(root.join("effects")).unwrap(), b"effect\n");
    }
    wake::qualify(&root);
    fs::remove_dir_all(root).unwrap();
}
