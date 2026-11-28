use super::super::{
    gemini::{GEMINI_REPLAY, project_parts},
    gemini_request::stream_url,
};
use super::*;
use crate::catalog::GEMINI_HYDROGEN;

fn request_for_gemini() -> ModelRequest {
    let mut request = request();
    request.route = ModelRoute::direct(
        ModelRef {
            provider: "google".into(),
            model: GEMINI_HYDROGEN.into(),
        },
        ModelRouteReason::UserRequest,
    );
    request
}

#[test]
fn gemini_api_base_cannot_override_the_selected_model_or_carry_a_query() {
    let wire = HttpWire::GeminiGenerateContent;
    let base = HttpModelService::resolve_endpoint("https://example.invalid/proxy/", wire).unwrap();
    assert_eq!(base, "https://example.invalid/proxy/v1beta");
    let base = Url::parse(&base).unwrap();
    assert_eq!(
        stream_url(&base, GEMINI_HYDROGEN).unwrap().as_str(),
        "https://example.invalid/proxy/v1beta/models/gemini-4-hydrogen:streamGenerateContent?alt=sse"
    );
    for id in [
        "../models/other",
        "",
        "..",
        "a:generateContent",
        "foo?key=private",
        "a%2fb",
    ] {
        assert!(stream_url(&base, id).is_err(), "{id}");
    }
    for address in [
        "https://example.invalid/v1beta?key=bad",
        "https://example.invalid/v1beta/models/other:streamGenerateContent",
    ] {
        assert!(HttpModelService::resolve_endpoint(address, wire).is_err());
    }
}

#[test]
fn gemini_native_replay_keeps_signed_part_boundaries_and_checks_projection() {
    let mut r = request_for_gemini();
    let parts = vec![
        json!({"text":"Hel","thoughtSignature":"YQ=="}),
        json!({"text":"lo"}),
        json!({"text":"thought summary","thought":true,"thoughtSignature":"Yg=="}),
        json!({"functionCall":{"name":"read","id":"wire","args":{"path":"a"}},"thoughtSignature":"Yw=="}),
    ];
    let content: Vec<_> = project_parts(&parts)
        .unwrap()
        .into_iter()
        .filter(|part| !matches!(part, Content::Thinking(_)))
        .collect();
    assert_eq!(content[0], Content::Text("Hello".into()));
    assert!(matches!(&content[1], Content::ToolCall(call) if call.id == "gemini-3"));
    r.messages.push(Message {
        role: Role::Assistant,
        content,
        provider_replay: Some(ProviderReplay::new(
            "google",
            GEMINI_REPLAY,
            json!({"model":GEMINI_HYDROGEN,"parts":parts}),
        )),
    });
    let image = ion_ai::ImageContent::from_bytes(&[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ])
    .unwrap();
    r.messages[0].content.push(Content::Image(image.clone()));
    r.messages.push(Message {
        role: Role::Tool,
        provider_replay: None,
        content: vec![Content::ToolResult(ToolResult {
            call_id: "gemini-3".into(),
            name: "read".into(),
            result: json!("refused"),
            is_error: true,
            images: vec![image.clone()],
        })],
    });
    let body = gemini_body(&r).unwrap();
    assert_eq!(
        body["contents"][0]["parts"][1],
        json!({"inlineData":{
            "mimeType":"image/png","data":image.data()
        }})
    );
    assert_eq!(body["contents"][1]["parts"], json!(parts));
    assert_eq!(
        body["contents"][2],
        json!({"role":"user","parts":[{"functionResponse":{
            "name":"read", "id":"wire", "response":{"error":"refused","images":[{"$ref":"ion-image-0"}]},
            "parts":[{"inlineData":{"mimeType":"image/png","data":image.data(),"displayName":"ion-image-0"}}]
        }}]})
    );
    r.route.effective.model = "other".into();
    assert_eq!(
        gemini_body(&r).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
    r.route.effective.model = GEMINI_HYDROGEN.into();
    r.messages[1].content[0] = Content::Text("tampered".into());
    assert_eq!(
        gemini_body(&r).unwrap_err().kind,
        ProviderErrorKind::InvalidRequest
    );
    r.messages[1].provider_replay = None;
    assert_eq!(
        gemini_body(&r).unwrap_err().kind,
        ProviderErrorKind::Unsupported
    );
}

#[test]
fn gemini_finish_and_accounting_do_not_promote_partial_calls() {
    let r = request_for_gemini();
    let output = OutputContract::from(&r);
    let mut state = GeminiState::default();
    let offered = json!({"candidates":[{"content":{"parts":[{
        "functionCall":{"name":"read","args":{}},"thoughtSignature":"YQ=="
    }]}}]});
    assert_eq!(
        state.accept(&offered, &output).unwrap(),
        vec![ModelStreamEvent::OutputObserved]
    );
    assert!(
        state
            .accept(
                &json!({"candidates":[{"finishReason":"MAX_TOKENS"}]}),
                &output
            )
            .is_err()
    );
    for bad in [
        json!({"candidates":[{},{}]}),
        json!({"candidates":[{"index":1,"finishReason":"STOP"}]}),
        json!({"candidates":[{"content":{"parts":[{"functionCall":{"name":"read","willContinue":true}}]}}]}),
    ] {
        assert!(GeminiState::default().accept(&bad, &output).is_err());
    }
    let mut state = GeminiState::default();
    state.accept(&json!({"usageMetadata":{"promptTokenCount":20,"cachedContentTokenCount":10,"thoughtsTokenCount":3}}), &output).unwrap();
    let events = state
        .accept(
            &json!({"usageMetadata":{"candidatesTokenCount":4},"candidates":[{
                "content":{"parts":[{"text":"partial"}]},"finishReason":"MAX_TOKENS"
            }]}),
            &output,
        )
        .unwrap();
    let ModelStreamEvent::Completed(response) = events.last().unwrap() else {
        panic!("missing terminal event")
    };
    assert_eq!(
        response.termination,
        ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
    );
    assert_eq!(response.usage.input_tokens, Some(20));
    assert_eq!(response.usage.cache_read_input_tokens, Some(10));
    assert_eq!(response.usage.output_tokens, Some(7));
}
