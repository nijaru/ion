mod responses;
mod stream_limits;
mod tool_ids;

use super::*;
use ion_ai::{
    GenerationControls, ModelContextChange, ModelContextState, ModelContextTimeline, ModelRef,
    ToolResult, ToolSpec,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

async fn read_request_body(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut reader = BufReader::new(socket);
    let mut length = 0;
    loop {
        let mut line = String::new();
        assert_ne!(reader.read_line(&mut line).await.unwrap(), 0);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.unwrap();
    body
}

async fn serve(body: String, status: &str) -> String {
    serve_split(body, status, usize::MAX).await
}

async fn serve_split(body: String, status: &str, chunk_size: usize) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let status = status.to_owned();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request_body(&mut socket).await;
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        for chunk in response.as_bytes().chunks(chunk_size) {
            socket.write_all(chunk).await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    endpoint
}

fn request() -> ModelRequest {
    ModelRequest {
        route: ModelRoute::direct(
            ModelRef {
                provider: "test".into(),
                model: "test-model".into(),
            },
            ModelRouteReason::UserRequest,
        ),
        provider_session_id: None,
        instructions: Some("instructions".into()),
        messages: vec![Message {
            role: Role::User,
            content: vec![Content::Text("hello".into())],
            provider_replay: None,
        }],
        tools: vec![ToolSpec {
            name: "read".into(),
            description: "read".into(),
            input_schema: json!({"type":"object"}),
        }],
        context_timeline: None,
        prompt_cache: ion_ai::PromptCacheIntent::Reusable,
        controls: GenerationControls {
            max_output_tokens: 1024,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: false,
        },
    }
}

fn anthropic_state(request: &ModelRequest) -> AnthropicState {
    let body = anthropic_body(request, true).unwrap();
    AnthropicState::new(anthropic_body_prefix_digest(&body).unwrap())
}

#[test]
fn openrouter_conversation_affinity_is_explicit_and_wire_scoped() {
    let mut request = request();
    request.provider_session_id = Some("018f7d1b-2391-7000-8000-000000000001".into());
    let body = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
    assert_eq!(body["session_id"], "018f7d1b-2391-7000-8000-000000000001");
    for wire in [
        HttpWire::ChatCompletions,
        HttpWire::DeepSeekChat,
        HttpWire::MiMoChat,
        HttpWire::LlamaCppNoThinking,
    ] {
        assert!(
            chat_body(&request, wire)
                .unwrap()
                .get("session_id")
                .is_none()
        );
    }
    assert!(
        anthropic_body(&request, false)
            .unwrap()
            .get("session_id")
            .is_none()
    );
    request.prompt_cache = PromptCacheIntent::Default;
    assert_eq!(
        chat_body(&request, HttpWire::OpenRouterChat).unwrap()["session_id"],
        body["session_id"]
    );
    request.provider_session_id = None;
    assert!(
        chat_body(&request, HttpWire::OpenRouterChat)
            .unwrap()
            .get("session_id")
            .is_none()
    );
    for id in [String::new(), "a".repeat(257)] {
        request.provider_session_id = Some(id);
        assert!(chat_body(&request, HttpWire::OpenRouterChat).is_err());
    }
    request.provider_session_id = Some("a".repeat(256));
    assert!(chat_body(&request, HttpWire::OpenRouterChat).is_ok());
}

#[tokio::test]
async fn catalog_openrouter_route_keeps_affinity_controls_and_structured_replay() {
    let store = crate::model_setup::ModelStore::new(
        std::env::temp_dir().join("ion-unused-catalog-config"),
    );
    let identity = ModelRef {
        provider: "openrouter".into(),
        model: "openai/gpt-5.4".into(),
    };
    let selection = store.resolve_identity(&identity).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let detail = json!({"type":"reasoning.encrypted","data":"fixture-opaque","index":0});
    let event = json!({"choices":[{"index":0,"delta":{"reasoning_details":[detail.clone()],"content":"done"},"finish_reason":"stop"}]});
    let body = format!("data: {event}\n\ndata: [DONE]\n\n");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let captured: Value =
            serde_json::from_slice(&read_request_body(&mut socket).await).unwrap();
        socket.write_all(format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        ).as_bytes()).await.unwrap();
        captured
    });
    let mut request = request();
    request.route = ModelRoute::direct(identity, ModelRouteReason::UserRequest);
    request.provider_session_id = Some("fixture-session".into());
    request.controls.reasoning = Reasoning::High;
    let service = HttpModelService::new(&endpoint, selection.wire, Arc::new(|| None)).unwrap();
    let mut stream = service.stream(request.clone()).await.unwrap();
    let mut completed = None;
    while let Some(event) = stream.next().await {
        if let ModelStreamEvent::Completed(response) = event.unwrap() {
            completed = Some(response);
        }
    }
    let captured = server.await.unwrap();
    assert_eq!(captured["session_id"], "fixture-session");
    assert_eq!(captured["reasoning"]["effort"], "high");
    let response = completed.unwrap();
    assert_eq!(response.message.content, vec![Content::Text("done".into())]);
    request.messages.push(response.message);
    request
        .messages
        .push(Message::user_input("continue".into(), []));
    let continuation = chat_body(&request, selection.wire).unwrap();
    let assistant = continuation["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    assert_eq!(assistant["reasoning_details"], json!([detail]));
}

#[test]
fn anthropic_inline_tool_deltas_keep_the_initial_top_level_prefix() {
    let search = ToolSpec {
        name: "tool_search".into(),
        description: "search available tools".into(),
        input_schema: json!({"type":"object"}),
    };
    let special = ToolSpec {
        name: "special_lookup".into(),
        description: "specialized lookup".into(),
        input_schema: json!({"type":"object"}),
    };
    let mut request = request();
    request.route.effective.provider = "anthropic".into();
    request.route.effective.model = "claude-opus-5-5".into();
    request.tools = vec![special.clone()];
    request.messages = vec![
        Message::user_input("find a tool".into(), []),
        Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(ToolCall {
                id: "search".into(),
                name: "tool_search".into(),
                arguments: json!({"query":"special"}),
                raw_arguments: None,
            })],
            provider_replay: None,
        },
        Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "search".into(),
                name: "tool_search".into(),
                result: json!({"loaded":["special_lookup"]}),
                images: Vec::new(),
                is_error: false,
            })],
            provider_replay: None,
        },
    ];
    request.context_timeline = Some(ModelContextTimeline {
        initial: ModelContextState {
            instructions: request.instructions.clone(),
            tools: vec![search.clone()],
        },
        changes: vec![ModelContextChange {
            after_message: 3,
            context: ModelContextState {
                instructions: request.instructions.clone(),
                tools: vec![special.clone()],
            },
        }],
    });

    let body = anthropic_body_for_route(&request, true, true).unwrap();
    assert_eq!(
        body["tools"],
        json!([{
            "name":"tool_search",
            "description":"search available tools",
            "input_schema":{"type":"object"}
        }])
    );
    let system = body["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(system["role"], "system");
    assert_eq!(
        system["content"],
        json!([
            {
                "type":"tool_removal",
                "tool":{"type":"tool_reference","name":"tool_search"}
            },
            {
                "type":"tool_addition",
                "tool":{
                    "type":"tool_definition",
                    "definition":{
                        "name":"special_lookup",
                        "description":"specialized lookup",
                        "input_schema":{"type":"object"}
                    }
                }
            }
        ])
    );
    assert!(anthropic_body_uses_inline_tools(&body));
}

#[test]
fn anthropic_inline_tools_preserve_requested_tool_restrictions() {
    let mut request = request();
    request.route.effective.provider = "anthropic".into();
    request.route.effective.model = "claude-opus-5-5".into();
    request.context_timeline = Some(ModelContextTimeline {
        initial: ModelContextState {
            instructions: request.instructions.clone(),
            tools: Vec::new(),
        },
        changes: vec![ModelContextChange {
            after_message: 1,
            context: ModelContextState {
                instructions: request.instructions.clone(),
                tools: request.tools.clone(),
            },
        }],
    });
    for (choice, parallel, expected) in [
        (ToolChoice::None, false, json!({"type":"none"})),
        (
            ToolChoice::Auto,
            false,
            json!({"type":"auto","disable_parallel_tool_use":true}),
        ),
        (
            ToolChoice::Auto,
            true,
            json!({"type":"auto","disable_parallel_tool_use":false}),
        ),
    ] {
        request.controls.tool_choice = choice;
        request.controls.parallel_tool_calls = parallel;
        let body = anthropic_body_for_route(&request, true, true).unwrap();
        assert!(body.get("tools").is_none());
        assert!(anthropic_body_uses_inline_tools(&body));
        assert_eq!(
            body["messages"][1]["content"][0]["tool"]["definition"]["name"],
            "read"
        );
        assert_eq!(body["tool_choice"], expected);
    }

    let timeline = request.context_timeline.as_mut().unwrap();
    timeline.initial.tools = std::mem::take(&mut request.tools);
    timeline.changes[0].context.tools.clear();
    request.controls.tool_choice = ToolChoice::None;
    let body = anthropic_body_for_route(&request, true, true).unwrap();
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["messages"][1]["content"][0]["type"], "tool_removal");
    assert_eq!(body["tool_choice"], json!({"type":"none"}));
}

#[test]
fn anthropic_inline_tool_definition_replaces_same_name_schema() {
    let old = ToolSpec {
        name: "read".into(),
        description: "old".into(),
        input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
    };
    let new = ToolSpec {
        name: "read".into(),
        description: "new".into(),
        input_schema: json!({"type":"object","properties":{"path":{"type":"string"},"limit":{"type":"integer"}}}),
    };
    let mut request = request();
    request.route.effective.provider = "anthropic".into();
    request.route.effective.model = "claude-opus-5-5".into();
    request.tools = vec![new.clone()];
    request.context_timeline = Some(ModelContextTimeline {
        initial: ModelContextState {
            instructions: request.instructions.clone(),
            tools: vec![old],
        },
        changes: vec![ModelContextChange {
            after_message: 1,
            context: ModelContextState {
                instructions: request.instructions.clone(),
                tools: vec![new],
            },
        }],
    });
    let body = anthropic_body_for_route(&request, true, true).unwrap();
    let blocks = body["messages"][1]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "tool_addition");
    assert_eq!(blocks[0]["tool"]["definition"]["description"], "new");
    assert_eq!(blocks[0]["tool"]["definition"]["name"], "read");
}

