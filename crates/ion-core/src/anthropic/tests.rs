use super::streaming::{MAX_FRAME, MAX_RESPONSE, make_stream};
use super::*;
use futures_util::{StreamExt, stream};
use ion_ai::{Content, IncompleteReason, ModelResponse, ModelStreamEvent, ResponseTermination};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

pub(super) fn identity(realm: String) -> ModelBoundaryIdentity {
    ModelBoundaryIdentity {
        binding: crate::ProviderBindingId::new("anthropic-test").unwrap(),
        adapter: crate::SemanticCompatibilityId::new("anthropic-messages-v1").unwrap(),
        request_encoding: crate::SemanticCompatibilityId::new("anthropic-messages-v1").unwrap(),
        egress: EgressRealm::Remote(realm),
    }
}
fn request() -> SemanticRequest {
    SemanticRequest {
        provider: crate::ProviderBindingId::new("anthropic-test").unwrap(),
        model: ion_ai::ModelRef {
            provider: "anthropic".into(),
            model: "claude-test".into(),
        },
        instructions: "rules".into(),
        messages: vec![crate::TranscriptMessage::user_text("hello")],
        tools: vec![],
        controls: ion_ai::GenerationControls {
            max_output_tokens: 1024,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: false,
        },
    }
}
fn spec() -> ion_ai::ToolSpec {
    ion_ai::ToolSpec {
        name: "read".into(),
        description: "Read a file".into(),
        input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
    }
}
pub(super) fn start() -> Value {
    json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude-test","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":1}}})
}
pub(super) fn block(index: usize, content: Value) -> Value {
    json!({"type":"content_block_start","index":index,"content_block":content})
}
pub(super) fn delta(index: usize, delta: Value) -> Value {
    json!({"type":"content_block_delta","index":index,"delta":delta})
}
pub(super) fn block_stop(index: usize) -> Value {
    json!({"type":"content_block_stop","index":index})
}
pub(super) fn finish(reason: &str, output: u64) -> Value {
    json!({"type":"message_delta","delta":{"stop_reason":reason,"stop_sequence":null},"usage":{"output_tokens":output}})
}
pub(super) fn stop() -> Value {
    json!({"type":"message_stop"})
}
pub(super) fn text_response() -> Vec<Value> {
    vec![
        start(),
        block(0, json!({"type":"text","text":""})),
        delta(0, json!({"type":"text_delta","text":"read confirmed"})),
        block_stop(0),
        finish("end_turn", 3),
        stop(),
    ]
}
pub(super) fn sse(values: &[Value]) -> String {
    values
        .iter()
        .map(|v| format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap()))
        .collect()
}
async fn events(bytes: Vec<u8>, chunk_size: usize) -> Vec<Result<ModelStreamEvent, ProviderError>> {
    let chunks = bytes
        .chunks(chunk_size)
        .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
        .collect::<Vec<_>>();
    make_stream(stream::iter(chunks), CancellationToken::new())
        .collect()
        .await
}
async fn completed(values: &[Value]) -> ModelResponse {
    let events = events(sse(values).into_bytes(), 7).await;
    assert!(events.iter().all(Result::is_ok), "{events:?}");
    let Some(Ok(ModelStreamEvent::Completed(response))) = events.last() else {
        panic!("no completion: {events:?}")
    };
    response.clone()
}
async fn fails(values: &[Value]) {
    let events = events(sse(values).into_bytes(), 19).await;
    assert!(events.iter().any(Result::is_err), "{values:?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Ok(ModelStreamEvent::Completed(_)))),
        "{values:?}"
    );
    assert_eq!(events.iter().filter(|e| e.is_err()).count(), 1);
}

