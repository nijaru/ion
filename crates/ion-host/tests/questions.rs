//! Live typed-question custody through the real Agent Turn and tool settlement.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::Result;
use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination,
    Role, Script, ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    Agent, ChildOutcome, CodeLimits, Session, SessionEntry, ToolDefinition, ToolExecutor,
    ToolInvocation, ToolOutput, ToolSet, ToolSource,
};
use ion_host::{
    Application, ApplicationRuntime, ControlUpdate, Delivery, PendingQuestion, QuestionAnswer,
    QuestionCustody, QuestionTools, Resources, SessionCatalog, SessionSetup, Settlement, UserInput,
    code_mode::QuickJs,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn model() -> ModelRef {
    ModelRef {
        provider: "fixture".into(),
        model: "fixture".into(),
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
        returned_model: None,
    })])
}

struct Effect {
    calls: Arc<AtomicUsize>,
    root: PathBuf,
}

impl ToolSource for Effect {
    fn registrations(self: Arc<Self>) -> Vec<ion_core::ToolRegistration> {
        vec![ion_core::ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "effect".into(),
                description: "Record one dependent effect".into(),
                input_schema: json!({
                    "type":"object",
                    "additionalProperties":false,
                    "required":["value"],
                    "properties":{"value":{"type":"string"}}
                }),
            }),
            self,
        )]
    }
}

impl ToolExecutor for Effect {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, ToolOutput> {
        let value = invocation.call().arguments["value"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let root = self.root.clone();
        let calls = self.calls.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            fs::write(root.join("effect.txt"), &value).unwrap();
            ToolOutput {
                value: json!({"effect": value}),
                images: Vec::new(),
                is_error: false,
            }
        })
    }
}

struct App {
    service: Arc<ScriptedModelService>,
    effect: Arc<Effect>,
}

impl Application for App {
    fn resources(&self, _: Option<&Path>) -> Result<Resources> {
        Ok(Resources::new("Question fixture".into()))
    }

    fn runtime(
        &self,
        _: Option<&Path>,
        _: Option<&ModelRef>,
        questions: Option<Arc<QuestionCustody>>,
    ) -> Result<ApplicationRuntime> {
        let mut sources: Vec<Arc<dyn ToolSource>> = vec![self.effect.clone()];
        if let Some(questions) = questions {
            sources.push(Arc::new(QuestionTools::new(questions)));
        }
        let tools = ToolSet::new(sources).with_code_mode(Arc::new(QuickJs), CodeLimits::default());
        let agent = Agent::with_tool_set(self.service.clone(), Arc::new(tools), model());
        Ok(ApplicationRuntime::new(Arc::new(agent)))
    }
}

struct Fixture {
    control: ion_host::SessionControl<()>,
    responder: Option<ion_host::QuestionResponder>,
    service: Arc<ScriptedModelService>,
    effect: PathBuf,
    calls: Arc<AtomicUsize>,
    root: PathBuf,
}