#[test]
fn anthropic_instruction_change_falls_back_to_latest_leading_context() {
    let mut request = request();
    request.route.effective.provider = "anthropic".into();
    request.route.effective.model = "claude-opus-5-5".into();
    let current = request.tools.clone();
    request.instructions = Some("new instructions".into());
    request.context_timeline = Some(ModelContextTimeline {
        initial: ModelContextState {
            instructions: Some("old instructions".into()),
            tools: current.clone(),
        },
        changes: vec![ModelContextChange {
            after_message: 1,
            context: ModelContextState {
                instructions: request.instructions.clone(),
                tools: current,
            },
        }],
    });
    let body = anthropic_body_for_route(&request, true, true).unwrap();
    assert_eq!(body["system"], "new instructions");
    assert!(!anthropic_body_uses_inline_tools(&body));
    assert!(
        body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| message["role"] != "system")
    );
}

#[test]
fn anthropic_tool_delta_preserves_signed_replay_prefix() {
    let old_tool = ToolSpec {
        name: "read".into(),
        description: "read".into(),
        input_schema: json!({"type":"object"}),
    };
    let new_tool = ToolSpec {
        name: "write".into(),
        description: "write".into(),
        input_schema: json!({"type":"object"}),
    };
    let mut initial = request();
    initial.route.effective.provider = "anthropic".into();
    initial.route.effective.model = "claude-opus-5-5".into();
    initial.tools = vec![old_tool.clone()];
    let initial_body = anthropic_body(&initial, true).unwrap();
    let prefix = anthropic_body_prefix_digest(&initial_body).unwrap();

    let call = ToolCall {
        id: "tool_1".into(),
        name: "read".into(),
        arguments: json!({"path":"x"}),
        raw_arguments: None,
    };
    let replay = ProviderReplay::new(
        "anthropic",
        ANTHROPIC_CONTENT_REPLAY,
        json!({
            "blocks":[
                {"type":"thinking","thinking":"","signature":"signature"},
                {"type":"tool_use","id":"tool_1","name":"read","input":{"path":"x"}}
            ],
            "prefix_sha256":prefix
        }),
    )
    .with_prefix_binding(true);

    let mut continuation = initial.clone();
    continuation.messages.push(Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(call)],
        provider_replay: Some(replay),
    });
    continuation.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: "tool_1".into(),
            name: "read".into(),
            result: json!({"text":"ok"}),
            images: Vec::new(),
            is_error: false,
        })],
        provider_replay: None,
    });
    continuation.tools = vec![new_tool.clone()];
    continuation.context_timeline = Some(ModelContextTimeline {
        initial: ModelContextState {
            instructions: initial.instructions.clone(),
            tools: vec![old_tool],
        },
        changes: vec![ModelContextChange {
            after_message: 3,
            context: ModelContextState {
                instructions: continuation.instructions.clone(),
                tools: vec![new_tool],
            },
        }],
    });

    let body = anthropic_body_for_route(&continuation, true, true).unwrap();
    assert_eq!(body["messages"][1]["content"][0]["type"], "thinking");
    assert_eq!(body["messages"][3]["role"], "system");
    assert!(anthropic_body_uses_inline_tools(&body));
}

#[test]
fn user_images_keep_order_and_encode_for_each_supported_wire() {
    let image: ion_ai::ImageContent = serde_json::from_value(json!({
        "mime_type":"image/png",
        "data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
    }))
    .unwrap();
    let mut request = request();
    request.instructions = None;
    request.messages[0].content = vec![
        Content::Text("inspect".into()),
        Content::Image(image.clone()),
        Content::Text("and explain".into()),
    ];
    let chat = chat_body(&request, HttpWire::ChatCompletions).unwrap();
    let chat = &chat["messages"];
    assert_eq!(chat[0]["content"][0]["text"], "inspect");
    assert!(
        chat[0]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );
    assert_eq!(chat[0]["content"][2]["text"], "and explain");
    let anthropic = anthropic_body(&request, false).unwrap();
    let anthropic = &anthropic["messages"];
    assert_eq!(anthropic[0]["content"][0]["text"], "inspect");
    assert_eq!(
        anthropic[0]["content"][1]["source"]["media_type"],
        "image/png"
    );
    assert_eq!(anthropic[0]["content"][2]["text"], "and explain");

    request.messages[0].content = vec![Content::Text(String::new()), Content::Image(image)];
    let chat = chat_body(&request, HttpWire::ChatCompletions).unwrap();
    let chat = &chat["messages"];
    let chat_parts = chat[0]["content"].as_array().unwrap();
    assert_eq!(chat_parts.len(), 1);
    assert_eq!(chat_parts[0]["type"], "image_url");
    let anthropic = anthropic_body(&request, false).unwrap();
    let anthropic = &anthropic["messages"];
    assert_eq!(anthropic[0]["content"].as_array().unwrap().len(), 1);
}

#[test]
fn explicit_http_endpoint_accepts_non_loopback_host() {
    assert!(
        HttpModelService::validate_endpoint("http://desktop:8080/v1/chat/completions").is_ok()
    );
    assert!(HttpModelService::validate_endpoint("file:///tmp/model").is_err());
}

#[test]
fn compatible_base_url_resolves_one_standard_request_path() {
    for (input, wire, expected) in [
        (
            "http://desktop:8080/v1",
            HttpWire::ChatCompletions,
            "http://desktop:8080/v1/chat/completions",
        ),
        (
            "http://desktop:8080/v1/chat/completions",
            HttpWire::ChatCompletions,
            "http://desktop:8080/v1/chat/completions",
        ),
        (
            "https://proxy.example/api/v1/",
            HttpWire::OpenRouterChat,
            "https://proxy.example/api/v1/chat/completions",
        ),
        (
            "https://api.anthropic.com",
            HttpWire::AnthropicMessages,
            "https://api.anthropic.com/v1/messages",
        ),
        (
            "https://proxy.example/anthropic/v1/",
            HttpWire::AnthropicMessages,
            "https://proxy.example/anthropic/v1/messages",
        ),
        (
            "https://proxy.example/anthropic/v1/messages",
            HttpWire::AnthropicMessages,
            "https://proxy.example/anthropic/v1/messages",
        ),
    ] {
        assert_eq!(
            HttpModelService::resolve_endpoint(input, wire).unwrap(),
            expected
        );
    }
    assert!(
        HttpModelService::resolve_endpoint(
            "http://desktop:8080/v1?key=hidden",
            HttpWire::ChatCompletions,
        )
        .is_err()
    );
}

#[test]
fn structured_context_error_is_distinct_from_other_bad_requests() {
    for (status, envelope, expected) in [
        (
            400,
            json!({"error":{"code":"context_length_exceeded"}}),
            ProviderErrorKind::ContextLength,
        ),
        (
            400,
            json!({"error":{"code":"","type":"context_length_exceeded"}}),
            ProviderErrorKind::ContextLength,
        ),
        (
            400,
            json!({"error":{"code":"invalid_api_key"}}),
            ProviderErrorKind::InvalidRequest,
        ),
        (413, Value::Null, ProviderErrorKind::InvalidRequest),
        (
            413,
            json!({"error":{"type":"request_too_large"}}),
            ProviderErrorKind::ContextLength,
        ),
        (
            400,
            json!({"error":{"message":"Prompt is too long"}}),
            ProviderErrorKind::ContextLength,
        ),
        (
            400,
            json!({"message":"Provider returned error","metadata":{"raw":"Maximum context length exceeded"}}),
            ProviderErrorKind::ContextLength,
        ),
        (
            400,
            json!({"error":{"message":"Provider returned error","metadata":{"raw":{"message":"Maximum context length exceeded"}}}}),
            ProviderErrorKind::InvalidRequest,
        ),
        (
            429,
            json!({"error":{"message":"Context limit mentioned in a rate-limit response"}}),
            ProviderErrorKind::RateLimited,
        ),
        (
            429,
            json!({"error":{"code":"insufficient_quota"}}),
            ProviderErrorKind::Quota,
        ),
        (
            429,
            json!({"error":{"message":"quota used"},"code":"insufficient_quota"}),
            ProviderErrorKind::Quota,
        ),
    ] {
        assert_eq!(classify_status_error(status, Some(&envelope)), expected);
    }
    assert_eq!(
        provider_error_detail_value(&json!({"error":{"message":"bad\nrequest"}})).as_deref(),
        Some("badrequest")
    );
}

#[test]
fn context_rejection_kind_survives_wrapping_and_display_truncation() {
    for (code, message, raw) in [
        (
            json!(400),
            "Provider returned error".to_owned(),
            "This model's maximum context length is 1050000 tokens.",
        ),
        (
            Value::Null,
            "input length and max_tokens exceed context limit: 199000 + 8192 > 200000"
                .to_owned(),
            "",
        ),
        (
            json!(""),
            "Input length 12345 exceeds the maximum allowed input length of 8192 tokens."
                .to_owned(),
            "",
        ),
        (
            Value::Null,
            "Generic provider information. ".repeat(30),
            "Prompt is too long",
        ),
    ] {
        let value = json!({"error":{
            "code":code,"type":"invalid_request_error","message":message,"metadata":{"raw":raw}
        }});
        assert_eq!(
            classify_status_error(400, Some(&value)),
            ProviderErrorKind::ContextLength
        );
        assert_eq!(
            chat_stream_error(&value).kind,
            ProviderErrorKind::ContextLength
        );
        assert_eq!(
            anthropic_sse_error(&value).kind,
            ProviderErrorKind::ContextLength
        );
        assert!(provider_error_detail_value(&value).unwrap().chars().count() <= 500);
    }
    let untyped = json!({"error":{"message":"Provider returned error","metadata":{"raw":"Prompt is too long"}}});
    assert_eq!(
        chat_stream_error(&untyped).kind,
        ProviderErrorKind::ContextLength
    );
    for (code, expected) in [
        ("rate_limit_exceeded", ProviderErrorKind::RateLimited),
        ("insufficient_quota", ProviderErrorKind::Quota),
        ("server_error", ProviderErrorKind::Server),
    ] {
        assert_eq!(
            chat_stream_error(&json!({"error":{
                "code":code,"message":"Failure while checking context limit"
            }}))
            .kind,
            expected
        );
    }
    for (kind, expected) in [
        ("rate_limit_error", ProviderErrorKind::RateLimited),
        ("authentication_error", ProviderErrorKind::Authentication),
    ] {
        assert_eq!(
            anthropic_sse_error(&json!({"error":{
                "type":kind,"message":"Failure while checking context limit"
            }}))
            .kind,
            expected
        );
    }
}

