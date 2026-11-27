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
    state.accept(&json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"test-model","content":[],"stop_reason":null,"usage":{"input_tokens":10,"output_tokens":0}}}), &OutputContract::from(&request)).unwrap();
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
                    &OutputContract::from(&request),
                )
                .unwrap(),
        ));
    }
    for (index, text) in [(3, "B"), (2, "a")] {
        events.extend(visible(state.accept(&json!({"type":"content_block_delta","index":index,"delta":{"type":"text_delta","text":text}}), &OutputContract::from(&request)).unwrap()));
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
                &OutputContract::from(&request),
            )
            .unwrap();
    }
    state.accept(&json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}), &OutputContract::from(&request)).unwrap();
    let events = state
        .accept(
            &json!({"type":"message_stop"}),
            &OutputContract::from(&request),
        )
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
fn chat_refusal_deltas_are_visible_assistant_text() {
    let mut state = ChatState::default();
    for part in ["I cannot ", "help with that 🦀."] {
        assert_eq!(
            visible(
                state
                    .accept(&json!({"choices":[{"index":0,"delta":{"refusal":part}}]}))
                    .unwrap()
            ),
            vec![ModelStreamEvent::TextDelta {
                block: 0,
                text: part.into()
            }]
        );
    }
    state
        .accept(&json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}))
        .unwrap();
    let response = state.complete(&OutputContract::from(&request())).unwrap();
    assert_eq!(
        response.message.content,
        vec![Content::Text("I cannot help with that 🦀.".into())]
    );
    assert_eq!(response.termination, ResponseTermination::Completed);
}

#[test]
fn chat_refusal_and_content_keep_distinct_dense_text_identities() {
    for fields in [["content", "refusal"], ["refusal", "content"]] {
        let mut state = ChatState::default();
        assert!(
            visible(
                state
                    .accept(&json!({"choices":[{"delta":{"content":"","refusal":null}}]}))
                    .unwrap()
            )
            .is_empty()
        );
        for (field, text, block) in [
            (fields[0], "A", 0),
            (fields[1], "B", 1),
            (fields[0], "a", 0),
        ] {
            assert_eq!(
                visible(
                    state
                        .accept(&json!({"choices":[{"delta":{field:text}}]}))
                        .unwrap()
                ),
                vec![ModelStreamEvent::TextDelta {
                    block,
                    text: text.into()
                }]
            );
        }
        state
            .accept(&json!({"choices":[{"delta":{},"finish_reason":"stop"}]}))
            .unwrap();
        state.accept(&json!({"choices":[{"delta":{"content":null,"refusal":""},"finish_reason":"stop"}]})).unwrap();
        assert_eq!(
            state
                .complete(&OutputContract::from(&request()))
                .unwrap()
                .message
                .content,
            vec![Content::Text("Aa".into()), Content::Text("B".into())]
        );
    }
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
