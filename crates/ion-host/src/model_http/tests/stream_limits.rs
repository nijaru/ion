use super::*;

#[tokio::test]
async fn framing_overhead_does_not_consume_the_decoded_output_allowance() {
    let frame = format!(
        "data: {}\n\n",
        json!({"id":"stream-fixture-00000000000000000000","object":"chat.completion.chunk",
            "created":1234567890,"model":"test-model","system_fingerprint":"fp_fixture",
            "choices":[{"index":0,"delta":{"content":" a"},"finish_reason":null}],"usage":null})
    );
    let mut body = frame.repeat(40_000);
    body.push_str(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    );
    assert!(body.len() > 8 * 1024 * 1024);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request_body(&mut socket).await;
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        // A rejected stream can close its socket before the fixture finishes.
        let _ = socket.write_all(body.as_bytes()).await;
    });
    let model =
        HttpModelService::new(&endpoint, HttpWire::ChatCompletions, Arc::new(|| None)).unwrap();
    let mut request = request();
    request.controls.max_output_tokens = 128_000;
    let mut stream = model.stream(request).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let mut text = 0;
        let mut completed = None;
        while let Some(event) = stream.next().await {
            match event? {
                ModelStreamEvent::TextDelta(part) => text += part.len(),
                ModelStreamEvent::Completed(response) => completed = Some(response),
                _ => {}
            }
        }
        Ok::<_, ProviderError>((text, completed))
    })
    .await;
    drop(stream);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let (text, completed) = result.unwrap().unwrap();
    assert_eq!(text, 80_000);
    assert_eq!(
        completed.unwrap().message.content,
        vec![Content::Text(" a".repeat(40_000))]
    );
}

#[test]
fn decoded_limits_cover_text_arguments_and_both_reasoning_channels() {
    let part = "x".repeat(128 * 1024);
    for (wire, delta) in [
        (HttpWire::ChatCompletions, json!({"content":part})),
        (HttpWire::DeepSeekChat, json!({"reasoning_content":part})),
        (
            HttpWire::ChatCompletions,
            json!({"tool_calls":[{"index":0,"id":"call","function":{"name":"read","arguments":part}}]}),
        ),
        (
            HttpWire::OpenRouterChat,
            json!({"reasoning_details":[{"type":"reasoning.encrypted","data":part}]}),
        ),
    ] {
        let mut state = ChatState::new(wire);
        let value = json!({"choices":[{"delta":delta}]});
        let error = (0..128)
            .find_map(|_| state.accept(&value).err())
            .expect("decoded output must be bounded");
        assert!(
            error.message.contains("decoded response exceeded"),
            "{error}"
        );
        assert!(state.budget.used <= 8 * 1024 * 1024);
    }
    for (kind, field) in [
        ("text_delta", "text"),
        ("signature_delta", "signature"),
        ("input_json_delta", "partial_json"),
    ] {
        let mut state = AnthropicState::default();
        let start = json!({"type":"message_start","message":{"type":"message","role":"assistant","model":"test-model","content":[],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":0}}});
        state.accept(&start, &request()).unwrap();
        let block = match kind {
            "text_delta" => json!({"type":"text","text":""}),
            "signature_delta" => json!({"type":"thinking","thinking":"","signature":""}),
            _ => json!({"type":"tool_use","id":"call","name":"read","input":{}}),
        };
        state
            .accept(
                &json!({"type":"content_block_start","index":0,"content_block":block}),
                &request(),
            )
            .unwrap();
        let value =
            json!({"type":"content_block_delta","index":0,"delta":{"type":kind,field:part}});
        let error = (0..128)
            .find_map(|_| state.accept(&value, &request()).err())
            .expect("decoded output must be bounded");
        assert!(
            error.message.contains("decoded response exceeded"),
            "{error}"
        );
        assert!(state.budget.used <= 8 * 1024 * 1024);
    }
}

#[test]
fn comments_and_repeated_metadata_do_not_refresh_progress() {
    for suffix in [
        "\n",
        "data: {\"model\":\"test-model\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":0}}\n\n",
    ] {
        let frame = format!(":{}\n{suffix}", "x".repeat(128 * 1024));
        let mut decoder = Decoder::Chat(ChatState::default());
        let mut budget = StreamBudget::default();
        let error = (0..128)
            .find_map(|_| {
                decode_stream_frame(
                    frame.as_bytes(),
                    &mut decoder,
                    &request(),
                    HttpWire::ChatCompletions,
                    &mut budget,
                )
                .err()
            })
            .expect("nonproductive framing must be bounded");
        assert!(error.message.contains("no-progress"), "{error}");
    }
}

#[test]
fn anthropic_transformations_charge_changed_but_not_identical_snapshots() {
    let transformations = json!([{"type":"thinking_dropped","reason":"prefix_binding_mismatch","payload":"x".repeat(128 * 1024)}]);
    let mut state = AnthropicState::default();
    state.accept(&json!({"type":"message_start","message":{"type":"message","role":"assistant","model":"test-model","content":[],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":0},"input_transformations":transformations}}), &request()).unwrap();
    let before = state.budget.used;
    let mut delta = json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":0},"input_transformations":transformations});
    state.accept(&delta, &request()).unwrap();
    assert_eq!(
        state.budget.used, before,
        "identical snapshots do not add retention"
    );
    let error = (0..128)
        .find_map(|index| {
            delta["input_transformations"][0]["round"] = json!(index);
            state.accept(&delta, &request()).err()
        })
        .expect("changed metadata snapshots must consume the response budget");
    assert!(
        error.message.contains("decoded response exceeded"),
        "{error}"
    );
}