#[test]
fn wrapped_provider_errors_retain_bounded_upstream_text() {
    for value in [
        json!({"error":{"code":429,"message":"Provider returned error","metadata":{
            "raw":"Laguna is temporarily rate-limited upstream. Retry shortly.",
            "provider_name":"Poolside","limit_source":"upstream_provider_shared_pool"
        }}}),
        json!({"code":429,"message":"Provider returned error","metadata":{
            "raw":"Laguna is temporarily rate-limited upstream. Retry shortly."
        }}),
    ] {
        let detail = provider_error_detail_value(&value).unwrap();
        assert_eq!(
            detail,
            "Provider returned error: Laguna is temporarily rate-limited upstream. Retry shortly."
        );
        let error = chat_stream_error(&json!({"error": value.get("error").unwrap_or(&value)}));
        assert_eq!(error.kind, ProviderErrorKind::RateLimited);
        assert!(error.message.contains(&detail));
    }
    assert_eq!(
        provider_error_detail_value(&json!({"error":{"metadata":{"raw":"upstream reason"}}}))
            .as_deref(),
        Some("upstream reason")
    );
    for raw in [
        json!("failed"),
        json!(""),
        json!({"secret":"not diagnostic text"}),
    ] {
        assert_eq!(
            provider_error_detail_value(
                &json!({"error":{"message":"failed","metadata":{"raw":raw}}})
            )
            .as_deref(),
            Some("failed")
        );
    }
    let detail = provider_error_detail_value(&json!({"error":{
        "message":"failed","metadata":{"raw":format!("\n\u{1b}\t{}", "界".repeat(600))}
    }}))
    .unwrap();
    assert_eq!(detail.chars().count(), 500);
    assert!(detail.starts_with("failed: 界"));
    assert!(!detail.chars().any(char::is_control));
}

#[test]
fn error_prefix_is_independent_of_transport_segmentation() {
    let body = format!(
        "{{\"error\":{{\"code\":\"insufficient_quota\"}}}}{}",
        " ".repeat(64 * 1024)
    );
    for chunk_size in [1, 1024, usize::MAX] {
        let mut retained = Vec::new();
        for chunk in body.as_bytes().chunks(chunk_size) {
            append_error_prefix(&mut retained, chunk);
        }
        assert_eq!(retained.len(), MAX_ERROR_BODY);
        assert_eq!(retained, body.as_bytes()[..MAX_ERROR_BODY]);
        let value: Value = serde_json::from_slice(&retained).unwrap();
        assert_eq!(
            classify_status_error(429, Some(&value)),
            ProviderErrorKind::Quota
        );
    }
}

#[test]
fn anthropic_sse_error_keeps_provider_reason_and_kind() {
    let mut decoder = Decoder::Anthropic(anthropic_state(&request()));
    let error = decode_frame(
        b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Too busy\"}}\n\n",
        &mut decoder,
        &request(),
        HttpWire::AnthropicMessages,
    )
    .unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::Overloaded);
    assert!(error.message.contains("Too busy"));

    let mut decoder = Decoder::Anthropic(anthropic_state(&request()));
    let error = decode_frame(
        b"event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"Slow down\"}}\n\n",
        &mut decoder,
        &request(),
        HttpWire::AnthropicMessages,
    )
    .unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::RateLimited);
    assert!(error.message.contains("Slow down"));
}

#[test]
fn anthropic_unknown_sse_event_does_not_interrupt_a_message() {
    let mut decoder = Decoder::Anthropic(anthropic_state(&request()));
    assert!(
        decode_frame(
            b"event: future_event\ndata: {\"type\":\"future_event\"}\n\n",
            &mut decoder,
            &request(),
            HttpWire::AnthropicMessages,
        )
        .unwrap()
        .is_empty()
    );
    assert!(decode_frame(
        b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"type\":\"message\",\"role\":\"assistant\",\"model\":\"returned\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
        &mut decoder,
        &request(),
        HttpWire::AnthropicMessages,
    )
    .is_ok());
    assert!(
        decode_frame(
            b"event: future_event\ndata: {\"type\":\"future_event\"}\n\n",
            &mut decoder,
            &request(),
            HttpWire::AnthropicMessages,
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn server_retry_headers_are_parsed_without_unbounded_arithmetic() {
    let mut headers = HeaderMap::new();
    headers.insert("retry-after", HeaderValue::from_static("2"));
    assert_eq!(parse_retry_after(&headers), Some(2000));
    headers.insert("retry-after-ms", HeaderValue::from_static("75"));
    assert_eq!(parse_retry_after(&headers), Some(75));
    headers.remove("retry-after-ms");
    headers.insert(
        "retry-after",
        HeaderValue::from_static("18446744073709551615"),
    );
    assert_eq!(parse_retry_after(&headers), None);
    let date = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(5));
    headers.insert("retry-after", HeaderValue::from_str(&date).unwrap());
    assert!(parse_retry_after(&headers).is_some_and(|delay| delay <= 5000));
}

#[test]
fn chat_preserves_tool_status_independently_of_arbitrary_payload() {
    for wire in [
        HttpWire::ChatCompletions,
        HttpWire::LlamaCppNoThinking,
        HttpWire::DeepSeekChat,
        HttpWire::MiMoChat,
        HttpWire::OpenRouterChat,
    ] {
        for payload in [
            json!(null),
            json!({"is_error":true,"error":"user data","result":[1,"quoted\\\""]}),
        ] {
            for is_error in [false, true] {
                let mut request = request();
                request.messages.push(Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "call".into(),
                        name: "read".into(),
                        arguments: json!({}),
                        raw_arguments: None,
                    })],
                    provider_replay: None,
                });
                request.messages.push(Message {
                    role: Role::Tool,
                    content: vec![Content::ToolResult(ToolResult {
                        call_id: "call".into(),
                        name: "read".into(),
                        result: payload.clone(),
                        images: Vec::new(),
                        is_error,
                    })],
                    provider_replay: None,
                });
                let body = chat_body(&request, wire).unwrap();
                let content: Value =
                    serde_json::from_str(body["messages"][3]["content"].as_str().unwrap())
                        .unwrap();
                assert_eq!(content, json!({"is_error":is_error,"result":payload}));
            }
        }
    }
}

#[test]
fn replay_preserves_call_ids_and_rejects_orphans() {
    let mut request = request();
    request.messages.push(Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(ToolCall {
            id: "provider-id".into(),
            name: "read".into(),
            arguments: json!({"path":"a"}),
            raw_arguments: None,
        })],
        provider_replay: None,
    });
    request.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: "provider-id".into(),
            name: "read".into(),
            result: json!({"ok":true}),
            images: Vec::new(),
            is_error: false,
        })],
        provider_replay: None,
    });
    request.messages.push(Message {
        role: Role::User,
        content: vec![Content::Text("again".into())],
        provider_replay: None,
    });
    let chat = chat_body(&request, HttpWire::ChatCompletions).unwrap();
    assert_eq!(chat["messages"][2]["tool_calls"][0]["id"], "provider-id");
    assert_eq!(chat["messages"][3]["tool_call_id"], "provider-id");
    let anthropic = anthropic_body(&request, true).unwrap();
    assert_eq!(anthropic["messages"][1]["content"][0]["id"], "provider-id");
    assert_eq!(
        anthropic["messages"][2]["content"][0]["tool_use_id"],
        "provider-id"
    );
    assert_eq!(anthropic["messages"][2]["content"][0]["is_error"], false);
    request.messages[2].content = vec![Content::ToolResult(ToolResult {
        call_id: "provider-id".into(),
        name: "read".into(),
        result: json!({"error":"file missing"}),
        images: Vec::new(),
        is_error: true,
    })];
    let anthropic = anthropic_body(&request, true).unwrap();
    assert_eq!(anthropic["messages"][2]["content"][0]["is_error"], true);
    request.messages[1].content = vec![Content::ToolCall(ToolCall {
        id: "provider-id".into(),
        name: "read".into(),
        arguments: json!({}),
        raw_arguments: Some("{\"path\":".into()),
    })];
    assert_eq!(
        chat_body(&request, HttpWire::ChatCompletions).unwrap()["messages"][2]["tool_calls"][0]
            ["function"]["arguments"],
        "{}"
    );
    let mut duplicate = request.clone();
    let call = duplicate.messages[1].content[0].clone();
    duplicate.messages[1].content.push(call);
    for result in [
        chat_body(&duplicate, HttpWire::ChatCompletions),
        anthropic_body(&duplicate, true),
    ] {
        assert_eq!(result.unwrap_err().kind, ProviderErrorKind::InvalidRequest);
    }
    request.messages[2].content = vec![Content::ToolResult(ToolResult {
        call_id: "wrong".into(),
        name: "read".into(),
        result: json!(null),
        images: Vec::new(),
        is_error: true,
    })];
    assert!(chat_body(&request, HttpWire::ChatCompletions).is_err());
}