#[tokio::test]
async fn text_fragmentation_crlf_multiline_data_and_cumulative_usage() {
    let mut values = text_response();
    values[2]["delta"]["text"] = json!("read confirmed λ🦀");
    values[0]["message"]["usage"]["cache_creation_input_tokens"] = json!(2);
    values[0]["message"]["usage"]["cache_read_input_tokens"] = json!(7);
    values.insert(2, json!({"type":"ping"}));
    values.insert(
        values.len() - 1,
        json!({"type":"message_delta","delta":{},"usage":{"output_tokens":8}}),
    );
    let wire = sse(&values)
        .replace("\n", "\r\n")
        .replace("\"delta\":", "\r\ndata: \"delta\":");
    let mut data = b":comment\r\n\r\n".to_vec();
    data.extend_from_slice(wire.as_bytes());
    for size in [1, 2, 7, data.len()] {
        let events = events(data.clone(), size).await;
        let mut usage = Vec::new();
        for event in events {
            match event.unwrap() {
                ModelStreamEvent::Usage(u) => usage.push(u),
                ModelStreamEvent::TextDelta(s) => assert_eq!(s, "read confirmed λ🦀"),
                ModelStreamEvent::Completed(response) => {
                    assert_eq!(response.usage, Usage::known(14, 8));
                    assert_eq!(response.returned_model.as_deref(), Some("claude-test"));
                    assert_eq!(
                        response.message.content,
                        vec![Content::Text("read confirmed λ🦀".into())]
                    );
                    assert!(response.is_complete());
                }
                _ => panic!("unexpected event"),
            }
        }
        assert_eq!(
            usage,
            vec![
                Usage::known(14, 1),
                Usage::known(14, 3),
                Usage::known(14, 8)
            ]
        );
    }
}

#[tokio::test]
async fn indexed_tools_and_text_preserve_source_order_and_whole_json_fragments() {
    let values = vec![
        start(),
        block(0, json!({"type":"text","text":"before"})),
        block_stop(0),
        block(
            1,
            json!({"type":"tool_use","id":"call_1","name":"read","input":{}}),
        ),
        block(
            2,
            json!({"type":"tool_use","id":"call_2","name":"read","input":{"path":"b"}}),
        ),
        delta(
            1,
            json!({"type":"input_json_delta","partial_json":"{\"path\":"}),
        ),
        block_stop(2),
        delta(
            1,
            json!({"type":"input_json_delta","partial_json":"\"a\"}"}),
        ),
        block_stop(1),
        block(3, json!({"type":"text","text":"after"})),
        block_stop(3),
        finish("tool_use", 8),
        stop(),
    ];
    let response = completed(&values).await;
    assert_eq!(response.message.content.len(), 4);
    assert_eq!(response.message.content[0], Content::Text("before".into()));
    assert!(
        matches!(&response.message.content[1], Content::ToolCall(c) if c.id == "call_1" && c.arguments == json!({"path":"a"}))
    );
    assert!(
        matches!(&response.message.content[2], Content::ToolCall(c) if c.id == "call_2" && c.arguments == json!({"path":"b"}))
    );
    assert_eq!(response.message.content[3], Content::Text("after".into()));
    let empty = completed(&[
        start(),
        block(
            0,
            json!({"type":"tool_use","id":"empty","name":"read","input":{}}),
        ),
        block_stop(0),
        finish("tool_use", 3),
        stop(),
    ])
    .await;
    assert!(matches!(&empty.message.content[0], Content::ToolCall(c) if c.arguments == json!({})));
}

#[tokio::test]
async fn incomplete_reasons_are_not_success() {
    for (reason, expected) in [
        ("max_tokens", IncompleteReason::MaxOutputTokens),
        ("refusal", IncompleteReason::ContentFilter),
        (
            "model_context_window_exceeded",
            IncompleteReason::ContextLength,
        ),
    ] {
        let mut values = text_response();
        values[4] = finish(reason, 3);
        assert_eq!(
            completed(&values).await.termination,
            ResponseTermination::Incomplete(expected)
        );
    }
}

