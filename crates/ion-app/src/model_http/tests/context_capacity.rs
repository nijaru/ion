use super::*;
use ion_core::{
    Agent, AgentLimits, Session, ToolDefinition, ToolExecutor, ToolOutput, ToolRegistration,
    ToolSource,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

fn changing_context() -> ModelRequest {
    let mut request = request();
    request.route.effective = ModelRef {
        provider: "anthropic".into(),
        model: "claude-opus-5-5".into(),
    };
    request.tools = vec![ToolSpec {
        name: "probe".into(),
        description: "current".into(),
        input_schema: json!({"type":"object"}),
    }];
    request.context_timeline = Some(ModelContextTimeline {
        initial: ModelContextState {
            instructions: request.instructions.clone(),
            tools: vec![ToolSpec {
                description: "historical schema ".repeat(2_000),
                ..request.tools[0].clone()
            }],
        },
        changes: vec![ModelContextChange {
            after_message: 1,
            context: ModelContextState {
                instructions: request.instructions.clone(),
                tools: request.tools.clone(),
            },
        }],
    });
    request
}

#[test]
fn native_context_overhead_matches_wire_delta_and_dispatch_eligibility() {
    for instructions in [None, Some("rules".into())] {
        for empty_tools in [false, true] {
            let mut request = changing_context();
            request.instructions = instructions.clone();
            if empty_tools {
                request.tools.clear();
            }
            let timeline = request.context_timeline.as_mut().unwrap();
            timeline.initial.instructions = instructions.clone();
            timeline.changes[0].context.instructions = instructions.clone();
            timeline.changes[0].context.tools = request.tools.clone();
            let actual =
                serde_json::to_vec(&anthropic_body_for_route(&request, true, true).unwrap())
                    .unwrap()
                    .len();
            let mut latest = request.clone();
            latest.context_timeline = None;
            let baseline =
                serde_json::to_vec(&anthropic_body_for_route(&latest, true, true).unwrap())
                    .unwrap()
                    .len();
            let expected = actual.saturating_sub(baseline);
            assert!(expected > 30_000);
            for (wire, endpoint, inline, enabled) in [
                (
                    HttpWire::AnthropicMessages,
                    "https://api.anthropic.com/v1/messages",
                    true,
                    true,
                ),
                (
                    HttpWire::AnthropicMessages,
                    "https://api.anthropic.com/v1/messages",
                    false,
                    false,
                ),
                (
                    HttpWire::AnthropicMessages,
                    "http://127.0.0.1:1/v1/messages",
                    true,
                    false,
                ),
                (
                    HttpWire::ChatCompletions,
                    "https://api.anthropic.com/chat/completions",
                    true,
                    false,
                ),
                (
                    HttpWire::OpenAiResponses,
                    "https://api.anthropic.com/responses",
                    true,
                    false,
                ),
            ] {
                let mut capabilities = ModelCapabilities::conservative();
                capabilities.context_mutation.inline_tool_definitions = inline;
                let credentials = Arc::new(|| -> Option<String> {
                    panic!("context measurement must not resolve credentials")
                });
                let service = HttpModelService::new_with_capabilities(
                    endpoint,
                    wire,
                    credentials,
                    capabilities,
                    None,
                )
                .unwrap();
                assert_eq!(
                    service.context_encoding_overhead(&request).unwrap(),
                    if enabled { expected } else { 0 }
                );
            }
        }
    }
}

#[test]
fn context_measurement_leaves_signed_replay_validation_at_issuance() {
    let mut request = changing_context();
    request.messages.extend([
        Message {
            role: Role::Assistant,
            content: vec![Content::Text("old".into())],
            provider_replay: Some(ProviderReplay::new("anthropic", ANTHROPIC_CONTENT_REPLAY, json!({
                "blocks":[{"type":"thinking","thinking":"","signature":"fixture"},{"type":"text","text":"old"}],
                "prefix_sha256":"0".repeat(64)
            })).with_prefix_binding(true)),
        },
        Message::user_input("continue".into(), []),
    ]);
    request.context_timeline.as_mut().unwrap().changes[0].after_message = 3;
    assert!(anthropic_context_encoding_overhead(&request, true, true).unwrap() > 0);
    assert_eq!(
        anthropic_body_for_route(&request, true, true)
            .unwrap_err()
            .kind,
        ProviderErrorKind::ReplayContextChanged
    );
}

struct NativeBoundary {
    bodies: Mutex<Vec<Value>>,
}

impl ModelService for NativeBoundary {
    fn context_encoding_overhead(&self, request: &ModelRequest) -> Result<usize, ProviderError> {
        anthropic_context_encoding_overhead(request, true, true)
    }

    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            let body = anthropic_body_for_route(&request, true, true)?;
            let prefix = anthropic_body_prefix_digest(&body)?;
            self.bodies.lock().unwrap().push(body);
            let response = ModelResponse {
                message: Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("done".into())],
                    provider_replay: Some(ProviderReplay::new("anthropic", ANTHROPIC_CONTENT_REPLAY, json!({
                        "blocks":[
                            {"type":"thinking","thinking":"","signature":"fixture-signature"},
                            {"type":"text","text":"done"}
                        ],
                        "prefix_sha256":prefix
                    })).with_prefix_binding(true)),
                },
                usage: Usage::unknown(),
                termination: ion_ai::ResponseTermination::Completed,
                returned_model: None,
            };
            let stream: ModelStream = Box::pin(futures_util::stream::iter([Ok(
                ModelStreamEvent::Completed(response),
            )]));
            Ok(stream)
        })
    }
}

