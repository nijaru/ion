use super::*;
use crate::{
    Agent, AgentLimits, Session, ToolDefinition, ToolExecutor, ToolInvocation, ToolOutput,
    ToolRegistration, ToolSet, ToolSource,
};
use ion_ai::{
    BoxFuture, Content, ImageContent, Message, ModelExecution, ModelRef, ModelResponse, ModelRoute,
    ModelRouteReason, ModelStreamEvent, ProviderError, ProviderErrorKind, Role, Script,
    ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

fn model() -> ModelRef {
    ModelRef {
        provider: "fixture".into(),
        model: "fixture".into(),
    }
}

fn named_model(name: &str) -> ModelRef {
    ModelRef {
        provider: "fixture".into(),
        model: name.into(),
    }
}

fn root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ion-request-calibration-{label}-{}",
        uuid::Uuid::now_v7()
    ))
}

fn limits() -> AgentLimits {
    AgentLimits {
        max_output_tokens: 20_000,
        context_window_tokens: Some(100_000),
        ..AgentLimits::default()
    }
}

fn configured(
    service: Arc<ScriptedModelService>,
    tools: Arc<dyn ToolSource>,
    model: ModelRef,
    limits: AgentLimits,
) -> Agent {
    Agent::new(service, tools, model).with_limits(limits)
}

fn text_response(text: &str, usage: Usage) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::Text(text.into())],
            provider_replay: None,
        },
        usage,
        termination: ion_ai::ResponseTermination::Completed,
        returned_model: None,
    })])
}

fn replay_response(text: &str, usage: Usage) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::Text(text.into())],
            provider_replay: Some(ion_ai::ProviderReplay::new(
                model().provider,
                "fixture",
                serde_json::json!({"signed": "continuation"}),
            )),
        },
        usage,
        termination: ion_ai::ResponseTermination::Completed,
        returned_model: None,
    })])
}

fn tool_response(usage: Usage) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(ToolCall {
                id: "echo-1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({}),
                raw_arguments: None,
            })],
            provider_replay: None,
        },
        usage,
        termination: ion_ai::ResponseTermination::Completed,
        returned_model: None,
    })])
}

struct EchoExecutor(usize);

impl ToolExecutor for EchoExecutor {
    fn execute<'a>(&'a self, _invocation: ToolInvocation<'a>) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async {
            ToolOutput {
                value: serde_json::json!({"echo": "y".repeat(self.0)}),
                images: Vec::new(),
                is_error: false,
            }
        })
    }
}

fn tool_registration(name: &str, output_bytes: usize) -> ToolRegistration {
    ToolRegistration::new(
        ToolDefinition::external(ToolSpec {
            name: name.into(),
            description: format!("{name} tool"),
            input_schema: serde_json::json!({"type":"object"}),
        }),
        Arc::new(EchoExecutor(output_bytes)),
    )
}

struct Echo;

impl ToolSource for Echo {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![tool_registration("echo", 800)]
    }
}

struct OtherTools;

impl ToolSource for OtherTools {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![tool_registration("other", 800)]
    }
}

fn tiny_image() -> ImageContent {
    ImageContent::from_bytes(&[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ])
    .unwrap()
}

fn gif_image() -> ImageContent {
    ImageContent::from_bytes(&[
        71, 73, 70, 56, 57, 97, 1, 0, 1, 0, 128, 0, 0, 0, 0, 0, 255, 255, 255, 44, 0, 0, 0, 0, 1,
        0, 1, 0, 0, 2, 1, 76, 0, 59,
    ])
    .unwrap()
}