#[tokio::test]
async fn rejects_invalid_event_order_model_and_terminal_semantics() {
    let good = text_response();
    let mut cases = vec![vec![], vec![stop()], good[1..].to_vec(), good[..5].to_vec()];
    for index in 1..good.len() {
        let mut duplicate = good.clone();
        duplicate.insert(index, start());
        cases.push(duplicate);
    }
    for index in 1..good.len() {
        let mut changed = good.clone();
        changed[index]["model"] = json!("changed");
        cases.push(changed);
    }
    for extra in [
        finish("end_turn", 3),
        finish("max_tokens", 3),
        json!({"type":"message_delta","delta":{"stop_reason":null},"usage":{"output_tokens":3}}),
        json!({"type":"error","error":{"message":"secret"}}),
        block(1, json!({"type":"text","text":"late"})),
        delta(0, json!({"type":"text_delta","text":"late"})),
        block_stop(0),
    ] {
        let mut late = good.clone();
        late.insert(5, extra);
        cases.push(late);
    }
    let mut no_stop_reason = good.clone();
    no_stop_reason[4]["delta"]["stop_reason"] = Value::Null;
    cases.push(no_stop_reason);
    let mut top_stop = good.clone();
    top_stop[2]["stop_reason"] = json!("end_turn");
    cases.push(top_stop);
    let mut terminal_stop = good.clone();
    terminal_stop[5]["delta"] = json!({"stop_reason":"max_tokens"});
    cases.push(terminal_stop);
    let mut initial_stop = good.clone();
    initial_stop[0]["message"]["stop_reason"] = json!("end_turn");
    cases.push(initial_stop);
    let mut delta_model = good.clone();
    delta_model[4]["delta"]["model"] = json!("changed");
    cases.push(delta_model);
    for reason in ["tool_use", "unknown", "stop_sequence", "pause_turn"] {
        let mut bad = good.clone();
        bad[4] = finish(reason, 3);
        cases.push(bad);
    }
    let mut missing_stop = good.clone();
    missing_stop.remove(3);
    cases.push(missing_stop);
    for index in [1, usize::MAX] {
        let mut bad = good.clone();
        bad[1]["index"] = json!(index);
        cases.push(bad);
    }
    let mut extra_delta = good.clone();
    extra_delta.insert(4, delta(0, json!({"type":"text_delta","text":"closed"})));
    cases.push(extra_delta);
    for mut bad in [start(), start(), start(), start()].into_iter().enumerate() {
        match bad.0 {
            0 => bad.1["message"]["model"] = Value::Null,
            1 => bad.1["message"]["content"] = json!([{"type":"text","text":"lost"}]),
            2 => bad.1["message"]["role"] = json!("user"),
            _ => bad.1["message"]["usage"] = Value::Null,
        }
        cases.push(vec![bad.1, finish("end_turn", 3), stop()]);
    }
    for case in cases {
        fails(&case).await;
    }
}

#[tokio::test]
async fn rejects_semantic_content_loss_and_invalid_tools() {
    for kind in [
        "thinking",
        "redacted_thinking",
        "server_tool_use",
        "future_block",
    ] {
        fails(&[
            start(),
            block(0, json!({"type":kind,"text":"lost"})),
            block_stop(0),
            finish("end_turn", 3),
            stop(),
        ])
        .await;
    }
    for kind in [
        "thinking_delta",
        "signature_delta",
        "citations_delta",
        "input_json_delta",
        "future_delta",
    ] {
        fails(&[
            start(),
            block(0, json!({"type":"text","text":""})),
            delta(0, json!({"type":kind,"text":"lost"})),
            block_stop(0),
            finish("end_turn", 3),
            stop(),
        ])
        .await;
    }
    for (initial, fragment) in [
        (json!({"a":1}), "{\"b\":2}"),
        (json!({}), "[]"),
        (json!({}), "null"),
        (json!({}), "{\"a\":"),
        (json!({}), ""),
        (json!([]), "{}"),
        (Value::Null, "{}"),
    ] {
        fails(&[
            start(),
            block(
                0,
                json!({"type":"tool_use","id":"tool_1","name":"read","input":initial}),
            ),
            delta(
                0,
                json!({"type":"input_json_delta","partial_json":fragment}),
            ),
            block_stop(0),
            finish("tool_use", 3),
            stop(),
        ])
        .await;
    }
    let tool = block(
        0,
        json!({"type":"tool_use","id":"call","name":"read","input":{}}),
    );
    fails(&[
        start(),
        tool.clone(),
        block_stop(0),
        finish("end_turn", 3),
        stop(),
    ])
    .await;
    fails(&[start(), tool.clone(), finish("tool_use", 3), stop()]).await;
    fails(&[
        start(),
        tool.clone(),
        block_stop(0),
        block(1, tool["content_block"].clone()),
        block_stop(1),
        finish("tool_use", 3),
        stop(),
    ])
    .await;
    fails(&[
        start(),
        json!({"type":"future_event"}),
        finish("end_turn", 3),
        stop(),
    ])
    .await;
}