struct Schema(String);
impl ToolSource for Schema {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "probe".into(),
                description: self.0.clone(),
                input_schema: json!({"type":"object"}),
            }),
            self,
        )]
    }
}
impl ToolExecutor for Schema {
    fn execute<'a>(&'a self, _: ion_core::ToolInvocation<'a>) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async { unreachable!("the capacity fixture never asks for tool execution") })
    }
}

#[tokio::test]
async fn historical_native_tool_definitions_are_budgeted_before_issuance() {
    let root = std::env::temp_dir().join(format!(
        "ion-native-context-capacity-{}",
        uuid::Uuid::now_v7()
    ));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("session.sqlite");
    let session = Arc::new(Session::create(&path, Some(&root)).unwrap());
    let service = Arc::new(NativeBoundary {
        bodies: Mutex::new(Vec::new()),
    });
    let model = ModelRef {
        provider: "anthropic".into(),
        model: "claude-opus-5-5".into(),
    };
    let original = Agent::new(
        service.clone(),
        Arc::new(Schema("original schema documentation ".repeat(4_000))),
        model.clone(),
    );
    original
        .submit(
            session.clone(),
            "first".into(),
            "coding fixture".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    drop((original, session));

    // Reopen cold with a much smaller current definition. Native inline
    // mutation still sends the original definition to preserve signed replay.
    let reopened = Arc::new(Session::open(&path).unwrap());
    let byte_limit = 20_000;
    let current = Agent::new(
        service.clone(),
        Arc::new(Schema("small current definition".into())),
        model,
    )
    .with_limits(AgentLimits {
        max_request_bytes: byte_limit,
        ..AgentLimits::default()
    });
    current
        .submit(
            reopened.clone(),
            "continue".into(),
            "coding fixture".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    let bodies = service.bodies.lock().unwrap();
    assert!(serde_json::to_vec(&bodies[0]).unwrap().len() > byte_limit);
    for body in &bodies[1..] {
        let bytes = serde_json::to_vec(body).unwrap().len();
        assert!(
            bytes <= byte_limit,
            "latest-only planning admitted {bytes} wire bytes despite a {byte_limit}-byte budget"
        );
    }
    assert!(
        reopened.view().unwrap().compacted_through.is_some(),
        "historical definitions must be removed by bounded compaction before the next ordinary request"
    );
    drop(bodies);
    drop((current, reopened));
    std::fs::remove_dir_all(root).unwrap();
}