fn fixture(scripts: Vec<Script>, with_consumer: bool) -> Fixture {
    let root = std::env::temp_dir().join(format!("ion-question-{}", uuid::Uuid::now_v7()));
    let cwd = root.join("work");
    fs::create_dir_all(&cwd).unwrap();
    let cwd = fs::canonicalize(&cwd).unwrap();
    let catalog = SessionCatalog::new(root.join("state"), Some(cwd.clone()));
    let session = Arc::new(Session::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
    let service = Arc::new(ScriptedModelService::new(scripts));
    let calls = Arc::new(AtomicUsize::new(0));
    let custody = with_consumer.then(QuestionCustody::new);
    let app = Arc::new(App {
        service: service.clone(),
        effect: Arc::new(Effect {
            calls: calls.clone(),
            root: cwd.clone(),
        }),
    });
    let control = SessionSetup::new(session, catalog, app, None, None, custody)
        .unwrap()
        .into_control::<()>();
    let responder = control.attach_question_responder().unwrap();
    // SessionSetup retains the same custody; the app runtime already registered
    // its tool from that value during preflight.
    Fixture {
        control,
        responder,
        service,
        effect: cwd.join("effect.txt"),
        calls,
        root,
    }
}

fn question_call() -> Content {
    Content::ToolCall(ToolCall {
        id: "question".into(),
        name: "ask_user_question".into(),
        arguments: json!({
            "questions":[{
                "question":"Which route?",
                "options":[{"label":"Alpha"},{"label":"Beta"}]
            }]
        }),
        raw_arguments: None,
    })
}

fn effect_call() -> Content {
    Content::ToolCall(ToolCall {
        id: "effect".into(),
        name: "effect".into(),
        arguments: json!({"value":"chosen once"}),
        raw_arguments: None,
    })
}

fn code_mode_call(code: &str) -> Content {
    Content::ToolCall(ToolCall {
        id: "code".into(),
        name: "code_mode".into(),
        arguments: json!({"code": code}),
        raw_arguments: None,
    })
}

async fn wait_for_pending(fixture: &mut Fixture) -> PendingQuestion {
    loop {
        match fixture.control.next().await {
            ControlUpdate::QuestionChanged => {
                if let Some(pending) = fixture.responder.as_ref().unwrap().pending().unwrap() {
                    return pending;
                }
            }
            ControlUpdate::Settled(_) => panic!("question Turn settled before a pending request"),
            ControlUpdate::Idle => panic!("question Turn went idle before a pending request"),
            _ => {}
        }
    }
}

async fn wait_for_settlement(fixture: &mut Fixture) -> Settlement<()> {
    loop {
        match fixture.control.next().await {
            ControlUpdate::Settled(settlement) => return settlement,
            ControlUpdate::Idle => panic!("Turn ended without settlement"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn answer_resumes_the_same_turn_and_dispatches_dependent_effect_once() {
    let Content::ToolCall(mut call) = question_call() else {
        unreachable!("fixture constructs a tool call")
    };
    let preview = "  # Beta\n\n    preserve this indentation\n\n";
    call.arguments["questions"][0]["header"] = json!("  Routes 🦀  ");
    call.arguments["questions"][0]["options"][1]["preview"] = json!(preview);
    let mut fixture = fixture(
        vec![
            response(vec![Content::ToolCall(call)]),
            response(vec![effect_call()]),
            response(vec![Content::Text("done".into())]),
        ],
        true,
    );
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                files: Vec::new(),
                text: "ask".into(),
                images: Vec::new(),
            },
            (),
        )
        .unwrap();
    let mut pending = wait_for_pending(&mut fixture).await;
    assert_eq!(pending.origin.session(), fixture.control.session().id());
    assert!(pending.origin.turn() > 0);
    assert!(pending.origin.occurrence().assistant_entry > 0);
    let snapshot = fixture
        .control
        .snapshot(|_| ())
        .unwrap()
        .pending_question
        .unwrap();
    assert_eq!(snapshot.origin, pending.origin);
    let serialized = serde_json::to_value(&pending).unwrap();
    assert_eq!(
        serialized["questions"],
        json!([{
            "question": "Which route?",
            "header": "Routes 🦀",
            "options": [
                {"label": "Alpha", "description": null},
                {"label": "Beta", "description": null, "preview": preview}
            ],
            "multi_select": false
        }])
    );
    assert_eq!(
        serde_json::to_value(snapshot).unwrap()["questions"],
        serialized["questions"]
    );
    // A frontend's copy cannot rewrite the immutable labels used for answers.
    pending.questions[0].options[1].label = "forged label".into();
    pending.questions[0].options[1].preview = Some("forged preview".into());
    pending.questions[0].header = Some("forged header".into());
    fixture
        .responder
        .as_ref()
        .unwrap()
        .answer(pending.request, vec![QuestionAnswer::Option { index: 1 }])
        .unwrap();
    assert!(matches!(
        fixture
            .responder
            .as_ref()
            .unwrap()
            .answer(pending.request, vec![QuestionAnswer::Option { index: 0 }]),
        Err(ion_host::QuestionError::StaleRequest)
    ));
    let settlement = wait_for_settlement(&mut fixture).await;
    assert!(matches!(
        settlement.outcome,
        ion_host::OperationOutcome::Turn(Ok(ref text)) if text == "done"
    ));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read_to_string(&fixture.effect).unwrap(), "chosen once");
    let requests = fixture.service.requests();
    assert_eq!(requests.len(), 3);
    let result = requests[1]
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|part| match part {
            Content::ToolResult(result) if result.call_id == "question" => Some(result),
            _ => None,
        })
        .expect("question result reaches the next model request");
    assert!(!result.is_error);
    assert!(result.images.is_empty());
    assert_eq!(
        result.result,
        json!({"status": "answered", "answers": [{
            "kind": "option", "question": 1, "option": 1, "label": "Beta"
        }]})
    );
    drop(fixture.responder);
    fixture.control.request_shutdown();
    let _ = fixture.control.shutdown().await;
    drop(fixture.control);
    fs::remove_dir_all(fixture.root).unwrap();
}

#[tokio::test]
async fn question_tool_outside_control_admission_requires_root_turn_stop() {
    let mut fixture = fixture(
        vec![
            response(vec![question_call()]),
            response(vec![Content::Text("bypassed".into())]),
        ],
        true,
    );
    let agent = fixture.control.agent().clone();
    let session = fixture.control.session().clone();
    let stop = CancellationToken::new();
    let operation = agent.submit(
        session.clone(),
        "direct".into(),
        String::new(),
        stop.clone(),
        |_| {},
    );
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), operation).await;
    if result.is_err() {
        stop.cancel();
    }
    // Join cleanup even when the bypassed operation did not settle in time.
    let cleanup = tokio::time::timeout(std::time::Duration::from_secs(5), session.shutdown())
        .await
        .expect("bypassed operation retained execution during cleanup");
    let result = result.expect("bypassed Core call did not settle");
    cleanup.unwrap();
    assert_eq!(result.unwrap(), "bypassed");
    assert!(fixture.control.pending_question().is_none());
    assert!(
        fixture
            .responder
            .as_ref()
            .unwrap()
            .pending()
            .unwrap()
            .is_none()
    );
    let requests = fixture.service.requests();
    assert_eq!(requests.len(), 2);
    let tool_result = requests[1]
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|part| match part {
            Content::ToolResult(result) if result.call_id == "question" => Some(result),
            _ => None,
        })
        .expect("question tool result committed before the next model request");
    assert!(tool_result.is_error);
    assert!(
        tool_result.result["error"]
            .as_str()
            .is_some_and(|message| message.contains("active controlled Turn"))
    );
    drop(fixture.responder.take());
    drop(fixture.control);
    fs::remove_dir_all(fixture.root).unwrap();
}