#[test]
fn replay_cannot_cross_wire_or_provider_boundaries() {
    let mut request = request();
    request.messages.push(Message {
        role: Role::Assistant,
        content: vec![Content::Text("answer".into())],
        provider_replay: None,
    });
    request
        .messages
        .push(Message::user_input("next".into(), []));
    for (kind, data, native) in [
        (CHAT_REASONING_CONTENT_REPLAY, json!("private"), false),
        (
            ANTHROPIC_CONTENT_REPLAY,
            json!({
                "blocks":[{"type":"text","text":"answer"}],
                "prefix_sha256":"0".repeat(64)
            }),
            true,
        ),
    ] {
        request.messages[1].provider_replay = Some(ProviderReplay::new("test", kind, data));
        let chat = chat_body(&request, HttpWire::DeepSeekChat);
        let anthropic = anthropic_body(&request, false);
        let (compatible, incompatible) = if native {
            (anthropic, chat)
        } else {
            (chat, anthropic)
        };
        assert!(compatible.is_ok());
        assert_eq!(
            incompatible.unwrap_err().kind,
            ProviderErrorKind::Unsupported
        );

        request.messages[1]
            .provider_replay
            .as_mut()
            .unwrap()
            .provider = "other".into();
        for result in [
            chat_body(&request, HttpWire::DeepSeekChat),
            anthropic_body(&request, false),
        ] {
            assert_eq!(result.unwrap_err().kind, ProviderErrorKind::Unsupported);
        }
    }
}

#[test]
fn tool_images_keep_all_tool_replies_adjacent_on_chat_and_stay_inside_anthropic_results() {
    let image = ion_ai::ImageContent::from_bytes(&[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
        8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207,
        192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
        96, 130,
    ])
    .unwrap();
    let mut request = request();
    request.messages.push(Message {
        role: Role::Assistant,
        content: ["first", "second"]
            .into_iter()
            .map(|id| {
                Content::ToolCall(ToolCall {
                    id: id.into(),
                    name: "read".into(),
                    arguments: json!({"path":"a.png"}),
                    raw_arguments: None,
                })
            })
            .collect(),
        provider_replay: None,
    });
    for id in ["first", "second"] {
        request.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: id.into(),
                name: "read".into(),
                result: json!({"path":"a.png"}),
                images: vec![image.clone()],
                is_error: false,
            })],
            provider_replay: None,
        });
    }
    let chat = chat_body(&request, HttpWire::ChatCompletions).unwrap();
    let messages = chat["messages"].as_array().unwrap();
    assert_eq!(messages[3]["role"], "tool");
    assert_eq!(messages[4]["role"], "tool");
    assert_eq!(messages[5]["role"], "user");
    assert_eq!(messages[5]["content"].as_array().unwrap().len(), 3);
    assert!(
        messages[5]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,")
    );

    let anthropic = anthropic_body(&request, true).unwrap();
    let results = anthropic["messages"][2]["content"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["content"][1]["type"], "image");
    assert_eq!(
        results[1]["content"][1]["source"]["media_type"],
        "image/png"
    );
}

#[test]
fn controls_are_encoded_or_rejected() {
    let mut request = request();
    request.controls.temperature = Some(0.4);
    request.controls.top_p = Some(0.9);
    request.controls.reasoning = Reasoning::High;
    let chat = chat_body(&request, HttpWire::ChatCompletions).unwrap();
    assert_eq!(chat["temperature"], 0.4);
    assert_eq!(chat["top_p"], 0.9);
    assert_eq!(chat["reasoning_effort"], "high");
    assert_eq!(
        anthropic_body(&request, true).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
    request.controls.reasoning = Reasoning::BudgetTokens(100);
    assert_eq!(
        chat_body(&request, HttpWire::ChatCompletions)
            .unwrap_err()
            .kind,
        ProviderErrorKind::Unsupported
    );
}

#[test]
fn direct_flash_profiles_keep_default_thinking_with_replay() {
    let request = request();
    let deepseek = chat_body(&request, HttpWire::DeepSeekChat).unwrap();
    assert!(deepseek.get("reasoning_effort").is_none());
    assert!(deepseek.get("thinking").is_none());
    assert_eq!(deepseek["max_tokens"], request.controls.max_output_tokens);
    assert!(deepseek.get("max_completion_tokens").is_none());
    let mimo = chat_body(&request, HttpWire::MiMoChat).unwrap();
    assert!(mimo.get("thinking").is_none());
    let openrouter = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
    assert!(openrouter.get("reasoning").is_none());
    let llama_cpp = chat_body(&request, HttpWire::LlamaCppNoThinking).unwrap();
    assert_eq!(llama_cpp["chat_template_kwargs"]["enable_thinking"], false);

    let mut thinking = request;
    thinking.controls.reasoning = Reasoning::High;
    assert_eq!(
        chat_body(&thinking, HttpWire::DeepSeekChat).unwrap()["reasoning_effort"],
        "high"
    );
    assert_eq!(
        chat_body(&thinking, HttpWire::MiMoChat).unwrap()["thinking"]["type"],
        "enabled"
    );
    assert_eq!(
        chat_body(&thinking, HttpWire::OpenRouterChat).unwrap()["reasoning"]["effort"],
        "high"
    );
    assert_eq!(
        chat_body(&thinking, HttpWire::LlamaCppNoThinking)
            .unwrap_err()
            .kind,
        ProviderErrorKind::Unsupported
    );
    thinking.controls.reasoning = Reasoning::Off;
    for wire in [HttpWire::DeepSeekChat, HttpWire::MiMoChat] {
        assert_eq!(
            chat_body(&thinking, wire).unwrap()["thinking"]["type"],
            "disabled"
        );
    }
    assert_eq!(
        chat_body(&thinking, HttpWire::OpenRouterChat).unwrap()["reasoning"]["enabled"],
        false
    );
    let mut state = ChatState::default();
    assert_eq!(state.accept(&json!({"choices":[{"delta":{"reasoning_content":"hidden"},"finish_reason":null}]})).unwrap_err().kind, ProviderErrorKind::Unsupported);
}

#[test]
fn direct_reasoning_survives_streaming_and_later_tool_requests() {
    for wire in [HttpWire::DeepSeekChat, HttpWire::MiMoChat] {
        let mut request = request();
        request.route.effective.provider = match wire {
            HttpWire::DeepSeekChat => "deepseek",
            HttpWire::MiMoChat => "xiaomi",
            _ => unreachable!(),
        }
        .into();
        let mut state = ChatState::new(wire);
        for part in ["first ", "second"] {
            let events = state.accept(&json!({"choices":[{"delta":{"reasoning_content":part},"finish_reason":null}]})).unwrap();
            assert_eq!(
                events,
                vec![ModelStreamEvent::ThinkingDelta {
                    block: 0,
                    text: part.into()
                }]
            );
        }
        state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"remote","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
        let assistant = state.complete(&request).unwrap().message;
        assert_eq!(
            assistant.provider_replay.as_ref().unwrap().data,
            "first second"
        );
        assert_eq!(
            assistant.content[0],
            Content::Thinking("first second".into())
        );
        request.messages.push(assistant);
        request.messages.push(Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "remote".into(),
                name: "read".into(),
                result: json!({"value":42}),
                images: Vec::new(),
                is_error: false,
            })],
            provider_replay: None,
        });
        request.messages.push(Message {
            role: Role::User,
            content: vec![Content::Text("continue".into())],
            provider_replay: None,
        });
        let body = chat_body(&request, wire).unwrap();
        assert_eq!(body["messages"][2]["reasoning_content"], "first second");
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "remote");
        assert_eq!(body["messages"][3]["tool_call_id"], "remote");
        let mut wrong_provider = request.clone();
        wrong_provider.route.effective.provider = "other".into();
        assert_eq!(
            chat_body(&wrong_provider, wire).unwrap_err().kind,
            ProviderErrorKind::Unsupported
        );
        request.messages[1].provider_replay.as_mut().unwrap().kind = "unknown".into();
        assert_eq!(
            chat_body(&request, wire).unwrap_err().kind,
            ProviderErrorKind::Unsupported
        );
    }
}

#[test]
fn openrouter_replays_fragmented_plain_details() {
    let mut request = request();
    request.route.effective.provider = "openrouter".into();
    let mut state = ChatState::new(HttpWire::OpenRouterChat);
    for text in ["plan ", "read"] {
        let events = state.accept(&json!({"choices":[{"delta":{"reasoning":text,"reasoning_details":[{"type":"reasoning.text","text":text,"format":"unknown","index":0}]},"finish_reason":null}]})).unwrap();
        assert_eq!(
            events,
            vec![ModelStreamEvent::ThinkingDelta {
                block: 0,
                text: text.into()
            }]
        );
    }
    state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"remote","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
    let assistant = state.complete(&request).unwrap().message;
    assert_eq!(
        assistant.provider_replay.as_ref().unwrap().data,
        json!([{"type":"reasoning.text","text":"plan read","format":"unknown","index":0}])
    );
    request.messages.push(assistant);
    request.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: "remote".into(),
            name: "read".into(),
            result: json!({"ok":true}),
            images: Vec::new(),
            is_error: false,
        })],
        provider_replay: None,
    });
    let body = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
    assert_eq!(
        body["messages"][2]["reasoning_details"],
        json!([{"type":"reasoning.text","text":"plan read","format":"unknown","index":0}])
    );
    assert!(body["messages"][2].get("reasoning").is_none());
    assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "remote");
    assert_eq!(body["messages"][3]["tool_call_id"], "remote");

    let mut contradictory = ChatState::new(HttpWire::OpenRouterChat);
    contradictory.accept(&json!({"choices":[{"delta":{"reasoning":"one","reasoning_details":[{"type":"reasoning.text","text":"two"}]},"finish_reason":"stop"}]})).unwrap();
    assert_eq!(
        contradictory.complete(&request).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
    let mut generic = ChatState::default();
    assert_eq!(
        generic
            .accept(&json!({"choices":[{"delta":{"reasoning":"hidden"},"finish_reason":null}]}))
            .unwrap_err()
            .kind,
        ProviderErrorKind::Unsupported
    );
}

