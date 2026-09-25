use futures_util::StreamExt;
use ion_ai::{
    Content, GenerationControls, IncompleteReason, Message, ModelRef, ModelRequest, ModelResponse,
    ModelService, ModelStreamEvent, ProviderError, ProviderErrorKind, ProviderReplay, Reasoning,
    ResponseTermination, Role, Script, ScriptedModelService, ToolCall, ToolChoice, ToolSpec, Usage,
};

fn controls() -> GenerationControls {
    GenerationControls {
        max_output_tokens: 1024,
        temperature: None,
        top_p: None,
        reasoning: Reasoning::ProviderDefault,
        tool_choice: ToolChoice::Auto,
        parallel_tool_calls: false,
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        model: ModelRef {
            provider: "scripted".to_owned(),
            model: "test-model".to_owned(),
        },
        instructions: Some("be careful".to_owned()),
        messages: vec![Message {
            role: Role::User,
            content: vec![Content::Text("inspect the file".to_owned())],
            provider_replay: None,
        }],
        tools: vec![ToolSpec {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        }],
        controls: controls(),
    }
}

fn response(message: Message, usage: Usage, termination: ResponseTermination) -> ModelResponse {
    ModelResponse {
        message,
        usage,
        termination,
        returned_model: None,
    }
}

#[tokio::test]
async fn scripted_service_streams_provider_neutral_events() {
    let usage = Usage::known(10, 4);
    let tool_call = ToolCall {
        id: "call-1".to_owned(),
        name: "read".to_owned(),
        arguments: serde_json::json!({"path": "src/lib.rs"}),
    };
    let response = response(
        Message {
            role: Role::Assistant,
            content: vec![
                Content::Text("I will inspect it.".to_owned()),
                Content::ToolCall(tool_call.clone()),
            ],
            provider_replay: Some(ProviderReplay::new(
                "scripted",
                "reasoning",
                serde_json::json!({"opaque": "provider-state"}),
            )),
        },
        usage,
        ResponseTermination::Completed,
    );
    let expected = vec![
        ModelStreamEvent::TextDelta("I will inspect it.".to_owned()),
        ModelStreamEvent::ToolCall(tool_call),
        ModelStreamEvent::Usage(usage),
        ModelStreamEvent::Completed(response.clone()),
    ];
    let service = ScriptedModelService::new([Script::Stream(expected.clone())]);
    let service_ref: &dyn ModelService = &service;

    let stream = service_ref.stream(request()).await.expect("open stream");
    let observed: Vec<_> = stream
        .map(|event| event.expect("scripted event"))
        .collect()
        .await;

    assert_eq!(observed, expected);
    assert_eq!(
        observed.last(),
        Some(&ModelStreamEvent::Completed(response.clone()))
    );
    assert!(response.is_complete());
    let replay = response
        .message
        .provider_replay
        .as_ref()
        .expect("replay material");
    assert!(replay.is_compatible_with("scripted"));
    assert!(!replay.is_compatible_with("other-provider"));
}

#[tokio::test]
async fn incomplete_termination_is_not_a_complete_answer() {
    let response = response(
        Message {
            role: Role::Assistant,
            content: vec![Content::Text("partial".to_owned())],
            provider_replay: None,
        },
        Usage::known(120, 0),
        ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
    );
    assert!(!response.is_complete());
    assert_eq!(
        response.termination,
        ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
    );

    let service = ScriptedModelService::new([Script::Stream(vec![
        ModelStreamEvent::TextDelta("partial".to_owned()),
        ModelStreamEvent::Completed(response),
    ])]);
    let stream = service.stream(request()).await.expect("open stream");
    let observed: Vec<_> = stream
        .map(|event| event.expect("scripted event"))
        .collect()
        .await;
    match observed.last() {
        Some(ModelStreamEvent::Completed(response)) => assert!(!response.is_complete()),
        other => panic!("expected completion, got {other:?}"),
    }
}

#[test]
fn unknown_usage_is_distinct_from_reported_zero() {
    let unknown = Usage::unknown();
    let zero = Usage::known(0, 0);
    assert_ne!(unknown, zero);
    assert!(!unknown.is_known());
    assert!(zero.is_known());
    // Structured usage also survives a round trip without collapsing to zero.
    let encoded = serde_json::to_value(unknown).expect("serialize");
    assert_eq!(
        encoded,
        serde_json::json!({"input_tokens": null, "output_tokens": null})
    );
    let decoded: Usage = serde_json::from_value(encoded).expect("deserialize");
    assert_eq!(decoded, unknown);
}

#[tokio::test]
async fn typed_open_failure_has_no_hidden_retry() {
    let service = ScriptedModelService::new([Script::OpenError(ProviderError {
        kind: ProviderErrorKind::RateLimited,
        message: "try later".to_owned(),
    })]);

    let error = match service.stream(request()).await {
        Ok(_) => panic!("script must fail before stream creation"),
        Err(error) => error,
    };

    assert_eq!(error.kind, ProviderErrorKind::RateLimited);
    assert_eq!(service.requests().len(), 1);
}