#[tokio::test]
async fn committed_tool_exchange_calibrates_next_budget_from_total_input() {
    let root = root("tool");
    let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
    let service = Arc::new(ScriptedModelService::new([
        tool_response(Usage::known_with_cache(30_000, 1, 25_000, 0)),
        text_response("done", Usage::unknown()),
    ]));
    let agent = configured(
        service.clone(),
        Arc::new(Echo),
        model(),
        AgentLimits {
            max_output_tokens: 20_000,
            context_window_tokens: Some(55_000),
            ..AgentLimits::default()
        },
    );
    let prompt = format!("please use echo {}", "x".repeat(4_000));
    assert_eq!(
        agent
            .submit(
                session.clone(),
                prompt,
                "base".into(),
                CancellationToken::new(),
                |_| {}
            )
            .await
            .unwrap(),
        "done"
    );
    let requests = service.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].controls.max_output_tokens, 20_000);
    // Total input includes cache reads. Subtracting them would keep the full
    // ceiling; adding them would exceed the window and fail the turn.
    assert!(
        requests[1].controls.max_output_tokens < 20_000,
        "next request did not use the committed total input usage: {}",
        requests[1].controls.max_output_tokens
    );
    assert!(requests[1].controls.max_output_tokens > 0);
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn observed_usage_avoids_false_context_rejection() {
    struct LargeEcho;
    impl ToolSource for LargeEcho {
        fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
            vec![tool_registration("echo", 15_000)]
        }
    }

    let root = root("preserve-context");
    let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
    let service = Arc::new(ScriptedModelService::new([
        tool_response(Usage::known(10_000, 1)),
        text_response("done", Usage::unknown()),
    ]));
    let agent = configured(
        service.clone(),
        Arc::new(LargeEcho),
        model(),
        AgentLimits {
            max_output_tokens: 20_000,
            context_window_tokens: Some(30_000),
            ..AgentLimits::default()
        },
    );
    assert_eq!(
        agent
            .submit(
                session.clone(),
                format!("please use echo {}", "x".repeat(55_000)),
                "base".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap(),
        "done"
    );
    let requests = service.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| {
        message.content.iter().any(|part| {
            matches!(part, Content::ToolResult(result) if result.result["echo"].as_str().is_some_and(|text| text.len() == 15_000))
        })
    }));
    assert!(
        !session
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| { matches!(entry, crate::SessionEntry::Compacted { .. }) }),
        "compatible observed usage should preserve the full tool exchange"
    );
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn incompatible_context_or_route_clears_the_sample_after_unknown_usage() {
    for change in ["instructions", "tools", "model"] {
        let root = root(change);
        let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
        let service = Arc::new(ScriptedModelService::new([
            text_response("first", Usage::known(85_000, 1)),
            text_response("probe", Usage::unknown()),
            text_response("again", Usage::unknown()),
        ]));
        let base = configured(service.clone(), Arc::new(Echo), model(), limits());
        let prompt = format!("first {}", "x".repeat(5_000));
        base.submit(
            session.clone(),
            prompt,
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();

        let (probe, instructions) = match change {
            "instructions" => (base.clone(), "changed instructions"),
            "tools" => (
                configured(service.clone(), Arc::new(OtherTools), model(), limits()),
                "base",
            ),
            "model" => (
                configured(
                    service.clone(),
                    Arc::new(Echo),
                    named_model("other"),
                    limits(),
                ),
                "base",
            ),
            _ => unreachable!(),
        };
        probe
            .submit(
                session.clone(),
                "probe".into(),
                instructions.into(),
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap();
        // A stale sample would reduce this fallback ceiling.
        assert_eq!(
            service.requests()[1].controls.max_output_tokens,
            20_000,
            "{change}"
        );

        base.submit(
            session.clone(),
            "again".into(),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            service.requests()[2].controls.max_output_tokens,
            20_000,
            "unknown usage revived a sample from an incompatible {change} scope"
        );
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn replaced_service_does_not_share_calibration_and_unknown_usage_keeps_it_cold() {
    let root = root("service");
    let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
    let first = Arc::new(ScriptedModelService::new([
        text_response("first", Usage::known(85_000, 1)),
        text_response("again", Usage::unknown()),
    ]));
    let replacement_service = Arc::new(ScriptedModelService::new([text_response(
        "probe",
        Usage::unknown(),
    )]));
    let original = configured(first.clone(), Arc::new(Echo), model(), limits());
    original
        .submit(
            session.clone(),
            format!("first {}", "x".repeat(5_000)),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();

    let replacement = configured(
        replacement_service.clone(),
        Arc::new(Echo),
        model(),
        limits(),
    );
    replacement
        .submit(
            session.clone(),
            "probe".into(),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    // Same provider/model names are insufficient without service identity.
    // The replacement request must use the neutral fallback.
    assert_eq!(replacement_service.requests().len(), 1);
    assert_eq!(
        replacement_service.requests()[0].controls.max_output_tokens,
        20_000
    );

    original
        .submit(
            session.clone(),
            "again".into(),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        first.requests()[1].controls.max_output_tokens,
        20_000,
        "unknown usage from a replaced service revived the old sample"
    );
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn rejected_preparation_preserves_the_current_bindings_evidence() {
    let root = root("rejected-preparation");
    let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
    let service = Arc::new(ScriptedModelService::new([
        text_response("first", Usage::known(85_000, 1)),
        text_response("again", Usage::unknown()),
    ]));
    let original = configured(service.clone(), Arc::new(Echo), model(), limits());
    original
        .submit(
            session.clone(),
            format!("first {}", "x".repeat(5_000)),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    let replacement_service = Arc::new(ScriptedModelService::new([]));
    let rejected = configured(
        replacement_service.clone(),
        Arc::new(Echo),
        model(),
        AgentLimits {
            max_request_bytes: 500,
            ..limits()
        },
    );
    assert!(matches!(
        rejected
            .submit(
                session.clone(),
                "probe".into(),
                "base".into(),
                CancellationToken::new(),
                |_| {},
            )
            .await,
        Err(AgentError::ContextTooLarge)
    ));
    assert!(replacement_service.requests().is_empty());
    original
        .submit(
            session.clone(),
            "again".into(),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert!(
        service.requests()[1].controls.max_output_tokens < 20_000,
        "a rejected preparation discarded the current binding's observed usage"
    );
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn replay_reset_clears_calibration_and_unknown_usage_does_not_restore_it() {
    let root = root("replay");
    let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
    let service = Arc::new(ScriptedModelService::new([
        replay_response("first", Usage::known(80_000, 1)),
        Script::OpenError(ProviderError {
            kind: ProviderErrorKind::ReplayContextChanged,
            message: "signed replay changed".into(),
            retry_after_ms: None,
        }),
        text_response("recovered", Usage::unknown()),
        text_response("third", Usage::unknown()),
    ]));
    let agent = configured(service.clone(), Arc::new(Echo), model(), limits());
    agent
        .submit(
            session.clone(),
            format!("first {}", "x".repeat(5_000)),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    agent
        .submit(
            session.clone(),
            "second".into(),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    agent
        .submit(
            session.clone(),
            "third".into(),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();

    let requests = service.requests();
    assert_eq!(requests.len(), 4);
    assert!(
        requests[1].controls.max_output_tokens < 20_000,
        "initial replay request did not use the committed sample"
    );
    assert_eq!(
        requests[2].controls.max_output_tokens, 20_000,
        "replay rebase did not clear the sample"
    );
    assert_eq!(
        requests[3].controls.max_output_tokens, 20_000,
        "unknown usage restored a sample from before the replay reset"
    );
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn compaction_clears_calibration_before_the_next_request() {
    let root = root("compaction");
    let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
    let service = Arc::new(ScriptedModelService::new([
        text_response("first", Usage::known(80_000, 1)),
        text_response("next", Usage::unknown()),
    ]));
    let agent = configured(service.clone(), Arc::new(Echo), model(), limits());
    agent
        .submit(
            session.clone(),
            format!("first {}", "x".repeat(5_000)),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    let entries = session.view().unwrap().entries;
    let through_entry = entries
        .iter()
        .position(|entry| matches!(entry, crate::SessionEntry::TurnEnded { .. }))
        .unwrap() as u64
        + 1;
    session
        .record_compaction(
            through_entry,
            "summary ".repeat(2_500),
            ModelExecution {
                route: ModelRoute::direct(model(), ModelRouteReason::UserRequest),
                returned_model: None,
            },
            Usage::unknown(),
        )
        .unwrap();
    agent
        .submit(
            session.clone(),
            "next".into(),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(
        service.requests()[1].controls.max_output_tokens,
        20_000,
        "a later request reused the calibration from before compaction"
    );
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn identical_images_keep_observed_context_and_changed_images_fall_back() {
    let root = root("images");
    let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
    let service = Arc::new(ScriptedModelService::new([
        text_response("first", Usage::known(100_000, 1)),
        text_response("same", Usage::unknown()),
        text_response("changed", Usage::unknown()),
    ]));
    let limits = AgentLimits {
        max_output_tokens: 20_000,
        context_window_tokens: Some(120_000),
        image_input: true,
        ..AgentLimits::default()
    };
    let agent = configured(service.clone(), Arc::new(Echo), model(), limits);
    let (png, gif) = (tiny_image(), gif_image());
    let first = Message {
        role: Role::User,
        content: vec![
            Content::Text("two images".into()),
            Content::Image(png.clone()),
            Content::Image(gif.clone()),
        ],
        provider_replay: None,
    };
    agent
        .submit_message(
            session.clone(),
            first,
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    agent
        .submit(
            session.clone(),
            "same images with more text".into(),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    let changed = Message {
        role: Role::User,
        content: vec![
            Content::Text("changed image order".into()),
            Content::Image(gif),
            Content::Image(png),
        ],
        provider_replay: None,
    };
    agent
        .submit_message(
            session.clone(),
            changed,
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();

    let requests = service.requests();
    assert_eq!(requests[0].controls.max_output_tokens, 20_000);
    assert!(
        requests[1].controls.max_output_tokens < 20_000,
        "identical images did not retain their observed context baseline"
    );
    assert_eq!(
        requests[2].controls.max_output_tokens, 20_000,
        "changed images reused a different image sample"
    );
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn tiny_calibrated_estimate_cannot_relax_the_retained_byte_limit() {
    let root = root("bytes");
    let session = Arc::new(Session::create(root.join("session.db"), None).unwrap());
    let service = Arc::new(ScriptedModelService::new([text_response(
        "first",
        Usage::known(1, 1),
    )]));
    let limits = AgentLimits {
        max_request_bytes: 2_000,
        ..AgentLimits::default()
    };
    let agent = configured(service.clone(), Arc::new(Echo), model(), limits);
    agent
        .submit(
            session.clone(),
            "first".into(),
            "base".into(),
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();

    session.begin_turn("x".repeat(1_400), model()).unwrap();
    let model = model();
    let tools = ToolSet::new([Arc::new(Echo) as Arc<dyn ToolSource>]);
    let model_service: Arc<dyn ion_ai::ModelService> = service.clone();
    let prepared = PreparedRequest::new(
        &session,
        session.request_basis(&model).unwrap(),
        &model_service,
        &tools,
        ModelRouteReason::UserRequest,
        RequestSettings {
            model: &model,
            instructions: "base",
            limits,
            authority: None,
        },
        None,
    );
    assert!(matches!(prepared, Err(AgentError::ContextTooLarge)));
    assert_eq!(service.requests().len(), 1);
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn output_budget_saturates_extreme_inputs() {
    let limits = AgentLimits {
        context_window_tokens: Some(10_000),
        ..AgentLimits::default()
    };
    assert_eq!(limits.output_budget(0, u64::MAX, 100), None);
    assert_eq!(limits.output_budget(0, u64::MAX - 100, 100), None);
}
