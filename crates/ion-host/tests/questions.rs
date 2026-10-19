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
    CodingAgent, CodingSession, CodingToolOutput, CodingToolSource, ToolDefinition, ToolExecutor,
    ToolInvocation, ToolSet,
};
use ion_host::{
    Application, ApplicationRuntime, ControlUpdate, Delivery, QuestionAnswer, QuestionCustody,
    QuestionTools, Resources, SessionCatalog, SessionSetup, UserInput,
};
use serde_json::json;

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

impl CodingToolSource for Effect {
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
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, CodingToolOutput> {
        let value = invocation.call().arguments["value"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let root = self.root.clone();
        let calls = self.calls.clone();
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            fs::write(root.join("effect.txt"), &value).unwrap();
            CodingToolOutput {
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
        let mut sources: Vec<Arc<dyn CodingToolSource>> = vec![self.effect.clone()];
        if let Some(questions) = questions {
            sources.push(Arc::new(QuestionTools::new(questions)));
        }
        let agent = CodingAgent::with_tool_set(
            self.service.clone(),
            Arc::new(ToolSet::new(sources)),
            model(),
        );
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
    let session = Arc::new(CodingSession::create(catalog.new_path().unwrap(), Some(&cwd)).unwrap());
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

#[tokio::test]
async fn answer_resumes_the_same_turn_and_dispatches_dependent_effect_once() {
    let mut fixture = fixture(
        vec![
            response(vec![question_call()]),
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
                text: "ask".into(),
                images: Vec::new(),
            },
            (),
        )
        .unwrap();
    let mut answered = false;
    let settlement = loop {
        match fixture.control.next().await {
            ControlUpdate::QuestionChanged => {
                let pending = fixture.responder.as_ref().unwrap().pending().unwrap();
                if let Some(pending) = pending {
                    assert_eq!(pending.origin.session(), fixture.control.session().id());
                    assert!(pending.origin.turn() > 0);
                    assert!(pending.origin.occurrence().assistant_entry > 0);
                    assert_eq!(
                        fixture
                            .control
                            .snapshot(|_| ())
                            .unwrap()
                            .pending_question
                            .unwrap()
                            .origin,
                        pending.origin
                    );
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
                    answered = true;
                }
            }
            ControlUpdate::Settled(settlement) => break settlement,
            ControlUpdate::Idle => panic!("question Turn ended without settlement"),
            _ => {}
        }
    };
    assert!(matches!(
        settlement.outcome,
        ion_host::OperationOutcome::Turn(Ok(ref text)) if text == "done"
    ));
    assert!(answered);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fs::read_to_string(&fixture.effect).unwrap(), "chosen once");
    let requests = fixture.service.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        serde_json::to_string(&requests[1].messages)
            .unwrap()
            .contains("\"label\":\"Beta\"")
    );
    drop(fixture.responder);
    fixture.control.request_shutdown();
    let _ = fixture.control.shutdown().await;
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
                text: "ask".into(),
                images: Vec::new(),
            },
            (),
        )
        .unwrap();
    loop {
        match fixture.control.next().await {
            ControlUpdate::QuestionChanged => break,
            ControlUpdate::Settled(_) => panic!("question was not pending"),
            _ => {}
        }
    }
    drop(fixture.responder.take());
    let settlement = loop {
        match fixture.control.next().await {
            ControlUpdate::Settled(settlement) => break settlement,
            ControlUpdate::Idle => panic!("turn did not settle after consumer loss"),
            _ => {}
        }
    };
    match settlement.outcome {
        ion_host::OperationOutcome::Turn(Err(ion_core::CodingAgentError::Cancelled)) => {}
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
                text: "ask".into(),
                images: Vec::new(),
            },
            (),
        )
        .unwrap();
    loop {
        match fixture.control.next().await {
            ControlUpdate::QuestionChanged => {
                if fixture
                    .responder
                    .as_ref()
                    .unwrap()
                    .pending()
                    .unwrap()
                    .is_some()
                {
                    break;
                }
            }
            ControlUpdate::Settled(_) => panic!("question was not pending"),
            _ => {}
        }
    }
    fixture.control.request_stop();
    let settlement = loop {
        match fixture.control.next().await {
            ControlUpdate::Settled(settlement) => break settlement,
            ControlUpdate::Idle => panic!("stopped turn did not settle"),
            _ => {}
        }
    };
    assert!(matches!(
        settlement.outcome,
        ion_host::OperationOutcome::Turn(Err(ion_core::CodingAgentError::Cancelled))
    ));
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert!(!fixture.effect.exists());
    assert_eq!(fixture.service.requests().len(), 1);
    drop(fixture.responder);
    let _ = fixture.control.shutdown().await;
    drop(fixture.control);
    fs::remove_dir_all(fixture.root).unwrap();
}
