use super::super::responses_request::{RESPONSES_REPLAY, project_output};
use super::*;

fn text_item(text: &str) -> Value {
    json!({"id":"msg_1","type":"message","role":"assistant","status":"completed","phase":"final_answer",
        "content":[{"type":"output_text","text":text,"annotations":[]}]})
}
fn call_item(id: &str, args: &str) -> Value {
    json!({"id":format!("fc_{id}"),"type":"function_call","call_id":id,"name":"read","arguments":args,"status":"completed"})
}
fn terminal(output: Vec<Value>) -> Value {
    json!({"type":"response.completed","response":{"id":"resp_1","model":"test-model","status":"completed","output":output,
        "usage":{"input_tokens":10,"output_tokens":8,"total_tokens":18,"input_tokens_details":{"cached_tokens":6},"output_tokens_details":{"reasoning_tokens":3}}}})
}
fn added(i: usize, item: Value) -> Value {
    json!({"type":"response.output_item.added","output_index":i,"item":item})
}
fn event(value: &Value) -> String {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["type"].as_str().unwrap()
    )
}
fn completed(events: Vec<ModelStreamEvent>) -> ModelResponse {
    events
        .into_iter()
        .find_map(|e| match e {
            ModelStreamEvent::Completed(r) => Some(r),
            _ => None,
        })
        .unwrap()
}

#[test]
fn plain_chat_history_migrates_but_native_chat_replay_is_rejected() {
    let mut r = request();
    r.route = ModelRoute::direct(
        ModelRef {
            provider: "openai".into(),
            model: "gpt-5.5".into(),
        },
        ModelRouteReason::UserRequest,
    );
    r.messages.extend([
        Message {
            role: Role::Assistant,
            content: vec![
                Content::Text("Reading.".into()),
                Content::ToolCall(ToolCall {
                    id: "call_1".into(),
                    name: "read".into(),
                    arguments: json!({"path":"a"}),
                    raw_arguments: None,
                }),
            ],
            provider_replay: None,
        },
        Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "call_1".into(),
                name: "read".into(),
                result: json!("contents"),
                is_error: false,
                images: vec![],
            })],
            provider_replay: None,
        },
        Message::user_input("Continue.".into(), []),
    ]);
    assert_eq!(
        responses_body(&r).unwrap()["input"],
        json!([
            {"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]},
            {"type":"message","role":"assistant","content":[{"type":"input_text","text":"Reading."}]},
            {"type":"function_call","call_id":"call_1","name":"read","arguments":"{\"path\":\"a\"}"},
            {"type":"function_call_output","call_id":"call_1","output":"{\"is_error\":false,\"result\":\"contents\"}"},
            {"type":"message","role":"user","content":[{"type":"input_text","text":"Continue."}]},
        ])
    );
    r.messages[1].provider_replay = Some(ProviderReplay::new(
        "openai",
        CHAT_REASONING_CONTENT_REPLAY,
        json!("private reasoning"),
    ));
    let error = responses_body(&r).unwrap_err();
    assert_eq!(error.kind, ProviderErrorKind::Unsupported);
    assert!(
        error
            .message
            .contains("explicitly compact or start a new Session")
    );
}

