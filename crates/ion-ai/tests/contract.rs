use futures_util::StreamExt;
use ion_ai::{
    Content, Message, ModelRef, ModelRequest, ModelResponse, ModelService, ModelStreamEvent,
    ProviderError, ProviderErrorKind, Role, Script, ScriptedModelService, ToolCall, ToolSpec,
    Usage,
};

fn request() -> ModelRequest {
    ModelRequest {
        model: ModelRef {
            provider: "scripted".to_owned(),
            model: "test-model".to_owned(),
        },
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
    }
}

#[tokio::test]
async fn scripted_service_streams_provider_neutral_events() {
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 4,
    };
    let tool_call = ToolCall {
        id: "call-1".to_owned(),
        name: "read".to_owned(),
        arguments: serde_json::json!({"path": "src/lib.rs"}),
    };
    let response = ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![
                Content::Text("I will inspect it.".to_owned()),
                Content::ToolCall(tool_call.clone()),
            ],
            provider_replay: Some(serde_json::json!({"opaque": "provider-state"})),
        },
        usage,
    };
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
        Some(&ModelStreamEvent::Completed(response))
    );
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