#[tokio::test]
async fn usage_is_checked_not_wrapped_or_silently_reset() {
    for usage in [
        json!({"output_tokens":0}),
        json!({"output_tokens":-1}),
        json!({"output_tokens":1.5}),
        json!({"input_tokens":4}),
        json!({"input_tokens":u64::MAX,"cache_read_input_tokens":1}),
    ] {
        let mut values = text_response();
        values[4]["usage"] = usage;
        fails(&values).await;
    }
    let mut values = text_response();
    values[0]["message"]["usage"]["input_tokens"] = json!(u64::MAX);
    values[0]["message"]["usage"]["cache_creation_input_tokens"] = json!(1);
    fails(&values).await;
    for usage in [Value::Null, json!({}), json!({"input_tokens":5})] {
        let mut missing = text_response();
        missing[4]["usage"] = usage;
        fails(&missing).await;
    }
}

#[tokio::test]
async fn malformed_framing_and_unterminated_frames_fail_without_echoing_data() {
    for wire in [
        "event: ping\ndata: secret\n\n",
        "event: ping\ndata: {\"type\":\"message_stop\"}\n\n",
        "data: {\"type\":\"ping\"}\n\n",
        "event: ping\nevent: ping\ndata: {\"type\":\"ping\"}\n\n",
        "event: error\ndata: secret\n\n",
        "event: ping\ndata:",
        "data: [DONE]\n\n",
    ] {
        let result = events(wire.as_bytes().to_vec(), 1).await;
        assert!(result.iter().any(Result::is_err));
        for error in result.into_iter().filter_map(Result::err) {
            assert!(!error.message.contains("secret"));
        }
    }
    assert!(events(vec![0xff, b'\n', b'\n'], 1).await[0].is_err());
}

#[tokio::test]
async fn frame_and_total_caps_include_comments_and_partial_tail_at_exact_boundaries() {
    let good = sse(&text_response());
    // A frame including its delimiter can be exactly MAX_FRAME, but not one byte more.
    for excess in [0, 1] {
        let mut data = vec![b':'];
        data.extend(vec![b'x'; MAX_FRAME - 3 + excess]);
        data.extend(b"\n\n");
        data.extend(good.as_bytes());
        let result = events(data, 4093).await;
        assert_eq!(result.iter().any(Result::is_err), excess != 0);
    }
    // Overflow must reject without waiting for a delimiter, including after a valid frame.
    let mut tail = b":ok\n\n".to_vec();
    tail.extend(vec![b'x'; MAX_FRAME + 1]);
    assert!(events(tail, MAX_FRAME + 10).await.last().unwrap().is_err());
    for excess in [0, 1] {
        let padding = MAX_RESPONSE - good.len() + excess;
        let mut data = Vec::new();
        while padding - data.len() > MAX_FRAME {
            data.push(b':');
            data.extend(vec![b'x'; MAX_FRAME - 3]);
            data.extend(b"\n\n");
        }
        let remaining = padding - data.len();
        data.push(b':');
        data.extend(vec![b'x'; remaining - 3]);
        data.extend(b"\n\n");
        data.extend(good.as_bytes());
        let result = events(data, 8191).await;
        assert_eq!(result.iter().any(Result::is_err), excess != 0);
        assert_eq!(
            result
                .iter()
                .any(|e| matches!(e, Ok(ModelStreamEvent::Completed(_)))),
            excess == 0
        );
    }
}