#[tokio::test]
async fn responder_loss_stops_before_dependent_dispatch() {
    let mut fixture = fixture(
        vec![
            response(vec![question_call()]),
            response(vec![effect_call()]),
            response(vec![Content::Text("must not run".into())]),
        ],
        true,
    );
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                files: Vec::new(),
                text: "ask".into(),
                images: Vec::new(),
            },
            (),
        )
        .unwrap();
    let _pending = wait_for_pending(&mut fixture).await;
    drop(fixture.responder.take());
    let settlement = wait_for_settlement(&mut fixture).await;
    match settlement.outcome {
        ion_host::OperationOutcome::Turn(Err(ion_core::AgentError::Cancelled)) => {}
        ion_host::OperationOutcome::Turn(other) => panic!("unexpected Turn outcome: {other:?}"),
        _ => panic!("unexpected non-Turn settlement"),
    }
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert!(!fixture.effect.exists());
    assert_eq!(fixture.service.requests().len(), 1);
    let _ = fixture.control.shutdown().await;
    drop(fixture.control);
    fs::remove_dir_all(fixture.root).unwrap();
}

#[tokio::test]
async fn responder_loss_in_code_mode_stops_the_root_turn_before_dependent_effects() {
    let mut fixture = fixture(
        vec![
            response(vec![code_mode_call(
                r#"
                const answer = await tools.ask_user_question({
                    questions:[{
                        question:"Which route?",
                        options:[{label:"Alpha"},{label:"Beta"}]
                    }]
                });
                await tools.effect({value:"guest after responder loss"});
                return answer;
                "#,
            )]),
            response(vec![effect_call()]),
            response(vec![Content::Text("next turn complete".into())]),
        ],
        true,
    );
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                files: Vec::new(),
                text: "ask from code mode".into(),
                images: Vec::new(),
            },
            (),
        )
        .unwrap();
    let pending = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        wait_for_pending(&mut fixture),
    )
    .await
    .expect("code-mode question did not become pending");
    assert!(
        pending.origin.composed_child().is_some(),
        "fixture must exercise a genuine Code Mode child origin"
    );
    drop(fixture.responder.take());
    let settlement = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        wait_for_settlement(&mut fixture),
    )
    .await
    .expect("responder loss did not settle the Turn");
    assert!(matches!(
        settlement.outcome,
        ion_host::OperationOutcome::Turn(Err(ion_core::AgentError::Cancelled))
    ));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert!(!fixture.effect.exists());
    assert_eq!(fixture.service.requests().len(), 1);

    let view = fixture.control.session().view().unwrap();
    let child = view
        .entries
        .iter()
        .find_map(|entry| match entry {
            SessionEntry::ChildToolResult { child, outcome, .. }
                if Some(*child) == pending.origin.composed_child() =>
            {
                Some(outcome)
            }
            _ => None,
        })
        .expect("started question child has a committed outcome");
    match child {
        ChildOutcome::Observed { output } => {
            assert_eq!(output.value["status"], "cancelled");
            assert!(output.value["reason"].is_string());
        }
        other => panic!("unexpected question child outcome: {other:?}"),
    }
    assert!(view.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::TurnEnded {
            reason: ion_core::TurnEndReason::Cancelled,
            ..
        }
    )));

    // A cancelled Turn's authority is not carried into the next operation.
    fixture.responder = Some(
        fixture
            .control
            .attach_question_responder()
            .unwrap()
            .expect("question custody remains configured"),
    );
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                files: Vec::new(),
                text: "new turn".into(),
                images: Vec::new(),
            },
            (),
        )
        .unwrap();
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        wait_for_settlement(&mut fixture),
    )
    .await
    .expect("new Turn did not settle");
    assert!(matches!(
        next.outcome,
        ion_host::OperationOutcome::Turn(Ok(ref text)) if text == "next turn complete"
    ));
    assert_eq!(fixture.service.requests().len(), 3);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read_to_string(&fixture.effect).unwrap(), "chosen once");
    fixture.control.shutdown().await;
    drop(fixture.control);
    fs::remove_dir_all(fixture.root).unwrap();
}