#[test]
fn late_structured_thinking_keeps_summaries_without_duplicate_streams() {
    let mut state = ChatState::new(HttpWire::OpenRouterChat);
    let events = state
        .accept(
            &json!({"choices":[{"delta":{"reasoning":"plain thought"},"finish_reason":null}]}),
        )
        .unwrap();
    assert_eq!(
        events,
        vec![ModelStreamEvent::ThinkingDelta {
            block: 0,
            text: "plain thought".into()
        }]
    );
    let events = state.accept(&json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","text":"plain thought","signature":"opaque"},{"type":"reasoning.summary","summary":"readable summary"},{"type":"reasoning.encrypted","data":"encrypted"}],"content":"answer"},"finish_reason":"stop"}]})).unwrap();
    assert_eq!(
        events,
        vec![
            ModelStreamEvent::OutputObserved,
            ModelStreamEvent::TextDelta("answer".into())
        ]
    );
    assert_eq!(
        state.complete(&request()).unwrap().message.content,
        vec![
            Content::Thinking("plain thought".into()),
            Content::Thinking("readable summary".into()),
            Content::Text("answer".into())
        ]
    );
}

#[test]
fn openrouter_replays_plain_reasoning_without_details() {
    let mut request = request();
    request.route.effective.provider = "openrouter".into();
    let mut state = ChatState::new(HttpWire::OpenRouterChat);
    state
        .accept(
            &json!({"choices":[{"delta":{"reasoning":"inspect file"},"finish_reason":null}]}),
        )
        .unwrap();
    state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"remote","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
    let assistant = state.complete(&request).unwrap().message;
    assert_eq!(
        assistant.provider_replay.as_ref().unwrap().kind,
        OPENROUTER_PLAIN_REASONING_REPLAY
    );
    request.messages.push(assistant);
    request.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: "remote".into(),
            name: "read".into(),
            result: json!({"ok":true}),
            images: Vec::new(),
            is_error: false,
        })],
        provider_replay: None,
    });
    let body = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
    assert_eq!(body["messages"][2]["reasoning"], "inspect file");
    assert!(body["messages"][2].get("reasoning_details").is_none());
}

#[test]
fn openrouter_replays_signed_and_encrypted_details_in_order() {
    let mut request = request();
    request.route.effective.provider = "openrouter".into();
    let mut state = ChatState::new(HttpWire::OpenRouterChat);
    state.accept(&json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","text":"Need ","index":0}]},"finish_reason":null}]})).unwrap();
    state.accept(&json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","text":"read","index":0,"signature":"signed","format":"google-gemini-v1"},{"type":"reasoning.summary","summary":"First ","index":1}]},"finish_reason":null}]})).unwrap();
    state.accept(&json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.summary","summary":"inspect","index":1},{"type":"reasoning.encrypted","data":"opaque","format":"google-gemini-v1","id":"thought-1","index":2}]},"finish_reason":null}]})).unwrap();
    state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"provider-call","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
    let assistant = state.complete(&request).unwrap().message;
    assert_eq!(
        &assistant.content[..2],
        &[
            Content::Thinking("Need read".into()),
            Content::Thinking("First inspect".into())
        ]
    );
    let details = json!([
        {"type":"reasoning.text","text":"Need read","index":0,"signature":"signed","format":"google-gemini-v1"},
        {"type":"reasoning.summary","summary":"First inspect","index":1},
        {"type":"reasoning.encrypted","data":"opaque","format":"google-gemini-v1","id":"thought-1","index":2}
    ]);
    assert_eq!(assistant.provider_replay.as_ref().unwrap().data, details);
    request.messages.push(assistant);
    request.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: "provider-call".into(),
            name: "read".into(),
            result: json!({"ok":true}),
            images: Vec::new(),
            is_error: false,
        })],
        provider_replay: None,
    });
    let body = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
    assert_eq!(body["messages"][2]["reasoning_details"], details);
    assert!(body["messages"][2].get("reasoning").is_none());
    assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "provider-call");
    assert_eq!(body["messages"][3]["tool_call_id"], "provider-call");

    for kind in [
        "openrouter_reasoning_details",
        "openrouter_reasoning_details_tool_status",
    ] {
        let mut prior_encoding = request.clone();
        prior_encoding.messages[1]
            .provider_replay
            .as_mut()
            .unwrap()
            .kind = kind.into();
        assert_eq!(
            chat_body(&prior_encoding, HttpWire::OpenRouterChat)
                .unwrap_err()
                .kind,
            ProviderErrorKind::Unsupported
        );
    }
    let mut wrong_provider = request.clone();
    wrong_provider.route.effective.provider = "other".into();
    assert_eq!(
        chat_body(&wrong_provider, HttpWire::OpenRouterChat)
            .unwrap_err()
            .kind,
        ProviderErrorKind::Unsupported
    );
    request.messages[1].provider_replay.as_mut().unwrap().data =
        json!([{"type":"reasoning.encrypted"}]);
    assert_eq!(
        chat_body(&request, HttpWire::OpenRouterChat)
            .unwrap_err()
            .kind,
        ProviderErrorKind::InvalidRequest
    );
}

#[test]
fn openrouter_keeps_signature_only_text_detail() {
    let mut request = request();
    request.route.effective.provider = "openrouter".into();
    let mut state = ChatState::new(HttpWire::OpenRouterChat);
    let detail = json!({"type":"reasoning.text","format":"google-gemini-v1","index":0,"signature":"opaque"});
    state
        .accept(&json!({"choices":[{"delta":{"reasoning_details":[detail],"content":"Done"},"finish_reason":"stop"}]}))
        .unwrap();
    let assistant = state.complete(&request).unwrap().message;
    assert_eq!(
        assistant.provider_replay.as_ref().unwrap().data,
        json!([detail])
    );
    request.messages.push(assistant);
    request
        .messages
        .push(Message::user_input("Continue".into(), []));
    let body = chat_body(&request, HttpWire::OpenRouterChat).unwrap();
    assert_eq!(body["messages"][2]["reasoning_details"], json!([detail]));

    let mut invalid = ChatState::new(HttpWire::OpenRouterChat);
    assert_eq!(
        invalid
            .accept(&json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":0}]},"finish_reason":null}]}))
            .unwrap_err()
            .kind,
        ProviderErrorKind::InvalidRequest
    );
    assert!(!valid_openrouter_detail(&json!({
        "type":"reasoning.text","text":42,"signature":"opaque"
    })));
}

#[test]
fn chat_completion_needs_finish_and_valid_tool() {
    let request = request();
    let mut state = ChatState::default();
    state.accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
    assert_eq!(state.complete(&request).unwrap().message.content.len(), 1);
    let mut state = ChatState::default();
    state.accept(&json!({"choices":[]})).unwrap();
    assert!(state.complete(&request).is_err());

    let mut state = ChatState::default();
    state.accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"invented","function":{"name":"not_a_tool","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
    assert_eq!(
        state.complete(&request).unwrap().message.content,
        vec![Content::ToolCall(ToolCall {
            id: "invented".into(),
            name: "not_a_tool".into(),
            arguments: json!({}),
            raw_arguments: None,
        })]
    );

    let mut state = ChatState::default();
    state.accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"broken","function":{"name":"exec","arguments":"{\"command\":"}}]},"finish_reason":"tool_calls"}]})).unwrap();
    let response = state.complete(&request).unwrap();
    assert_eq!(
        response.message.content,
        vec![Content::ToolCall(ToolCall {
            id: "broken".into(),
            name: "exec".into(),
            arguments: json!({}),
            raw_arguments: Some("{\"command\":".into()),
        })]
    );
}

#[test]
fn chat_tool_metadata_is_stable_while_arguments_stream() {
    let request = request();
    let mut state = ChatState::default();
    state
        .accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-first","function":{"name":"read","arguments":"{\"path\":\""}}]},"finish_reason":null}]}))
        .unwrap();
    state
        .accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-first","function":{"name":"read","arguments":"README.md\"}"}}]},"finish_reason":"tool_calls"}]}))
        .unwrap();
    assert_eq!(
        state.complete(&request).unwrap().message.content,
        vec![Content::ToolCall(ToolCall {
            id: "call-first".into(),
            name: "read".into(),
            arguments: json!({"path":"README.md"}),
            raw_arguments: None,
        })]
    );

    for replacement in [
        json!({"index":0,"id":"different","function":{"name":"read"}}),
        json!({"index":0,"id":null,"function":{"name":"write"}}),
    ] {
        let mut state = ChatState::default();
        state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"first","function":{"name":"read","arguments":"{}"}}]}}]})).unwrap();
        assert!(
            state
                .accept(&json!({"choices":[{"delta":{"tool_calls":[replacement]}}]}))
                .is_err()
        );
    }

    let mut duplicate = ChatState::default();
    duplicate
        .accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[
        {"index":0,"id":"same","function":{"name":"read","arguments":"{}"}},
        {"index":1,"id":"same","function":{"name":"read","arguments":"{}"}}
    ]},"finish_reason":"tool_calls"}]}))
        .unwrap();
    assert!(duplicate.complete(&request).is_err());
}

#[test]
fn chat_rejects_mistyped_semantics_instead_of_discarding_them() {
    for choice in [
        json!({"index":"0","delta":{}}),
        json!({"index":-1,"delta":{}}),
        json!({"delta":[],"finish_reason":"tool_calls"}),
        json!({"delta":{},"finish_reason":42}),
        json!({"delta":{"content":42}}),
        json!({"delta":{"content":"answer","tool_calls":{}}}),
        json!({"delta":{"tool_calls":[{"index":0,"id":42}]}}),
        json!({"delta":{"tool_calls":[{"index":0,"function":[]}]}}),
        json!({"delta":{"tool_calls":[{"index":0,"function":{"name":42}}]}}),
        json!({"delta":{"tool_calls":[{"index":0,"function":{"arguments":42}}]}}),
        json!({"delta":{"tool_calls":[{"index":0,"type":42}]}}),
    ] {
        let mut state = ChatState::default();
        state.accept(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"first","function":{"name":"read","arguments":"{}"}}]}}]})).unwrap();
        assert!(
            state.accept(&json!({"choices":[choice.clone()]})).is_err(),
            "accepted {choice}"
        );
    }
}

