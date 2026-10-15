//! Credential-free non-coding consumer of the same shared lifecycle as the CLI.
//! Run with a fresh state directory: `cargo run -p ion --example restricted -- STATE`.
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result, anyhow, ensure};
use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination,
    Role, Script, ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    CodingAgent, CodingSession, CodingToolOutput, CodingToolSource, RequestKey, ToolDefinition,
    ToolExecutor, ToolRegistration, TurnEndReason,
};
use ion_host::{
    Application, ApplicationRuntime, ControlUpdate, OperationOutcome, RequestAdmission, Resources,
    SessionCatalog, SessionSetup,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const INSTRUCTIONS: &str =
    "Count the characters using count_chars. No workspace or native tools are available.";

#[derive(Default)]
struct CharacterCounter(AtomicUsize);
impl CodingToolSource for CharacterCounter {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "count_chars".into(),
                description: "Count Unicode scalar values in text.".into(),
                input_schema: json!({"type":"object","additionalProperties":false,"required":["text"],"properties":{"text":{"type":"string"}}}),
            }),
            self,
        )]
    }
}
impl ToolExecutor for CharacterCounter {
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        _stop: CancellationToken,
    ) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            match call.arguments.get("text").and_then(|value| value.as_str()) {
                Some(text) => {
                    self.0.fetch_add(1, Ordering::SeqCst);
                    CodingToolOutput {
                        value: json!(text.chars().count()),
                        images: Vec::new(),
                        is_error: false,
                    }
                }
                None => CodingToolOutput {
                    value: json!("text must be a string"),
                    images: Vec::new(),
                    is_error: true,
                },
            }
        })
    }
}

struct CounterApplication {
    service: Arc<ScriptedModelService>,
    counter: Arc<CharacterCounter>,
}
impl Application for CounterApplication {
    fn resources(&self, cwd: Option<&Path>) -> Result<Resources> {
        ensure!(cwd.is_none(), "counter application needs no workspace");
        Ok(Resources::new(INSTRUCTIONS.into()))
    }
    fn runtime(&self, _cwd: Option<&Path>, model: Option<&ModelRef>) -> Result<ApplicationRuntime> {
        let identity = ModelRef {
            provider: "synthetic".into(),
            model: "counter".into(),
        };
        ensure!(
            model.is_none_or(|model| model == &identity),
            "unknown model"
        );
        Ok(ApplicationRuntime::new(Arc::new(CodingAgent::new(
            self.service.clone(),
            self.counter.clone(),
            identity,
        ))))
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

#[tokio::main]
async fn main() -> Result<()> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("usage: restricted STATE_DIRECTORY")?,
    );
    let service = Arc::new(ScriptedModelService::new([
        response(vec![Content::ToolCall(ToolCall {
            id: "count".into(),
            name: "count_chars".into(),
            arguments: json!({"text":"🦀e"}),
            raw_arguments: None,
        })]),
        response(vec![Content::Text("2 characters".into())]),
    ]));
    let counter = Arc::new(CharacterCounter::default());
    let application = Arc::new(CounterApplication {
        service: service.clone(),
        counter: counter.clone(),
    });
    let catalog = SessionCatalog::new(root.clone(), None);
    let session = Arc::new(CodingSession::create(catalog.new_path()?, None)?);
    let path = session.path().to_owned();
    let session_id = session.id();
    let key = RequestKey::new("counter-app".into(), "delivery-1".into())?;
    let literal = Message::user_input("Count 🦀e".into(), []);
    let mut control =
        SessionSetup::new(session, catalog, application.clone(), None, None)?.into_control::<()>();
    let result: Result<()> = async {
        let admission = control
            .submit_request(&key, literal.clone(), ())
            .map_err(|input| input.reason)?;
        ensure!(
            matches!(admission, RequestAdmission::Started { .. }),
            "first delivery must start work"
        );
        ensure!(
            matches!(
                control
                    .submit_request(&key, literal.clone(), ())
                    .map_err(|input| input.reason)?,
                RequestAdmission::Existing { .. }
            ),
            "redelivery started another operation"
        );
        loop {
            match control.next().await {
                ControlUpdate::Settled(settlement) => {
                    match settlement.outcome {
                        OperationOutcome::Turn(result) => {
                            result?;
                        }
                        _ => return Err(anyhow!("unexpected operation")),
                    }
                    break;
                }
                ControlUpdate::Returned(input) => {
                    return Err(input
                        .preparation_error
                        .unwrap_or_else(|| anyhow!("input returned: {:?}", input.reason)));
                }
                _ => {}
            }
        }
        ensure!(
            counter.0.load(Ordering::SeqCst) == 1,
            "custom tool must execute once"
        );
        let requests = service.requests();
        ensure!(requests.len() == 2, "expected a tool exchange");
        for request in requests {
            ensure!(
                request.instructions.as_deref() == Some(INSTRUCTIONS),
                "unexpected instruction inheritance"
            );
            ensure!(
                request.tools.len() == 1 && request.tools[0].name == "count_chars",
                "unexpected tool authority"
            );
        }
        ensure!(control.session().cwd().is_none(), "unexpected workspace");
        Ok(())
    }
    .await;
    let report = control.shutdown().await;
    if !report.cleanup_errors.is_empty() {
        return Err(anyhow!(
            "shared shutdown failed: {:?}; run result: {result:?}",
            report.cleanup_errors
        ));
    }
    result?;
    drop(control);
    let reopened = Arc::new(CodingSession::open(path)?);
    ensure!(reopened.id() == session_id, "logical identity changed");
    let mut control = SessionSetup::new(
        reopened,
        SessionCatalog::new(root, None),
        application,
        None,
        None,
    )?
    .into_control::<()>();
    let admission = control
        .submit_request(&key, literal, ())
        .map_err(|input| input.reason)?;
    ensure!(
        matches!(admission, RequestAdmission::Existing { receipt, .. } if receipt.session == session_id && receipt.turn == 1 && receipt.end == Some(TurnEndReason::Completed)),
        "reopen did not reconcile the completed delivery"
    );
    ensure!(
        counter.0.load(Ordering::SeqCst) == 1 && service.requests().len() == 2,
        "redelivery executed model/tool work"
    );
    ensure!(
        control.shutdown().await.cleanup_errors.is_empty(),
        "reopened shutdown failed"
    );
    println!(
        "restricted application: one custom tool effect; durable delivery reconciled after reopen; no coding defaults or workspace"
    );
    Ok(())
}