#[tokio::test]
async fn terminal_closes_source_without_eof_and_does_not_interpret_later_events() {
    let mut wire = sse(&text_response());
    wire.push_str("event: error\ndata: secret\n\n");
    let source = stream::iter([Ok(bytes::Bytes::from(wire))]).chain(stream::pending());
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        make_stream(source, CancellationToken::new()).collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    assert!(result.iter().all(Result::is_ok));
    assert!(matches!(
        result.last(),
        Some(Ok(ModelStreamEvent::Completed(_)))
    ));
}

#[test]
fn payload_encodes_system_tools_choices_and_stable_replay() {
    let mut req = request();
    req.tools.push(spec());
    for (choice, expected) in [
        (ToolChoice::None, json!({"type":"none"})),
        (
            ToolChoice::Auto,
            json!({"type":"auto","disable_parallel_tool_use":true}),
        ),
        (
            ToolChoice::Required,
            json!({"type":"any","disable_parallel_tool_use":true}),
        ),
        (
            ToolChoice::Named("read".into()),
            json!({"type":"tool","name":"read","disable_parallel_tool_use":true}),
        ),
    ] {
        req.controls.tool_choice = choice;
        let body = AnthropicMessages::payload(&req).unwrap();
        assert_eq!(body["tool_choice"], expected);
        assert_eq!(body["system"], "rules");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["input_schema"], req.tools[0].input_schema);
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body.get("thinking").is_none());
    }
    req.controls.parallel_tool_calls = true;
    assert_eq!(
        AnthropicMessages::payload(&req).unwrap()["tool_choice"]["disable_parallel_tool_use"],
        false
    );
    let calls = [7, 8].map(|n| TranscriptContent::ToolCall {
        invocation: crate::InvocationId::new(n).unwrap(),
        name: "read".into(),
        arguments: json!({"path":"a"}),
        origin_provider_id: Some("colliding".into()),
    });
    req.messages.push(crate::TranscriptMessage {
        role: TranscriptRole::Assistant,
        content: vec![
            TranscriptContent::Text("before".into()),
            calls[0].clone(),
            TranscriptContent::Text("between".into()),
            calls[1].clone(),
        ],
        provider_replay: None,
    });
    for n in [7, 8] {
        req.messages.push(crate::TranscriptMessage {
            role: TranscriptRole::Tool,
            content: vec![TranscriptContent::ToolResult {
                invocation: crate::InvocationId::new(n).unwrap(),
                name: "read".into(),
                result: json!({"ok":true}),
            }],
            provider_replay: None,
        });
    }
    let body = AnthropicMessages::payload(&req).unwrap();
    assert_eq!(body["messages"][1]["content"][0]["text"], "before");
    assert_eq!(body["messages"][1]["content"][2]["text"], "between");
    for (i, id) in ["ion_7", "ion_8"].iter().enumerate() {
        assert_eq!(body["messages"][1]["content"][i * 2 + 1]["id"], *id);
        assert_eq!(body["messages"][2]["role"], "user");
        assert_eq!(body["messages"][2]["content"][i]["type"], "tool_result");
        assert_eq!(body["messages"][2]["content"][i]["tool_use_id"], *id);
    }
    let mut empty_text = req.clone();
    empty_text.messages[1]
        .content
        .insert(1, TranscriptContent::Text(String::new()));
    assert_eq!(body, AnthropicMessages::payload(&empty_text).unwrap());
    let mut duplicate = req.clone();
    duplicate.messages.push(req.messages[2].clone());
    assert!(AnthropicMessages::payload(&duplicate).is_err());
    let mut orphan = req.clone();
    orphan.messages.remove(1);
    assert!(AnthropicMessages::payload(&orphan).is_err());
    let mut missing = req.clone();
    missing.messages.pop();
    assert!(AnthropicMessages::payload(&missing).is_err());
    let mut duplicate_call = req.clone();
    duplicate_call.messages[1].content.push(calls[0].clone());
    assert!(AnthropicMessages::payload(&duplicate_call).is_err());
    let mut wrong_name = req.clone();
    if let TranscriptContent::ToolResult { name, .. } = &mut wrong_name.messages[2].content[0] {
        *name = "other".into();
    }
    assert!(AnthropicMessages::payload(&wrong_name).is_err());
    let mut wrong_role = req.clone();
    wrong_role.messages[1].role = TranscriptRole::User;
    assert!(AnthropicMessages::payload(&wrong_role).is_err());
    let mut interposed = req.clone();
    interposed
        .messages
        .insert(2, crate::TranscriptMessage::user_text("late"));
    assert!(AnthropicMessages::payload(&interposed).is_err());
    req.messages[1].provider_replay = Some(ion_ai::ProviderReplay::new(
        "anthropic",
        "thinking",
        json!({}),
    ));
    assert_eq!(
        AnthropicMessages::payload(&req).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
}