#[test]
fn chat_tool_continuations_allow_omitted_null_and_empty_metadata() {
    let mut state = ChatState::default();
    for fragment in [
        json!({"index":0,"id":"first","type":"function","function":{"name":"read","arguments":"{"}}),
        json!({"index":0,"id":null,"type":null,"function":{"name":"","arguments":"\"path\":\"README.md\""}}),
        json!({"index":0,"id":"","function":{"name":null,"arguments":null}}),
        json!({"index":0,"function":{"arguments":"}"}}),
    ] {
        state.accept(&json!({"choices":[{"index":null,"delta":{"content":null,"tool_calls":[fragment]},"finish_reason":null}]})).unwrap();
    }
    state
        .accept(&json!({"choices":[{"delta":null,"finish_reason":"tool_calls"}]}))
        .unwrap();
    assert_eq!(
        state.complete(&request()).unwrap().message.content,
        vec![Content::ToolCall(ToolCall {
            id: "first".into(),
            name: "read".into(),
            arguments: json!({"path":"README.md"}),
            raw_arguments: None,
        })]
    );
}

#[test]
fn chat_length_stop_retains_calls_for_safe_rejection() {
    let request = request();
    let mut state = ChatState::default();
    state
        .accept(&json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"partial","function":{"name":"write","arguments":"{\"path\":\"short\",\"content\":\"hel\"}"}}]},"finish_reason":"length"}]}))
        .unwrap();
    let response = state.complete(&request).unwrap();
    assert_eq!(
        response.termination,
        ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
    );
    assert!(matches!(
        &response.message.content[0],
        Content::ToolCall(call) if call.id == "partial" && call.name == "write"
    ));
}

#[test]
fn chat_accepts_empty_openrouter_usage_chunk_after_finish() {
    let request = request();
    let mut state = ChatState::default();
    state.accept(&json!({"choices":[{"index":0,"delta":{"role":"assistant","content":"ok"},"finish_reason":null}]})).unwrap();
    state.accept(&json!({"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"stop"}]})).unwrap();
    state.accept(&json!({"usage":{"prompt_tokens":2,"completion_tokens":1},"choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":"stop"}]})).unwrap();
    assert_eq!(state.complete(&request).unwrap().usage, Usage::known(2, 1));

    let mut state = ChatState::default();
    state
        .accept(
            &json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}),
        )
        .unwrap();
    state
        .accept(&json!({"usage":{"prompt_tokens":2,"completion_tokens":1}}))
        .unwrap();
    assert_eq!(state.complete(&request).unwrap().usage, Usage::known(2, 1));

    let mut state = ChatState::default();
    state
        .accept(&json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop","usage":{"prompt_tokens":7,"completion_tokens":2}}]}))
        .unwrap();
    assert_eq!(state.complete(&request).unwrap().usage, Usage::known(7, 2));

    let mut state = ChatState::default();
    state
        .accept(&json!({"usage":{"prompt_tokens":8,"completion_tokens":3},"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop","usage":{"prompt_tokens":7,"completion_tokens":2}}]}))
        .unwrap();
    assert_eq!(state.complete(&request).unwrap().usage, Usage::known(8, 3));

    let mut state = ChatState::default();
    state
        .accept(&json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}))
        .unwrap();
    assert!(state.accept(&json!({"choices":[{"index":0,"delta":{"content":"late"},"finish_reason":"stop"}]})).is_err());
}

#[test]
fn chat_partial_usage_retains_previous_counts() {
    let request = request();
    let mut state = ChatState::default();
    state
        .accept(&json!({"usage":{"prompt_tokens":12,"completion_tokens":1}}))
        .unwrap();
    state
        .accept(&json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop","usage":{"completion_tokens":3}}]}))
        .unwrap();
    assert_eq!(state.complete(&request).unwrap().usage, Usage::known(12, 3));

    let mut state = ChatState::default();
    state
        .accept(&json!({"usage":{"prompt_tokens":12}}))
        .unwrap();
    state
        .accept(
            &json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}),
        )
        .unwrap();
    assert_eq!(
        state.complete(&request).unwrap().usage,
        Usage {
            input_tokens: Some(12),
            output_tokens: None,
            ..Usage::unknown()
        }
    );

    assert!(
        ChatState::default()
            .accept(&json!({"usage":{"prompt_tokens":"wrong"}}))
            .is_err()
    );
}

#[test]
fn sse_parsing_preserves_data_and_rejects_bad_utf8() {
    assert!(!has_sse_event_boundary(b"data: x\r\n"));
    for terminator in [
        b"\n\n".as_slice(),
        b"\r\n\r\n",
        b"\r\r",
        b"\r\n\n",
        b"\n\r\n",
        b"\r\r\n",
    ] {
        let mut frame = b"data: x".to_vec();
        frame.extend_from_slice(terminator);
        assert!(has_sse_event_boundary(&frame), "{terminator:?}");
    }
    let frame = parse_frame(b": ping\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n")
        .unwrap()
        .unwrap();
    assert_eq!(frame.data, "{\"a\":\n1}");
    let frame = parse_frame(b": ping\rdata: {\"a\":\rdata: 1}\r\r")
        .unwrap()
        .unwrap();
    assert_eq!(frame.data, "{\"a\":\n1}");
    assert!(parse_frame(b"data: \xff\n\n").is_err());
}

#[tokio::test]
async fn mimo_capacity_is_normalized_for_aliases_on_every_completion_path() {
    let frame = "data: {\"model\":\"served-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}],\"usage\":{\"prompt_tokens\":49900,\"completion_tokens\":0}}";
    for suffix in ["\n\ndata: [DONE]\n\n", "\n\ndata: [DONE]", ""] {
        let endpoint = serve(format!("{frame}{suffix}"), "200 OK").await;
        let model = HttpModelService::new_with_capabilities(
            &endpoint,
            HttpWire::MiMoChat,
            Arc::new(|| None),
            ModelCapabilities::conservative(),
            Some(50_000),
        )
        .unwrap();
        let mut request = request();
        request.route = ModelRoute::direct(
            ModelRef {
                provider: "renamed-mimo".into(),
                model: "requested-model".into(),
            },
            ModelRouteReason::UserRequest,
        );
        let mut stream = model.stream(request).await.unwrap();
        let mut completed = None;
        while let Some(event) = stream.next().await {
            if let ModelStreamEvent::Completed(response) = event.unwrap() {
                assert!(completed.is_none());
                completed = Some(response);
            }
        }
        let response = completed.unwrap();
        assert_eq!(
            response.termination,
            ResponseTermination::Incomplete(IncompleteReason::ContextLength),
            "{suffix:?}"
        );
        assert_eq!(response.usage, Usage::known(49_900, 0));
        assert_eq!(response.returned_model.as_deref(), Some("served-model"));
    }
}

#[test]
fn capacity_normalization_requires_the_mimo_wire_and_known_window() {
    for (wire, window, input, output, expected) in [
        (
            HttpWire::MiMoChat,
            Some(50_000),
            49_499,
            0,
            IncompleteReason::MaxOutputTokens,
        ),
        (
            HttpWire::MiMoChat,
            Some(50_000),
            49_500,
            0,
            IncompleteReason::ContextLength,
        ),
        (
            HttpWire::MiMoChat,
            Some(50_000),
            u64::MAX,
            0,
            IncompleteReason::ContextLength,
        ),
        (
            HttpWire::MiMoChat,
            Some(50_000),
            49_900,
            1,
            IncompleteReason::MaxOutputTokens,
        ),
        (
            HttpWire::MiMoChat,
            None,
            49_900,
            0,
            IncompleteReason::MaxOutputTokens,
        ),
        (
            HttpWire::ChatCompletions,
            Some(50_000),
            49_900,
            0,
            IncompleteReason::MaxOutputTokens,
        ),
    ] {
        let event = ModelStreamEvent::Completed(ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: Vec::new(),
                provider_replay: None,
            },
            usage: Usage::known(input, output),
            termination: ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens),
            returned_model: None,
        });
        let ModelStreamEvent::Completed(response) = normalize_capacity(event, wire, window)
        else {
            unreachable!();
        };
        assert_eq!(
            response.termination,
            ResponseTermination::Incomplete(expected)
        );
    }
}

#[tokio::test]
async fn partial_tool_and_opaque_output_stop_retries_without_admitting_calls() {
    use ion_core::{Agent, AgentEvent, Session, ToolSet};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let start = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"type\":\"message\",\"model\":\"test-model\",\"role\":\"assistant\",\"content\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\n";
    for (wire, body) in [
        (HttpWire::ChatCompletions, "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"partial\",\"function\":{\"name\":\"read\",\"arguments\":\"{\"}}]}}]}\n\n".to_owned()),
        (HttpWire::OpenRouterChat, "data: {\"choices\":[{\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.encrypted\",\"data\":\"PRIVATE_OPAQUE\"}]}}]}\n\n".to_owned()),
        (HttpWire::AnthropicMessages, format!("{start}event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"redacted_thinking\",\"data\":\"PRIVATE_OPAQUE\"}}}}\n\n")),
        (HttpWire::AnthropicMessages, format!("{start}event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"thinking\",\"thinking\":\"\",\"signature\":\"PRIVATE_OPAQUE\"}}}}\n\n")),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                count.fetch_add(1, Ordering::SeqCst);
                read_request_body(&mut socket).await;
                let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let root = std::env::temp_dir().join(format!("ion-partial-wire-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = Arc::new(Session::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let model = Arc::new(HttpModelService::new(&endpoint, wire, Arc::new(|| None)).unwrap());
        let agent = Agent::with_tool_set(model, Arc::new(ToolSet::new([])), request().route.effective);
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = events.clone();
        let result = agent.submit(session.clone(), "work".into(), "test".into(), CancellationToken::new(), move |event| observed.lock().unwrap().push(event)).await;
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        let events = events.lock().unwrap();
        assert!(result.is_err(), "{wire:?}");
        assert_eq!(requests.load(Ordering::SeqCst), 1, "{wire:?}");
        assert!(events.iter().any(|event| matches!(event, AgentEvent::ModelOutputObserved)), "{wire:?}");
        assert!(!events.iter().any(|event| matches!(event, AgentEvent::ProviderRetry { .. } | AgentEvent::ToolStarted { .. } | AgentEvent::AssistantCommitted { .. })));
        assert!(!format!("{events:?}").contains("PRIVATE_OPAQUE"));
        drop(session);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn chat_stream_accepts_cr_and_mixed_sse_line_endings() {
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\r\r",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\n",
        "data: [DONE]\n\r\n"
    );
    let endpoint = serve_split(body.into(), "200 OK", 1).await;
    let model =
        HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
    let mut stream = model.stream(request()).await.unwrap();
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelStreamEvent::TextDelta(text) if text == "hi"
    ));
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelStreamEvent::Completed(_)
    ));
}

