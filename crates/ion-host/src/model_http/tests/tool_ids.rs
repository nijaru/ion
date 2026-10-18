use super::*;

pub(super) fn exchange(ids: &[&str]) -> [Message; 2] {
    [
        Message {
            role: Role::Assistant,
            content: ids
                .iter()
                .map(|id| {
                    Content::ToolCall(ToolCall {
                        id: (*id).into(),
                        name: "read".into(),
                        arguments: json!({"path":"README.md"}),
                        raw_arguments: None,
                    })
                })
                .collect(),
            provider_replay: None,
        },
        Message {
            role: Role::Tool,
            content: ids
                .iter()
                .rev()
                .map(|id| {
                    Content::ToolResult(ion_ai::ToolResult {
                        call_id: (*id).into(),
                        name: "read".into(),
                        result: json!({"original":id}),
                        images: Vec::new(),
                        is_error: false,
                    })
                })
                .collect(),
            provider_replay: None,
        },
    ]
}

fn assert_wire_id(id: &str) {
    assert!(!id.is_empty() && id.len() <= 64, "{id:?}");
    assert!(
        id.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')),
        "{id:?}"
    );
}

#[test]
fn unsigned_migrated_ids_are_paired_without_mutating_history_or_chat_wire() {
    let long = "call".repeat(100);
    let ids = [
        "functions.echo:0",
        "functions_echo_0",
        "调用🙂",
        long.as_str(),
    ];
    let mut request = request();
    request.messages.extend(exchange(&ids));
    // Exercise the persisted neutral format, not an adapter-specific input.
    request.messages =
        serde_json::from_slice(&serde_json::to_vec(&request.messages).unwrap()).unwrap();
    let original = serde_json::to_value(&request.messages).unwrap();
    let body = anthropic_body(&request, true).unwrap();
    let calls = body["messages"][1]["content"].as_array().unwrap();
    let results = body["messages"][2]["content"].as_array().unwrap();
    let mut mapped = BTreeMap::new();
    for (raw, block) in ids.iter().zip(calls) {
        let wire = block["id"].as_str().unwrap();
        assert_wire_id(wire);
        mapped.insert(*raw, wire);
    }
    assert_eq!(mapped.values().collect::<BTreeSet<_>>().len(), ids.len());
    assert_eq!(mapped["functions_echo_0"], "functions_echo_0");
    for block in results {
        let value: Value =
            serde_json::from_str(block["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(
            block["tool_use_id"],
            mapped[value["original"].as_str().unwrap()]
        );
    }
    let chat = chat_body(&request, HttpWire::ChatCompletions).unwrap();
    let assistant = chat["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "assistant")
        .unwrap();
    for (raw, call) in ids.iter().zip(assistant["tool_calls"].as_array().unwrap()) {
        assert_eq!(call["id"], *raw);
    }
    assert_eq!(serde_json::to_value(&request.messages).unwrap(), original);
}

#[test]
fn later_unsigned_id_collisions_do_not_rewrite_an_earlier_wire_prefix() {
    for reserved_first in [false, true] {
        let mut request = request();
        request.messages.extend(exchange(&["functions.echo:0"]));
        let before = anthropic_body(&request, true).unwrap();
        let generated = before["messages"][1]["content"][0]["id"].as_str().unwrap();
        assert_wire_id(generated);
        if reserved_first {
            request.messages.splice(1..1, exchange(&[generated]));
        } else {
            request.messages.extend(exchange(&[generated]));
        }
        let after = anthropic_body(&request, true).unwrap();
        let first = after["messages"][1]["content"][0]["id"].as_str().unwrap();
        let second = after["messages"][3]["content"][0]["id"].as_str().unwrap();
        assert_wire_id(first);
        assert_wire_id(second);
        assert_ne!(first, second);
        assert_eq!(
            first, generated,
            "adding history changed the encoded prefix"
        );
        assert_eq!(after["messages"][2]["content"][0]["tool_use_id"], first);
        assert_eq!(after["messages"][4]["content"][0]["tool_use_id"], second);
    }
}

fn signed_call(request: &ModelRequest, id: &str) -> Message {
    let mut state = AnthropicState::default();
    for event in [
        json!({"type":"message_start","message":{"type":"message","role":"assistant",
            "model":"test-model","content":[],"stop_reason":null,
            "usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{
            "type":"thinking","thinking":"private","signature":"opaque-signature"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{
            "type":"tool_use","id":id,"name":"read","input":{"path":"README.md"}}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ] {
        for output in state.accept(&event, request).unwrap() {
            if let ModelStreamEvent::Completed(response) = output {
                return serde_json::from_slice(&serde_json::to_vec(&response.message).unwrap())
                    .unwrap();
            }
        }
    }
    panic!("signed fixture did not complete")
}

#[test]
fn migrated_prefix_and_native_replay_keep_separate_id_ownership() {
    let mut request = request();
    request.messages.extend(exchange(&["functions.echo:0"]));
    let prefix = anthropic_body(&request, true).unwrap();
    let generated = prefix["messages"][1]["content"][0]["id"].as_str().unwrap();
    for id in ["native:opaque", generated] {
        let mut request = request.clone();
        let assistant = signed_call(&request, id);
        let blocks = assistant.provider_replay.as_ref().unwrap().data["blocks"].clone();
        request.messages.push(assistant);
        request
            .messages
            .push(exchange(&[id]).into_iter().nth(1).unwrap());
        let original = serde_json::to_value(&request.messages).unwrap();
        if id == generated {
            let error = anthropic_body(&request, true).unwrap_err();
            assert_eq!(error.kind, ProviderErrorKind::Unsupported);
            assert!(error.message.contains("retained replay"));
        } else {
            let body = anthropic_body(&request, true).unwrap();
            assert_eq!(body["messages"][3]["content"], blocks);
            assert_eq!(body["messages"][4]["content"][0]["tool_use_id"], id);
            // Future unsigned collisions, including reuse of a native opaque
            // identity, cannot rename or rebind the already-signed prefix.
            request.messages.extend(exchange(&[generated, id]));
            let after = anthropic_body(&request, true).unwrap();
            assert_eq!(after["messages"][3]["content"], blocks);
            for block in after["messages"][5]["content"].as_array().unwrap() {
                assert_wire_id(block["id"].as_str().unwrap());
            }
            assert_eq!(
                &after["messages"].as_array().unwrap()[..3],
                prefix["messages"].as_array().unwrap()
            );
            request.messages.truncate(5);
        }
        assert_eq!(serde_json::to_value(&request.messages).unwrap(), original);
    }
}
