use super::*;

fn visible(events: Vec<ModelStreamEvent>) -> Vec<ModelStreamEvent> {
    events
        .into_iter()
        .filter(|event| {
            matches!(
                event,
                ModelStreamEvent::TextDelta { .. } | ModelStreamEvent::ThinkingDelta { .. }
            )
        })
        .collect()
}

#[test]
fn anthropic_text_identity_is_dense_lazy_and_independent_of_native_and_thinking_indices() {
    let request = request();
    let mut state = anthropic_state(&request);
    state.accept(&json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"test-model","content":[],"stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0}}}), &request).unwrap();
    let blocks = [
        json!({"type":"text","text":""}),
        json!({"type":"thinking","thinking":"T","signature":"signed"}),
        json!({"type":"text","text":"A"}),
        json!({"type":"text","text":""}),
        json!({"type":"text","text":""}),
    ];
    let mut events = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        events.extend(visible(
            state
                .accept(
                    &json!({"type":"content_block_start","index":index,"content_block":block}),
                    &request,
                )
                .unwrap(),
        ));
    }
    for (index, text) in [(3, "B"), (2, "a")] {
        events.extend(visible(state.accept(&json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}}), &request).unwrap()));
    }
    assert_eq!(
        events,
        vec![
            ModelStreamEvent::ThinkingDelta {
                block: 0,
                text: "T".into()
            },
            ModelStreamEvent::TextDelta {
                block: 0,
                text: "A".into()
            },
            ModelStreamEvent::TextDelta {
                block: 1,
                text: "B".into()
            },
            ModelStreamEvent::TextDelta {
                block: 0,
                text: "a".into()
            },
        ]
    );
    for index in 0..blocks.len() {
        state
            .accept(
                &json!({"type":"content_block_stop","index":index}),
                &request,
            )
            .unwrap();
    }
    state.accept(&json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}), &request).unwrap();
    let events = state
        .accept(&json!({"type":"message_stop"}), &request)
        .unwrap();
    let ModelStreamEvent::Completed(response) = &events[0] else {
        panic!()
    };
    assert_eq!(
        response.message.content,
        vec![
            Content::Text("".into()),
            Content::Thinking("T".into()),
            Content::Text("Aa".into()),
            Content::Text("B".into()),
            Content::Text("".into())
        ]
    );
    let replay = &response.message.provider_replay.as_ref().unwrap().data["blocks"];
    assert_eq!(replay[1], blocks[1]);
    assert_eq!(replay[2]["text"], "Aa");
    assert_eq!(replay[3]["text"], "B");
}

#[test]
fn chat_content_deltas_retain_one_text_identity() {
    let mut state = ChatState::default();
    for text in ["first", " second"] {
        assert_eq!(
            visible(
                state
                    .accept(&json!({"choices":[{"index":0,"delta":{"content":text}}]}))
                    .unwrap()
            ),
            vec![ModelStreamEvent::TextDelta {
                block: 0,
                text: text.into()
            }]
        );
    }
}