#[tokio::test]
async fn chat_stream_accepts_eof_after_finish_and_reports_usage() {
    let body = concat!(
        "data: {\"model\":\"returned\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n"
    );
    let endpoint = serve(body.into(), "200 OK").await;
    let model =
        HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
    let mut stream = model.stream(request()).await.unwrap();
    let mut completed = None;
    while let Some(event) = stream.next().await {
        if let ModelStreamEvent::Completed(response) = event.unwrap() {
            completed = Some(response);
        }
    }
    let response = completed.unwrap();
    assert_eq!(response.returned_model.as_deref(), Some("returned"));
    assert_eq!(response.usage, Usage::known(3, 2));
    assert_eq!(response.message.content, vec![Content::Text("hi".into())]);

    let endpoint = serve(body.replace("data: [DONE]\n\n", ""), "200 OK").await;
    let model =
        HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
    let mut stream = model.stream(request()).await.unwrap();
    let mut completed = false;
    while let Some(event) = stream.next().await {
        if matches!(event.unwrap(), ModelStreamEvent::Completed(_)) {
            completed = true;
        }
    }
    assert!(completed);

    let endpoint = serve("data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n".into(), "200 OK").await;
    let model =
        HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
    let mut stream = model.stream(request()).await.unwrap();
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelStreamEvent::TextDelta(_)
    ));
    assert_eq!(
        stream.next().await.unwrap().unwrap_err().kind,
        ProviderErrorKind::Transport
    );

    let endpoint = serve(
        "data: {\"choices\":[{\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}"
            .into(),
        "200 OK",
    )
    .await;
    let model =
        HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
    let mut stream = model.stream(request()).await.unwrap();
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelStreamEvent::TextDelta(_)
    ));
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelStreamEvent::Completed(_)
    ));
}

#[tokio::test]
async fn initial_sse_bom_preserves_the_first_delta_across_chunks() {
    let body = concat!(
        "\u{feff}data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"prefix \"}}]}\n\n",
        "\u{feff}data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"must_ignore\"}}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\u{feff}\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    for chunk_size in [1, 2, usize::MAX] {
        let endpoint = serve_split(body.into(), "200 OK", chunk_size).await;
        let model =
            HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None))
                .unwrap();
        let mut stream = model.stream(request()).await.unwrap();
        let mut deltas = String::new();
        let mut completed = None;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                ModelStreamEvent::TextDelta(text) => deltas.push_str(&text),
                ModelStreamEvent::Completed(response) => completed = Some(response),
                _ => {}
            }
        }
        assert_eq!(deltas, "prefix answer\u{feff}");
        assert_eq!(
            completed.unwrap().message.content,
            vec![Content::Text("prefix answer\u{feff}".into())]
        );
    }
}

#[test]
fn numeric_stream_status_keeps_context_and_quota_semantics() {
    for (envelope, expected) in [
        (
            json!({"error":{"code":400,"type":"context_length_exceeded","message":"too large"}}),
            ProviderErrorKind::ContextLength,
        ),
        (
            json!({"error":{"code":413,"message":"Prompt is too long"}}),
            ProviderErrorKind::ContextLength,
        ),
        (
            json!({"error":{"code":429,"type":"insufficient_quota","message":"quota used"}}),
            ProviderErrorKind::Quota,
        ),
    ] {
        let failure = ChatState::default().accept(&envelope).unwrap_err();
        assert_eq!(failure.kind, expected);
    }
}

#[tokio::test]
async fn padded_http_quota_body_keeps_the_bounded_prefix_across_chunks() {
    let body = format!(
        "{{\"error\":{{\"code\":\"insufficient_quota\",\"message\":\"quota used\"}}}}{}",
        " ".repeat(64 * 1024)
    );
    for chunk_size in [1024, usize::MAX] {
        let endpoint = serve_split(body.clone(), "429 Too Many Requests", chunk_size).await;
        let model =
            HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None))
                .unwrap();
        let failure = match model.stream(request()).await {
            Ok(_) => panic!("HTTP quota error opened a model stream"),
            Err(error) => error,
        };
        assert_eq!(failure.kind, ProviderErrorKind::Quota);
        assert!(failure.message.contains("quota used"));
    }
}

#[tokio::test]
async fn chat_stream_error_preserves_bounded_reason_and_never_completes() {
    let body = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
        "data: {\"error\":{\"code\":\"server_error\",\"message\":\"upstream disconnected\\nwhile generating\"},\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"error\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    let endpoint = serve(body.into(), "200 OK").await;
    let model =
        HttpModelService::new(&endpoint, HttpWire::OpenRouterChat, Arc::new(|| None)).unwrap();
    let mut stream = model.stream(request()).await.unwrap();
    assert!(matches!(
        stream.next().await.unwrap().unwrap(),
        ModelStreamEvent::TextDelta(_)
    ));
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::Server);
    assert!(
        error
            .message
            .contains("upstream disconnectedwhile generating")
    );
    assert!(!error.message.contains('\n'));

    let rate_limit = json!({"error":{"code":429,"message":"slow down"},"choices":[{"finish_reason":"error"}]});
    let mut state = ChatState::default();
    let error = state.accept(&rate_limit).unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::RateLimited);
    assert!(error.message.contains("slow down"));

    let long = json!({"error":{"message":"x".repeat(1000)}});
    let error = state.accept(&long).unwrap_err();
    assert_eq!(error.message.len(), "provider stream error: ".len() + 500);
}

#[tokio::test]
async fn anthropic_stream_closes_tool_block_before_completion() {
    let body = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"returned\",\"content\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool_1\",\"name\":\"read\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"x\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":5}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let endpoint = serve(body.into(), "200 OK").await;
    let model =
        HttpModelService::new(&endpoint, HttpWire::AnthropicMessages, Arc::new(|| None))
            .unwrap();
    let mut stream = model.stream(request()).await.unwrap();
    let mut completed = None;
    while let Some(event) = stream.next().await {
        if let ModelStreamEvent::Completed(response) = event.unwrap() {
            completed = Some(response);
        }
    }
    let response = completed.unwrap();
    assert_eq!(response.usage, Usage::known(3, 5));
    assert_eq!(
        response.message.content,
        vec![Content::ToolCall(ToolCall {
            id: "tool_1".into(),
            name: "read".into(),
            arguments: json!({"path":"x"}),
            raw_arguments: None,
        })]
    );

    let endpoint = serve_split(body.replace('\n', "\r"), "200 OK", 1).await;
    let model =
        HttpModelService::new(&endpoint, HttpWire::AnthropicMessages, Arc::new(|| None))
            .unwrap();
    let mut stream = model.stream(request()).await.unwrap();
    let mut completed = None;
    while let Some(event) = stream.next().await {
        if let ModelStreamEvent::Completed(response) = event.unwrap() {
            completed = Some(response);
        }
    }
    assert_eq!(completed.unwrap().message.content, response.message.content);
}