#[test]
fn endpoint_is_exact_https_origin_and_messages_path() {
    for url in [
        "http://api.example.test/v1/messages",
        "https://other.test/v1/messages",
        "https://api.example.test:444/v1/messages",
        "https://user@api.example.test/v1/messages",
        "https://api.example.test/v1/messages?q=x",
        "https://api.example.test/v1/messages#x",
        "https://api.example.test/v1/chat/completions",
        "https://api.example.test/v1/messages/",
        "not a url",
    ] {
        assert!(
            AnthropicMessages::new(
                identity("https://api.example.test".into()),
                url,
                Arc::new(|| None)
            )
            .is_err(),
            "{url}"
        );
    }
    assert!(
        AnthropicMessages::new(
            identity("https://api.example.test".into()),
            "https://api.example.test/v1/messages",
            Arc::new(|| None)
        )
        .is_ok()
    );
    assert!(
        AnthropicMessages::new(
            identity("http://127.0.0.1:8080".into()),
            "http://127.0.0.1:8080/v1/messages",
            Arc::new(|| None)
        )
        .is_ok()
    );
    assert!(
        AnthropicMessages::new(
            identity("http://localhost:8080".into()),
            "http://localhost:8080/v1/messages",
            Arc::new(|| None)
        )
        .is_err()
    );
    let mut local = identity("https://api.example.test".into());
    local.egress = EgressRealm::Local;
    assert!(
        AnthropicMessages::new(
            local,
            "https://api.example.test/v1/messages",
            Arc::new(|| None)
        )
        .is_err()
    );
}

#[tokio::test]
async fn unsupported_controls_reject_before_credentials_or_dispatch() {
    let boundary = AnthropicMessages::new(
        identity("https://api.example.test".into()),
        "https://api.example.test/v1/messages",
        Arc::new(|| panic!("credentials fetched before validation")),
    )
    .unwrap();
    let mut requests = Vec::new();
    for reasoning in [
        Reasoning::Off,
        Reasoning::Low,
        Reasoning::Medium,
        Reasoning::High,
        Reasoning::BudgetTokens(100),
    ] {
        let mut req = request();
        req.controls.reasoning = reasoning;
        requests.push(req);
    }
    let mut req = request();
    req.controls.temperature = Some(0.5);
    requests.push(req);
    let mut req = request();
    req.controls.top_p = Some(0.5);
    requests.push(req);
    for req in requests {
        assert_eq!(
            boundary.fingerprint(&req, "effect").unwrap_err().kind,
            ProviderErrorKind::Unsupported
        );
        assert!(matches!(
            boundary
                .start(
                    AttemptId::new(1).unwrap(),
                    "effect".into(),
                    req,
                    CancellationToken::new()
                )
                .await,
            ModelStart::NotStarted { .. }
        ));
    }
    let stop = CancellationToken::new();
    stop.cancel();
    assert!(matches!(
        boundary
            .start(AttemptId::new(1).unwrap(), "effect".into(), request(), stop)
            .await,
        ModelStart::NotStarted { .. }
    ));
    let digest = boundary.fingerprint(&request(), "a").unwrap();
    assert_eq!(digest, boundary.fingerprint(&request(), "b").unwrap());
    let mut req = request();
    req.instructions.push('x');
    assert_ne!(digest, boundary.fingerprint(&req, "a").unwrap());
    let mut req = request();
    req.controls.max_output_tokens = 0;
    assert!(boundary.fingerprint(&req, "a").is_err());
    let mut req = request();
    req.controls.tool_choice = ToolChoice::Required;
    assert!(boundary.fingerprint(&req, "a").is_err());
    let mut req = request();
    req.tools.push(spec());
    req.controls.tool_choice = ToolChoice::Named("absent".into());
    assert!(boundary.fingerprint(&req, "a").is_err());
}