#[tokio::test]
async fn control_stop_before_answer_prevents_dependent_dispatch() {
    let mut fixture = fixture(
        vec![
            response(vec![question_call()]),
            response(vec![effect_call()]),
            response(vec![Content::Text("must not run".into())]),
        ],
        true,
    );
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                files: Vec::new(),
                text: "ask".into(),
                images: Vec::new(),
            },
            (),
        )
        .unwrap();
    let _pending = wait_for_pending(&mut fixture).await;
    fixture.control.request_stop();
    let settlement = wait_for_settlement(&mut fixture).await;
    assert!(matches!(
        settlement.outcome,
        ion_host::OperationOutcome::Turn(Err(ion_core::AgentError::Cancelled))
    ));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert!(!fixture.effect.exists());
    assert_eq!(fixture.service.requests().len(), 1);
    drop(fixture.responder);
    let _ = fixture.control.shutdown().await;
    drop(fixture.control);
    fs::remove_dir_all(fixture.root).unwrap();
}

#[tokio::test]
async fn replacement_rebinds_custody_and_shutdown_revokes_saved_responder() {
    let mut fixture = fixture(
        vec![
            response(vec![question_call()]),
            response(vec![Content::Text("first done".into())]),
            response(vec![question_call()]),
            response(vec![effect_call()]),
            response(vec![Content::Text("must not run".into())]),
        ],
        true,
    );
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                files: Vec::new(),
                text: "first ask".into(),
                images: Vec::new(),
            },
            (),
        )
        .unwrap();
    let first = wait_for_pending(&mut fixture).await;
    let first_session = first.origin.session();
    let old_request = first.request;
    fixture
        .responder
        .as_ref()
        .unwrap()
        .answer(old_request, vec![QuestionAnswer::Option { index: 1 }])
        .unwrap();
    let settlement = wait_for_settlement(&mut fixture).await;
    assert!(matches!(
        settlement.outcome,
        ion_host::OperationOutcome::Turn(Ok(ref text)) if text == "first done"
    ));
    assert_eq!(fixture.service.requests().len(), 2);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);

    fixture.control.select_model(model()).unwrap();
    let cloned = fixture.control.clone_session().unwrap();
    assert_eq!(fixture.control.session_id(), cloned);
    fixture.control.new_session().unwrap();
    assert_ne!(fixture.control.session_id(), cloned);
    assert_ne!(fixture.control.session().id(), first_session);
    assert!(
        fixture
            .responder
            .as_ref()
            .unwrap()
            .pending()
            .unwrap()
            .is_none()
    );

    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                files: Vec::new(),
                text: "second ask".into(),
                images: Vec::new(),
            },
            (),
        )
        .unwrap();
    let second = wait_for_pending(&mut fixture).await;
    assert_ne!(second.request, old_request);
    assert_ne!(second.origin.session(), first_session);
    assert_eq!(second.origin.session(), fixture.control.session().id());
    assert_eq!(
        fixture
            .control
            .pending_question()
            .expect("live request is published")
            .request,
        second.request
    );
    assert!(matches!(
        fixture
            .responder
            .as_ref()
            .unwrap()
            .answer(old_request, vec![QuestionAnswer::Option { index: 0 }]),
        Err(ion_host::QuestionError::StaleRequest)
    ));
    let retained = fixture
        .responder
        .as_ref()
        .unwrap()
        .pending()
        .unwrap()
        .expect("new request survives stale answer");
    assert_eq!(retained.request, second.request);
    assert_eq!(retained.origin, second.origin);

    let requests_before_shutdown = fixture.service.requests().len();
    fixture.control.request_shutdown();
    assert!(fixture.control.pending_question().is_none());
    assert!(matches!(
        fixture.control.attach_question_responder(),
        Err(ion_host::QuestionError::Closed)
    ));
    assert!(matches!(
        fixture.responder.as_ref().unwrap().pending(),
        Err(ion_host::QuestionError::ForeignResponder)
    ));
    assert!(matches!(
        fixture
            .responder
            .as_ref()
            .unwrap()
            .answer(second.request, vec![QuestionAnswer::Option { index: 0 }]),
        Err(ion_host::QuestionError::ForeignResponder)
    ));

    let report = fixture.control.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    let settlement = report
        .updates
        .into_iter()
        .find_map(|update| match update {
            ControlUpdate::Settled(settlement) => Some(settlement),
            _ => None,
        })
        .expect("pending question Turn settles during shutdown");
    assert!(matches!(
        settlement.outcome,
        ion_host::OperationOutcome::Turn(Err(ion_core::AgentError::Cancelled))
    ));
    assert!(
        fixture
            .control
            .session()
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(
                entry,
                ion_core::SessionEntry::TurnEnded {
                    reason: ion_core::TurnEndReason::Cancelled,
                    ..
                }
            ))
    );
    assert_eq!(fixture.service.requests().len(), requests_before_shutdown);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert!(!fixture.effect.exists());

    drop(fixture.responder.take());
    drop(fixture.control);
    fs::remove_dir_all(fixture.root).unwrap();
}