#[test]
fn stateless_request_retains_native_phases_refusals_and_tool_status() {
    let mut request = request();
    let output = vec![
        json!({"id":"rs_1","type":"reasoning","summary":[],"encrypted_content":"CIPHER"}),
        json!({"id":"msg_comment","type":"message","role":"assistant","phase":"commentary","content":[{"type":"refusal","refusal":"No."}]}),
        call_item("call_1", "{\"path\":\"a\"}"),
    ];
    request.messages.push(Message {
        role: Role::Assistant,
        content: project_output(&output, true).unwrap(),
        provider_replay: Some(ProviderReplay::new(
            "test",
            RESPONSES_REPLAY,
            json!({"model":"test-model","output":output}),
        )),
    });
    request.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: "call_1".into(),
            name: "read".into(),
            result: json!({"withheld":true}),
            is_error: true,
            images: vec![],
        })],
        provider_replay: None,
    });
    request.controls.tool_choice = ToolChoice::Named("read".into());
    let body = responses_body(&request).unwrap();
    assert_eq!(
        body,
        json!({"model":"test-model","instructions":"instructions","input":[
        {"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]},
        output[0],output[1],output[2],
        {"type":"function_call_output","call_id":"call_1","output":"{\"is_error\":true,\"result\":{\"withheld\":true}}"}],
        "stream":true,"store":false,"include":["reasoning.encrypted_content"],"max_output_tokens":1024,
        "tools":[{"type":"function","name":"read","description":"read","parameters":{"type":"object"},"strict":false}],
        "tool_choice":{"type":"function","name":"read"},"parallel_tool_calls":false})
    );
    let original = request.messages[1].clone();
    request.messages[1].content[0] = Content::Text("changed".into());
    assert!(responses_body(&request).is_err());
    request.messages[1] = original;
    request.route.effective.model = "different".into();
    assert_eq!(
        responses_body(&request).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
    request.route.effective.model = "test-model".into();
    request.route.effective.provider = "other".into();
    assert!(responses_body(&request).is_err());
}

#[test]
fn foreign_namespaces_cannot_dispatch_a_same_named_local_function() {
    let r = request();
    for namespace in [json!("foreign"), json!({"invalid":true})] {
        let mut call = call_item("a", "{}");
        call["namespace"] = namespace;
        assert!(
            ResponsesState::default()
                .accept(&terminal(vec![call.clone()]), &r)
                .is_err()
        );
        let mut state = ResponsesState::default();
        state.accept(&added(0, call), &r).unwrap();
        assert!(
            state
                .accept(&terminal(vec![call_item("a", "{}")]), &r)
                .is_err()
        );
    }
}

#[test]
fn controls_do_not_clamp_or_guess_reasoning_capabilities() {
    let mut r = request();
    assert!(responses_body(&r).unwrap().get("reasoning").is_none());
    r.controls.reasoning = Reasoning::High;
    assert_eq!(
        responses_body(&r).unwrap()["reasoning"],
        json!({"effort":"high","summary":"auto"})
    );
    r.controls.max_output_tokens = 15;
    assert!(responses_body(&r).is_err());
    r.controls.max_output_tokens = 16;
    r.controls.reasoning = Reasoning::BudgetTokens(12);
    assert_eq!(
        responses_body(&r).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
}

#[test]
fn terminal_snapshot_backfills_reasoning_and_keeps_interleaved_calls_independent() {
    let mut r = request();
    r.controls.parallel_tool_calls = true;
    let mut state = ResponsesState::default();
    let mut first = call_item("a", "");
    first["status"] = json!("in_progress");
    let mut second = call_item("b", "");
    second["status"] = json!("in_progress");
    // A terminal-only reasoning item occupies the missing index zero.
    state.accept(&added(1, first), &r).unwrap();
    state.accept(&added(2, second), &r).unwrap();
    for (i, id, args) in [(2, "fc_b", "{\"path\":\"b\"}"), (1, "fc_a", "not json")] {
        let events=state.accept(&json!({"type":"response.function_call_arguments.delta","output_index":i,"item_id":id,"delta":args}),&r).unwrap();
        assert!(matches!(
            events.as_slice(),
            [ModelStreamEvent::OutputObserved]
        ));
    }
    let output = vec![
        json!({"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"TERMINAL_ONLY"}),
        call_item("a", "not json"),
        call_item("b", "{\"path\":\"b\"}"),
    ];
    let response = completed(state.accept(&terminal(output.clone()), &r).unwrap());
    assert_eq!(
        response.usage,
        Usage {
            input_tokens: Some(10),
            output_tokens: Some(8),
            cache_read_input_tokens: Some(6),
            cache_write_input_tokens: None
        }
    );
    assert_eq!(
        response.message.provider_replay.unwrap().data["output"],
        json!(output)
    );
    let Content::ToolCall(first) = &response.message.content[0] else {
        panic!()
    };
    assert_eq!(first.id, "a");
    assert_eq!(first.raw_arguments.as_deref(), Some("not json"));
    let Content::ToolCall(second) = &response.message.content[1] else {
        panic!()
    };
    assert_eq!(second.arguments, json!({"path":"b"}));
}

#[test]
fn summary_is_human_progress_but_ciphertext_is_not() {
    let r = request();
    let mut state = ResponsesState::default();
    state
        .accept(
            &added(0, json!({"id":"rs_1","type":"reasoning","summary":[]})),
            &r,
        )
        .unwrap();
    let events=state.accept(&json!({"type":"response.reasoning_summary_text.delta","output_index":0,"item_id":"rs_1","summary_index":0,"delta":"Checking"}),&r).unwrap();
    assert!(matches!(&events[0],ModelStreamEvent::ThinkingDelta{block:0,text} if text=="Checking"));
    let item = json!({"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"Checking"}]});
    state
        .accept(
            &json!({"type":"response.output_item.done","output_index":0,"item":item}),
            &r,
        )
        .unwrap();
    let mut final_item = item;
    final_item["encrypted_content"] = json!("NOT_HUMAN");
    let response = completed(
        state
            .accept(&terminal(vec![final_item, text_item("Done")]), &r)
            .unwrap(),
    );
    assert_eq!(
        response.message.content,
        vec![
            Content::Thinking("Checking".into()),
            Content::Text("Done".into())
        ]
    );
}

#[test]
fn final_snapshots_cannot_rewrite_observed_or_finalized_output() {
    let r = request();
    for (delta, final_text) in [("observed", "different"), ("observed", "obser")] {
        let mut state = ResponsesState::default();
        state
            .accept(
                &added(
                    0,
                    json!({"id":"msg_1","type":"message","role":"assistant","content":[]}),
                ),
                &r,
            )
            .unwrap();
        state.accept(&json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg_1","content_index":0,"delta":delta}),&r).unwrap();
        assert!(
            state
                .accept(&terminal(vec![text_item(final_text)]), &r)
                .is_err()
        );
    }
    let mut state = ResponsesState::default();
    state.accept(&added(0, text_item("initial")), &r).unwrap();
    assert!(
        state
            .accept(&terminal(vec![text_item("changed")]), &r)
            .is_err()
    );
}

#[test]
fn finalized_parts_reject_late_deltas_and_unfinished_calls_never_handoff() {
    let r = request();
    let mut state = ResponsesState::default();
    state
        .accept(
            &added(
                0,
                json!({"id":"msg_1","type":"message","role":"assistant","content":[]}),
            ),
            &r,
        )
        .unwrap();
    state.accept(&json!({"type":"response.content_part.done","output_index":0,"item_id":"msg_1","content_index":0,"part":{"type":"output_text","text":"done","annotations":[]}}),&r).unwrap();
    assert!(state.accept(&json!({"type":"response.output_text.delta","output_index":0,"item_id":"msg_1","content_index":0,"delta":"late"}),&r).is_err());
    let mut call = call_item("a", "{}");
    call["status"] = json!("in_progress");
    assert!(
        ResponsesState::default()
            .accept(&terminal(vec![call]), &r)
            .is_err()
    );
}

#[tokio::test]
async fn http_requires_terminal_evidence_and_publishes_failure_output_before_error() {
    let r = request();
    for output in [vec![], vec![call_item("a", "{}")]] {
        let value = json!({"type":"response.failed","response":{"id":"resp_1","status":"failed","output":output,"error":{"code":"server_error","message":"failed"}}});
        let endpoint = serve(event(&value), "200 OK").await;
        let service =
            HttpModelService::new(&endpoint, HttpWire::OpenAiResponses, Arc::new(|| None)).unwrap();
        let events = service
            .stream(r.clone())
            .await
            .unwrap()
            .collect::<Vec<_>>()
            .await;
        assert_eq!(
            events
                .iter()
                .any(|event| matches!(event, Ok(ModelStreamEvent::OutputObserved))),
            !output.is_empty()
        );
        assert!(events.last().unwrap().is_err());
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, Ok(ModelStreamEvent::Completed(_))))
        );
    }
    let endpoint = serve(event(&added(0, call_item("a", "{}"))), "200 OK").await;
    let service =
        HttpModelService::new(&endpoint, HttpWire::OpenAiResponses, Arc::new(|| None)).unwrap();
    let events = service.stream(r).await.unwrap().collect::<Vec<_>>().await;
    assert!(matches!(
        events.first().unwrap(),
        Ok(ModelStreamEvent::OutputObserved)
    ));
    assert_eq!(
        events.last().unwrap().as_ref().unwrap_err().kind,
        ProviderErrorKind::Transport
    );
}

#[test]
fn migrated_aliases_pair_results_without_rewriting_native_continuation() {
    let mut r = request();
    r.messages.push(Message {
        role: Role::Assistant,
        content: vec![Content::Text("Earlier answer".into())],
        provider_replay: None,
    });
    r.messages
        .extend(super::tool_ids::exchange(&["functions.echo:0", "ion_0"]));
    let original = r.messages.clone();
    let first = responses_body(&r).unwrap();
    assert_eq!(first["input"][1]["content"][0]["type"], "input_text");
    let calls = &first["input"].as_array().unwrap()[2..4];
    assert_eq!(calls[0]["call_id"], "ion_0");
    assert_eq!(calls[1]["call_id"], "ion_1");
    for result in &first["input"].as_array().unwrap()[4..] {
        let envelope: Value = serde_json::from_str(result["output"].as_str().unwrap()).unwrap();
        let expected = if envelope["result"]["original"] == "ion_0" {
            "ion_1"
        } else {
            "ion_0"
        };
        assert_eq!(result["call_id"], expected);
    }
    assert_eq!(r.messages, original);
    let output = vec![
        json!({"id":"rs_1","type":"reasoning","summary":[],"encrypted_content":"CIPHER"}),
        text_item("Native answer"),
    ];
    r.messages.push(
        completed(
            ResponsesState::default()
                .accept(&terminal(output), &r)
                .unwrap(),
        )
        .message,
    );
    let second = responses_body(&r).unwrap();
    let prefix = first["input"].as_array().unwrap();
    assert_eq!(&second["input"].as_array().unwrap()[..prefix.len()], prefix);
    let conflict = vec![call_item("ion_0", "{}")];
    r.messages.push(Message {
        role: Role::Assistant,
        content: project_output(&conflict, true).unwrap(),
        provider_replay: Some(ProviderReplay::new(
            "test",
            RESPONSES_REPLAY,
            json!({"model":"test-model","output":conflict}),
        )),
    });
    r.messages.push(Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: "ion_0".into(),
            name: "read".into(),
            result: json!(null),
            images: vec![],
            is_error: false,
        })],
        provider_replay: None,
    });
    assert_eq!(
        responses_body(&r).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
}

#[test]
fn unsigned_native_messages_keep_required_identity_across_models() {
    let mut r = request();
    let output = vec![text_item("Earlier answer")];
    r.messages.push(
        completed(
            ResponsesState::default()
                .accept(&terminal(output.clone()), &r)
                .unwrap(),
        )
        .message,
    );
    r.route.effective.model = "other-model".into();
    assert_eq!(responses_body(&r).unwrap()["input"][1], output[0]);
}

#[test]
fn readable_reasoning_is_not_confused_with_opaque_continuation() {
    let r = request();
    let mut state = ResponsesState::default();
    state
        .accept(
            &added(
                0,
                json!({"id":"rs_1","type":"reasoning","summary":[],"content":[]}),
            ),
            &r,
        )
        .unwrap();
    let events = state.accept(&json!({"type":"response.reasoning_text.delta","output_index":0,"item_id":"rs_1","content_index":0,"delta":"Readable"}),&r).unwrap();
    assert!(
        matches!(&events[0],ModelStreamEvent::ThinkingDelta {block:0,text} if text=="Readable")
    );
    let response = completed(state.accept(&terminal(vec![json!({"id":"rs_1","type":"reasoning","summary":[],"content":[{"type":"reasoning_text","text":"Readable"}],"encrypted_content":"OPAQUE"}),text_item("Done")]),&r).unwrap());
    assert_eq!(
        response.message.content,
        vec![
            Content::Thinking("Readable".into()),
            Content::Text("Done".into())
        ]
    );
}

#[test]
fn decoded_replay_and_repeated_metadata_have_independent_bounds() {
    let r = request();
    let mut state = ResponsesState::default();
    let oversized = json!({"type":"reasoning","id":"rs_1","summary":[],"encrypted_content":"x".repeat(8*1024*1024)});
    assert!(
        state
            .accept(&terminal(vec![oversized]), &r)
            .unwrap_err()
            .message
            .contains("byte limit")
    );
    let mut decoder = Decoder::Responses(ResponsesState::default());
    let mut budget = StreamBudget::default();
    let frame = event(
        &json!({"type":"response.in_progress","response":{"id":"resp_1","model":"test-model"},"padding":"x".repeat(8192)}),
    );
    decode_stream_frame(
        frame.as_bytes(),
        &mut decoder,
        &r,
        HttpWire::OpenAiResponses,
        &mut budget,
    )
    .unwrap();
    let retained = decoder.retained_bytes();
    let mut rejected = false;
    for _ in 0..1025 {
        if decode_stream_frame(
            frame.as_bytes(),
            &mut decoder,
            &r,
            HttpWire::OpenAiResponses,
            &mut budget,
        )
        .is_err()
        {
            rejected = true;
            break;
        }
    }
    assert!(rejected);
    assert_eq!(decoder.retained_bytes(), retained);
    let mismatch =
        b"event: response.completed\ndata: {\"type\":\"response.in_progress\",\"response\":{}}\n\n";
    assert!(decode_frame(mismatch, &mut decoder, &r, HttpWire::OpenAiResponses).is_err());
}

#[test]
fn usage_remains_unknown_when_absent_and_rejects_double_counting() {
    let r = request();
    let mut value = terminal(vec![text_item("Done")]);
    value["response"].as_object_mut().unwrap().remove("usage");
    assert_eq!(
        completed(ResponsesState::default().accept(&value, &r).unwrap()).usage,
        Usage::unknown()
    );
    for usage in [
        json!({"input_tokens":10,"input_tokens_details":{"cached_tokens":11}}),
        json!({"output_tokens":2,"output_tokens_details":{"reasoning_tokens":3}}),
        json!({"input_tokens":10,"output_tokens":8,"total_tokens":24}),
        json!({"input_tokens":-1}),
    ] {
        value["response"]["usage"] = usage;
        assert!(ResponsesState::default().accept(&value, &r).is_err());
    }
}

#[tokio::test]
async fn full_snapshot_frames_are_bounded_separately_from_chat_deltas() {
    let text = "x".repeat(MAX_FRAME + 1);
    let endpoint = serve_split(event(&terminal(vec![text_item(&text)])), "200 OK", 997).await;
    let service =
        HttpModelService::new(&endpoint, HttpWire::OpenAiResponses, Arc::new(|| None)).unwrap();
    let mut stream = service.stream(request()).await.unwrap();
    let mut response = None;
    while let Some(event) = stream.next().await {
        if let ModelStreamEvent::Completed(value) = event.unwrap() {
            response = Some(value);
        }
    }
    assert_eq!(response.unwrap().message.content, vec![Content::Text(text)]);
}