pub(super) fn read_request(socket: &mut TcpStream) -> (String, Value) {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    let (end, length) = loop {
        let n = socket.read(&mut chunk).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        assert!(bytes.len() < 1024 * 1024);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|s| {
                    s.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|n| n.parse().unwrap())
                })
                .unwrap();
            break (end + 4, length);
        }
    };
    while bytes.len() - end < length {
        let n = socket.read(&mut chunk).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
    }
    (
        String::from_utf8(bytes[..end].to_vec()).unwrap(),
        serde_json::from_slice(&bytes[end..end + length]).unwrap(),
    )
}
fn mock(raw: String) -> (AnthropicMessages, std::thread::JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let request = read_request(&mut socket);
        socket.write_all(raw.as_bytes()).unwrap();
        request
    });
    (
        AnthropicMessages::new(
            identity(origin.clone()),
            &format!("{origin}/v1/messages"),
            Arc::new(|| Some("test-secret".into())),
        )
        .unwrap(),
        server,
    )
}
pub(super) fn http(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
#[tokio::test]
async fn http_request_has_correct_headers_body_and_live_key_lookup() {
    let (mut boundary, server) = mock(http(&sse(&text_response())));
    let lookups = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&lookups);
    boundary.key = Arc::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
        Some("live-secret".into())
    });
    let digest = boundary.fingerprint(&request(), "effect").unwrap();
    assert_eq!(lookups.load(Ordering::SeqCst), 0);
    let ModelStart::Started {
        stream,
        start_receipt,
    } = boundary
        .start(
            AttemptId::new(1).unwrap(),
            "effect".into(),
            request(),
            CancellationToken::new(),
        )
        .await
    else {
        panic!("not dispatched")
    };
    assert!(start_receipt.is_none());
    assert!(stream.collect::<Vec<_>>().await.iter().all(Result::is_ok));
    let (headers, body) = server.join().unwrap();
    let headers = headers.to_ascii_lowercase();
    assert!(headers.starts_with("post /v1/messages http/1.1"));
    assert!(headers.contains("x-api-key: live-secret\r\n"));
    assert!(headers.contains("anthropic-version: 2023-06-01\r\n"));
    assert!(headers.contains("accept: text/event-stream\r\n"));
    assert!(!headers.contains("authorization:"));
    assert!(!headers.contains("idempotency"));
    assert_eq!(body, AnthropicMessages::payload(&request()).unwrap());
    assert_eq!(lookups.load(Ordering::SeqCst), 1);
    assert_eq!(digest, boundary.fingerprint(&request(), "effect").unwrap());
    boundary.key = Arc::new(|| Some("invalid\r\nsecret".into()));
    let ModelStart::NotStarted { reason } = boundary
        .start(
            AttemptId::new(3).unwrap(),
            "effect".into(),
            request(),
            CancellationToken::new(),
        )
        .await
    else {
        panic!("invalid key dispatched")
    };
    assert!(!reason.contains("secret"));
}