#[test]
fn anthropic_projects_only_human_initial_and_streamed_thinking() {
    let request = request();
    let mut state = anthropic_state(&request);
    let mut projected = Vec::new();
    let mut response = None;
    for event in [
        json!({"type":"message_start","message":{"type":"message","role":"assistant","model":"returned","content":[],"stop_reason":null,"usage":{"input_tokens":3,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"initial ","signature":"SIGNATURE_PRIVATE"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"thought"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"CHUNK_PRIVATE"}}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"ENCRYPTED_PRIVATE"}}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"thinking","thinking":"summary","signature":"OTHER_SIGNATURE"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":" tail"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"content_block_start","index":3,"content_block":{"type":"text","text":"answer"}}),
        json!({"type":"content_block_stop","index":3}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ] {
        for event in state.accept(&event, &request).unwrap() {
            match event {
                ModelStreamEvent::ThinkingDelta { block, text } => {
                    projected.push((block, text))
                }
                ModelStreamEvent::Completed(value) => response = Some(value),
                _ => {}
            }
        }
    }
    assert_eq!(
        projected,
        vec![
            (0, "initial ".into()),
            (0, "thought".into()),
            (1, "summary".into()),
            (0, " tail".into()),
        ]
    );
    let message = response.unwrap().message;
    assert_eq!(
        message.content,
        vec![
            Content::Thinking("initial thought tail".into()),
            Content::Thinking("summary".into()),
            Content::Text("answer".into())
        ]
    );
    let replay = message.provider_replay.unwrap();
    assert_eq!(
        replay.data["blocks"][0]["signature"],
        "SIGNATURE_PRIVATECHUNK_PRIVATE"
    );
    assert_eq!(replay.data["blocks"][1]["data"], "ENCRYPTED_PRIVATE");
}

#[test]
fn anthropic_plain_replay_checks_projection_without_binding_the_prefix() {
    let mut request = request();
    request.instructions = Some("different context".into());
    let blocks = json!([{"type":"text","text":"answer"}]);
    request.messages.push(Message {
        role: Role::Assistant,
        content: vec![Content::Text("answer".into())],
        provider_replay: Some(ProviderReplay::new(
            &request.route.effective.provider,
            ANTHROPIC_CONTENT_REPLAY,
            json!({"blocks":blocks, "prefix_sha256":"0".repeat(64)}),
        )),
    });
    request
        .messages
        .push(Message::user_input("next".into(), []));
    assert_eq!(
        anthropic_body(&request, true).unwrap()["messages"][1]["content"],
        blocks
    );

    request.messages[1].content[0] = Content::Text("edited".into());
    assert_eq!(
        anthropic_body(&request, true).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
    request.messages[1].content[0] = Content::Text("answer".into());
    request.messages[1].provider_replay.as_mut().unwrap().data["prefix_sha256"] =
        json!("invalid");
    assert_eq!(
        anthropic_body(&request, true).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
}

#[test]
fn anthropic_signed_blocks_survive_tool_continuation_and_reopen() {
    let mut request = request();
    let mut state = anthropic_state(&request);
    let events = [
        json!({"type":"message_start","message":{"type":"message","role":"assistant",
            "model":"returned","content":[],"stop_reason":null,
            "usage":{"input_tokens":3,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,
            "content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,
            "delta":{"type":"thinking_delta","thinking":""}}),
        json!({"type":"content_block_delta","index":0,
            "delta":{"type":"signature_delta","signature":"sig"}}),
        json!({"type":"content_block_delta","index":0,
            "delta":{"type":"signature_delta","signature":"nature"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,
            "content_block":{"type":"redacted_thinking","data":"opaque"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,
            "content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"content_block_start","index":3,
            "content_block":{"type":"tool_use","id":"tool_1","name":"read","input":{}}}),
        json!({"type":"content_block_delta","index":3,
            "delta":{"type":"input_json_delta","partial_json":"{\"path\":\"x\"}"}}),
        json!({"type":"content_block_stop","index":3}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},
            "usage":{"output_tokens":7}}),
        json!({"type":"message_stop"}),
    ];
    let mut completed = None;
    for event in events {
        for output in state.accept(&event, &request).unwrap() {
            if let ModelStreamEvent::Completed(response) = output {
                completed = Some(response);
            }
        }
    }
    let assistant = completed.unwrap().message;
    assert_eq!(assistant.content.len(), 2);
    assert!(matches!(&assistant.content[0], Content::Text(text) if text.is_empty()));
    let persisted: Message =
        serde_json::from_slice(&serde_json::to_vec(&assistant).unwrap()).unwrap();
    assert!(persisted.provider_replay.as_ref().unwrap().prefix_bound);
    request.messages.push(persisted.clone());
    request.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ion_ai::ToolResult {
            call_id: "tool_1".into(),
            name: "read".into(),
            result: json!({"text":"ok"}),
            images: Vec::new(),
            is_error: false,
        })],
        provider_replay: None,
    });
    let continuation = anthropic_body(&request, true).unwrap();
    let blocks = continuation["messages"][1]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 4);
    assert_eq!(
        blocks[0],
        json!({"type":"thinking","thinking":"","signature":"signature"})
    );
    assert_eq!(
        blocks[1],
        json!({"type":"redacted_thinking","data":"opaque"})
    );
    assert_eq!(blocks[2], json!({"type":"text","text":""}));
    assert_eq!(blocks[3]["id"], "tool_1");
    assert_eq!(
        continuation["messages"][2]["content"][0]["tool_use_id"],
        "tool_1"
    );

    request
        .messages
        .push(Message::user_input("next".into(), []));
    assert_eq!(
        anthropic_body(&request, true).unwrap()["messages"][1]["content"],
        Value::Array(blocks.clone())
    );
    let mut changed_prefix = request.clone();
    changed_prefix.instructions = Some("new instructions".into());
    assert_eq!(
        anthropic_body(&changed_prefix, true).unwrap_err().kind,
        ProviderErrorKind::ReplayContextChanged
    );
    changed_prefix = request.clone();
    changed_prefix.messages[0].content[0] = Content::Text("rewritten".into());
    assert_eq!(
        anthropic_body(&changed_prefix, true).unwrap_err().kind,
        ProviderErrorKind::ReplayContextChanged
    );
    let mut changed = request.clone();
    changed.messages[1].content[0] = Content::Text("edited".into());
    assert_eq!(
        anthropic_body(&changed, true).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
    let mut corrupt = request;
    corrupt.messages[1].provider_replay.as_mut().unwrap().data["blocks"][0]["signature"] =
        json!("");
    assert_eq!(
        anthropic_body(&corrupt, true).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
}

#[test]
fn anthropic_unsigned_thinking_is_rejected_before_completion() {
    let request = request();
    let mut state = anthropic_state(&request);
    for event in [
        json!({"type":"message_start","message":{"type":"message","role":"assistant",
            "model":"returned","content":[],"stop_reason":null,
            "usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,
            "content_block":{"type":"thinking","thinking":"","signature":""}}),
    ] {
        state.accept(&event, &request).unwrap();
    }
    assert_eq!(
        state
            .accept(&json!({"type":"content_block_stop","index":0}), &request)
            .unwrap_err()
            .kind,
        ProviderErrorKind::InvalidRequest
    );
}

#[test]
fn signed_anthropic_tool_with_invalid_json_does_not_enter_history() {
    let request = request();
    let mut state = anthropic_state(&request);
    for event in [
        json!({"type":"message_start","message":{"type":"message","role":"assistant",
            "model":"returned","content":[],"stop_reason":null,
            "usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,
            "content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,
            "delta":{"type":"signature_delta","signature":"signed"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,
            "content_block":{"type":"tool_use","id":"tool_1","name":"read","input":{}}}),
        json!({"type":"content_block_delta","index":1,
            "delta":{"type":"input_json_delta","partial_json":"{"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},
            "usage":{"output_tokens":3}}),
    ] {
        state.accept(&event, &request).unwrap();
    }
    assert_eq!(
        state
            .accept(&json!({"type":"message_stop"}), &request)
            .unwrap_err()
            .kind,
        ProviderErrorKind::InvalidRequest
    );
}

#[test]
fn current_claude_enforces_prefix_binding_only_on_native_api() {
    let mut request = request();
    request.route.effective.provider = "anthropic".into();
    request.route.effective.model = "claude-sonnet-5-5".into();
    assert_eq!(
        anthropic_body(&request, true).unwrap()["thinking"],
        json!({"type":"adaptive","block_binding":{"prefix_mismatch_behavior":"error"}})
    );
    assert!(
        anthropic_body(&request, false)
            .unwrap()
            .get("thinking")
            .is_none()
    );
    request.controls.tool_choice = ToolChoice::Required;
    assert_eq!(
        anthropic_body(&request, true).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
    request.controls.tool_choice = ToolChoice::Auto;
    request.route.effective.model = "claude-sonnet-4-6".into();
    assert!(
        anthropic_body(&request, true)
            .unwrap()
            .get("thinking")
            .is_none()
    );
}

#[test]
fn anthropic_reports_known_replay_transformations_without_unknown_entries() {
    let notices = anthropic_replay_notices(Some(&json!([
        {"type":"thinking_dropped","reason":"model_binding_mismatch","path":"messages.1.content.0"},
        {"type":"thinking_dropped","reason":"model_binding_mismatch","path":"messages.3.content.0"},
        {"type":"thinking_dropped","reason":"organization_binding_mismatch","path":"messages.5.content.0"},
        {"type":"future_type","reason":"future_reason"}
    ])));
    assert_eq!(notices.len(), 2);
    assert!(notices.iter().any(|notice| matches!(notice,
        ModelStreamEvent::ProviderReplayNotice { action, reason, count: 2 }
        if action == "thinking_dropped" && reason == "model_binding_mismatch"
    )));
    assert!(notices.iter().any(|notice| matches!(notice,
        ModelStreamEvent::ProviderReplayNotice { action, reason, count: 1 }
        if action == "thinking_dropped" && reason == "organization_binding_mismatch"
    )));
}

#[tokio::test]
async fn anthropic_stream_accepts_multiple_message_deltas() {
    let body = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"type\":\"message\",\"role\":\"assistant\",\"model\":\"returned\",\"content\":[],\"stop_reason\":null,\"usage\":{\"input_tokens\":3,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"ok\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":null},\"usage\":{\"output_tokens\":1}}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let endpoint = serve_split(body.into(), "200 OK", 1).await;
    let model =
        HttpModelService::new(&endpoint, HttpWire::AnthropicMessages, Arc::new(|| None))
            .unwrap();
    let mut stream = model.stream(request()).await.unwrap();
    let mut completed = None;
    while let Some(event) = stream.next().await {
        if let ModelStreamEvent::Completed(response) = event.unwrap() {
            completed = Some(response);
        }
    }
    let response = completed.unwrap();
    assert_eq!(response.usage, Usage::known(3, 2));
    assert_eq!(response.message.content, vec![Content::Text("ok".into())]);
}

#[test]
fn anthropic_partial_usage_delta_keeps_previous_counts() {
    let request = request();
    let mut state = anthropic_state(&request);
    state
        .accept(&json!({"type":"message_start","message":{"type":"message","role":"assistant","model":"returned","content":[],"stop_reason":null,"usage":{"input_tokens":3,"output_tokens":0}}}), &request)
        .unwrap();
    state
        .accept(&json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"ok"}}), &request)
        .unwrap();
    state
        .accept(&json!({"type":"content_block_stop","index":0}), &request)
        .unwrap();
    state
        .accept(&json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}), &request)
        .unwrap();
    assert_eq!(state.usage().unwrap(), Usage::known(3, 2));
    let completed = state
        .accept(&json!({"type":"message_stop"}), &request)
        .unwrap();
    assert!(
        matches!(&completed[0], ModelStreamEvent::Completed(response) if response.usage == Usage::known(3, 2))
    );

    let mut state = anthropic_state(&request);
    state
        .accept(&json!({"type":"message_start","message":{"type":"message","role":"assistant","model":"returned","content":[],"stop_reason":null,"usage":{"input_tokens":3,"output_tokens":0}}}), &request)
        .unwrap();
    state
        .accept(
            &json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
            &request,
        )
        .unwrap();
    assert_eq!(state.usage().unwrap(), Usage::known(3, 0));
}

#[test]
fn anthropic_multiple_message_deltas_need_consistent_stop_reason() {
    let request = request();
    let mut state = anthropic_state(&request);
    state
        .accept(&json!({"type":"message_start","message":{"type":"message","role":"assistant","model":"returned","content":[],"stop_reason":null,"usage":{"input_tokens":3,"output_tokens":0}}}), &request)
        .unwrap();
    state
        .accept(
            &json!({"type":"message_delta","delta":{},"usage":{"output_tokens":1}}),
            &request,
        )
        .unwrap();
    assert!(
        state
            .accept(&json!({"type":"message_stop"}), &request)
            .is_err()
    );
    state
        .accept(
            &json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),
            &request,
        )
        .unwrap();
    assert!(
        state
            .accept(
                &json!({"type":"message_delta","delta":{"stop_reason":"tool_use"}}),
                &request
            )
            .is_err()
    );
}