#[tokio::test]
async fn literal_loopback_messages_dispatches_without_a_key_header() {
    let (mut boundary, server) = mock(http(&sse(&text_response())));
    boundary.key = Arc::new(|| None);
    let ModelStart::Started { stream, .. } = boundary
        .start(
            AttemptId::new(1).unwrap(),
            "effect".into(),
            request(),
            CancellationToken::new(),
        )
        .await
    else {
        panic!("local request did not dispatch")
    };
    assert!(stream.collect::<Vec<_>>().await.iter().all(Result::is_ok));
    let (headers, _) = server.join().unwrap();
    assert!(!headers.to_ascii_lowercase().contains("x-api-key:"));
}

#[tokio::test]
async fn http_failures_and_redirects_never_echo_bodies_or_retry() {
    let target = TcpListener::bind("127.0.0.1:0").unwrap();
    target.set_nonblocking(true).unwrap();
    for (status, expected) in [
        (401, ProviderErrorKind::Authentication),
        (403, ProviderErrorKind::Permission),
        (408, ProviderErrorKind::Timeout),
        (429, ProviderErrorKind::RateLimited),
        (400, ProviderErrorKind::InvalidRequest),
        (529, ProviderErrorKind::Overloaded),
        (503, ProviderErrorKind::Overloaded),
        (500, ProviderErrorKind::Server),
        (302, ProviderErrorKind::Server),
    ] {
        let raw = format!(
            "HTTP/1.1 {status} Test\r\nLocation: http://{}/v1/messages\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret",
            target.local_addr().unwrap()
        );
        let (boundary, server) = mock(raw);
        let ModelStart::Started { stream, .. } = boundary
            .start(
                AttemptId::new(1).unwrap(),
                "effect".into(),
                request(),
                CancellationToken::new(),
            )
            .await
        else {
            panic!("not dispatched")
        };
        let result = stream.collect::<Vec<_>>().await;
        assert_eq!(result.len(), 1);
        let err = result[0].as_ref().unwrap_err();
        assert_eq!(err.kind, expected);
        assert_eq!(err.message, format!("HTTP {status}"));
        server.join().unwrap();
    }
    assert_eq!(
        target.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let (boundary, server) = mock("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecret".into());
    let ModelStart::Started { mut stream, .. } = boundary
        .start(
            AttemptId::new(1).unwrap(),
            "effect".into(),
            request(),
            CancellationToken::new(),
        )
        .await
    else {
        panic!("not dispatched")
    };
    assert_eq!(
        stream.next().await.unwrap().unwrap_err().message,
        "response is not text/event-stream"
    );
    server.join().unwrap();
}

#[tokio::test]
async fn cancellation_after_send_is_indeterminate_and_stream_cancellation_preserves_usage() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (arrived, arrival) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        read_request(&mut socket);
        arrived.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    let boundary = Arc::new(
        AnthropicMessages::new(
            identity(origin.clone()),
            &format!("{origin}/v1/messages"),
            Arc::new(|| Some("secret".into())),
        )
        .unwrap(),
    );
    let stop = CancellationToken::new();
    let running = tokio::spawn({
        let boundary = Arc::clone(&boundary);
        let stop = stop.clone();
        async move {
            boundary
                .start(AttemptId::new(1).unwrap(), "effect".into(), request(), stop)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), arrival)
        .await
        .unwrap()
        .unwrap();
    stop.cancel();
    assert!(matches!(
        running.await.unwrap(),
        ModelStart::Indeterminate { .. }
    ));
    release.send(()).unwrap();
    server.join().unwrap();
    let stop = CancellationToken::new();
    let source = stream::iter([Ok(bytes::Bytes::from(sse(&[start()])))]).chain(stream::pending());
    let mut result = make_stream(source, stop.clone());
    assert_eq!(
        result.next().await.unwrap().unwrap(),
        ModelStreamEvent::Usage(Usage::known(5, 1))
    );
    stop.cancel();
    assert_eq!(
        result.next().await.unwrap().unwrap_err().kind,
        ProviderErrorKind::Cancelled
    );
    assert!(result.next().await.is_none());
}
