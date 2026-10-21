use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

async fn server(response: String) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&buf[..n]);
            assert!(bytes.len() < 16384);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let header = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let length = header
                    .lines()
                    .find_map(|s| s.strip_prefix("content-length: "))
                    .map(|s| s.parse::<usize>().unwrap())
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let _ = stream.write_all(response.as_bytes()).await;
        String::from_utf8(bytes).unwrap()
    });
    (url, task)
}
fn response(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nx-request-id: fixture-request\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn route(backend: SearchBackend, url: &str, key: Option<&str>) -> SearchRoute {
    let key = key.map(str::to_owned);
    let model = (!backend.direct()).then(|| ModelRef {
        provider: "bound-provider".into(),
        model: "bound-model".into(),
    });
    SearchRoute::new(backend, url, model, Arc::new(move || key.clone()), true).unwrap()
}
fn evidence() -> Value {
    json!({"url":"https://example.com/docs#section","title":"Docs","summary":"actual excerpt","description":"actual excerpt","highlights":["actual excerpt"]})
}

#[tokio::test]
async fn native_routes_preserve_profile_and_query_only_while_exposing_structured_sources() {
    for backend in [SearchBackend::Anthropic, SearchBackend::Openai] {
        let payload = if backend == SearchBackend::Anthropic {
            json!({"stop_reason":"end_turn","content":[{"type":"web_search_tool_result","content":[evidence()]}],"usage":{"input_tokens":13,"output_tokens":7,"server_tool_use":{"web_search_requests":1}}})
        } else {
            json!({"status":"completed","output":[{"type":"web_search_call","status":"completed","action":{"sources":[evidence()]}}],"usage":{"input_tokens":13,"output_tokens":7}})
        };
        let body = payload.to_string();
        let (url, task) = server(response("200 OK", &body)).await;
        let tools = SearchTools::new(
            Some(route(
                backend,
                &format!("{url}/bound/path"),
                Some("native-secret"),
            )),
            vec![route(
                SearchBackend::Exa,
                "http://127.0.0.1:1/never",
                Some("fallback-secret"),
            )],
        )
        .unwrap();
        let output = tools
            .search(
                &json!({"query":"current Rust docs","max_results":2}),
                CancellationToken::new(),
            )
            .await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["results"][0]["url"], evidence()["url"]);
        assert_eq!(
            output.value["source"]["execution_model"]["model"],
            "bound-model"
        );
        assert_eq!(output.value["source"]["native"], true);
        assert_eq!(
            output.value["source"]["body_sha256"],
            format!("{:x}", Sha256::digest(body.as_bytes()))
        );
        assert_eq!(output.value["source"]["usage"]["input_tokens"], 13);
        assert_eq!(output.value["untrusted"], true);
        let request = task.await.unwrap();
        let (header, body) = request.split_once("\r\n\r\n").unwrap();
        let body: Value = serde_json::from_str(body).unwrap();
        assert!(header.starts_with("POST /bound/path "));
        assert!(!request.contains("fallback-secret"));
        assert!(!output.value.to_string().contains("native-secret"));
        if backend == SearchBackend::Anthropic {
            assert!(
                header
                    .to_ascii_lowercase()
                    .contains("x-api-key: native-secret")
            );
            assert_eq!(
                body["messages"],
                json!([{"role":"user","content":"current Rust docs"}])
            );
            assert_eq!(body["tools"][0]["max_uses"], 1);
        } else {
            assert!(
                header
                    .to_ascii_lowercase()
                    .contains("authorization: bearer native-secret")
            );
            assert_eq!(body["input"], "current Rust docs");
            assert_eq!(body["store"], false);
            assert_eq!(body["max_tool_calls"], 1);
        }
        assert_eq!(body["model"], "bound-model");
    }
}
#[tokio::test]
async fn direct_search_contracts_keep_real_excerpts_and_use_only_the_chosen_credential() {
    for backend in [
        SearchBackend::Exa,
        SearchBackend::Deepseek,
        SearchBackend::Brave,
    ] {
        let payload = if backend == SearchBackend::Brave {
            json!({"web":{"results":[evidence()]}})
        } else {
            json!({"results":[evidence()],"costDollars":{"total":0.007}})
        };
        let (url, task) = server(response("200 OK", &payload.to_string())).await;
        let tools =
            SearchTools::new(None, vec![route(backend, &url, Some("direct-secret"))]).unwrap();
        let output = tools
            .search(
                &json!({"query":"a+b & c","max_results":3}),
                CancellationToken::new(),
            )
            .await;
        assert!(!output.is_error, "{}", output.value);
        assert_eq!(output.value["results"][0]["excerpt"], "actual excerpt");
        assert_eq!(output.value["source"]["native"], false);
        let request = task.await.unwrap();
        let (header, body) = request.split_once("\r\n\r\n").unwrap();
        if backend == SearchBackend::Brave {
            assert!(header.starts_with("GET /?q=a%2Bb+%26+c&count=3 "));
            assert!(
                header
                    .to_ascii_lowercase()
                    .contains("x-subscription-token: direct-secret")
            );
            assert!(!header.to_ascii_lowercase().contains("authorization:"));
        } else {
            let body: Value = serde_json::from_str(body).unwrap();
            assert_eq!(body["query"], "a+b & c");
            assert!(
                header
                    .to_ascii_lowercase()
                    .contains("authorization: bearer direct-secret")
            );
            if backend == SearchBackend::Exa {
                assert_eq!(body["numResults"], 3);
                assert_eq!(body["type"], "auto");
                assert_eq!(output.value["source"]["usage"]["reported_cost_usd"], 0.007);
            } else {
                assert_eq!(body["count"], 3);
                assert_eq!(body["need_content"], false);
            }
        }
    }
}
#[tokio::test]
async fn routing_is_preflight_only_and_explicit_provider_cannot_widen_it() {
    let (url, task) = server(response(
        "200 OK",
        &json!({"web":{"results":[]}}).to_string(),
    ))
    .await;
    let tools = SearchTools::new(
        None,
        vec![
            route(
                SearchBackend::Deepseek,
                "http://127.0.0.1:1/never",
                Some("ds"),
            ),
            route(SearchBackend::Exa, "http://127.0.0.1:1/never", None),
            route(SearchBackend::Brave, &url, Some("brave")),
        ],
    )
    .unwrap();
    let output = tools
        .search(&json!({"query":"x".repeat(129)}), CancellationToken::new())
        .await;
    assert!(!output.is_error, "{}", output.value);
    assert_eq!(output.value["source"]["backend"], "brave");
    assert_eq!(
        output.value["source"]["selection"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    task.await.unwrap();
    let output = tools
        .search(
            &json!({"query":"docs","provider":"native"}),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(output.value["error"]["kind"], "unavailable");
    let output = tools
        .search(
            &json!({"query":"docs","endpoint":"https://evil.invalid"}),
            CancellationToken::new(),
        )
        .await;
    assert_eq!(output.value["error"]["kind"], "invalid_input");
}
#[tokio::test]
async fn failures_after_dispatch_never_spend_another_provider_or_disclose_error_bodies() {
    let streamed = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
        MAX_BODY + 1,
        " ".repeat(MAX_BODY + 1)
    );
    for (wire, kind) in [
        (response("429 Too Many Requests", "{\"secret\":\"echoed-credential\"}"), "http_error"),
        ("HTTP/1.1 302 Found\r\nLocation: __SECOND__/redirect\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(), "http_error"),
        (response("200 OK", "{broken"), "invalid_response"),
        ("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 90\r\nConnection: close\r\n\r\n{}".into(), "network_error"),
        (format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1), "body_limit"),
        (streamed, "body_limit"),
    ] {
        let second = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let second_url = format!("http://{}", second.local_addr().unwrap());
        let (url, task) = server(wire.replace("__SECOND__", &second_url)).await;
        let tools = SearchTools::new(None, vec![
            route(SearchBackend::Exa, &url, Some("primary")),
            route(SearchBackend::Brave, &second_url, Some("secondary")),
        ]).unwrap();
        let output = tokio::time::timeout(Duration::from_secs(2),
            tools.search(&json!({"query":"docs"}), CancellationToken::new())
        ).await.expect("no hidden redirect or retry waiting for a second server");
        assert!(output.is_error);
        assert_eq!(output.value["error"]["kind"], kind, "{}", output.value);
        assert!(!output.value.to_string().contains("echoed-credential"));
        task.await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(50), second.accept()).await.is_err());
    }
}
#[tokio::test]
async fn cancellation_joins_request_custody_and_does_not_try_a_fallback() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (accepted, seen) = tokio::sync::oneshot::channel();
    let (release, wait) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut bytes = [0; 4096];
        assert!(stream.read(&mut bytes).await.unwrap() > 0);
        accepted.send(()).unwrap();
        let _ = wait.await;
    });
    let tools = Arc::new(
        SearchTools::new(None, vec![route(SearchBackend::Exa, &url, Some("secret"))]).unwrap(),
    );
    let stop = CancellationToken::new();
    let token = stop.clone();
    let call = tokio::spawn(async move { tools.search(&json!({"query":"docs"}), token).await });
    tokio::time::timeout(Duration::from_secs(5), seen)
        .await
        .unwrap()
        .unwrap();
    stop.cancel();
    let output = tokio::time::timeout(Duration::from_secs(1), call)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.value["error"]["kind"], "cancelled");
    release.send(()).unwrap();
    server.await.unwrap();
}
#[test]
fn malformed_or_incomplete_grounding_cannot_become_plausible_empty_results() {
    assert!(providers::normalize(SearchBackend::Exa, &json!({}), 5).is_err());
    assert!(
        providers::normalize(
            SearchBackend::Exa,
            &json!({"results":[{"url":"file:///private"}]}),
            5
        )
        .is_err()
    );
    assert!(providers::normalize(SearchBackend::Openai,&json!({"status":"completed","output":[{"type":"message","content":[{"text":"Visit https://invented.invalid/"}]}]}),5).is_err());
    assert!(
        providers::normalize(
            SearchBackend::Anthropic,
            &json!({"stop_reason":"pause_turn","content":[]}),
            5
        )
        .is_err()
    );
    let results=providers::normalize(SearchBackend::Exa,&json!({"results":[evidence(),{"url":"file:///private"}, {"url":"https://other.example/","title":"é".repeat(1000)}]}),1).unwrap();
    assert_eq!(results.entries.len(), 1);
    assert_eq!(results.discarded, 1);
    assert_eq!(results.omitted, 1);
    assert!(results.truncated);
}

#[test]
fn valid_custom_model_endpoints_are_not_rejected_for_lacking_native_search_paths() {
    let selected = Selection {
        provider: "openai".into(),
        model: "custom".into(),
        endpoint: "http://127.0.0.1:1/custom-model-handler".into(),
        wire: HttpWire::ChatCompletions,
        api_key_env: None,
        max_output_tokens: 1024,
        context_window_tokens: None,
        requires_key: false,
        image_input: false,
        capabilities: crate::catalog::ModelCapabilities::conservative(),
    };
    let tools = SearchTools::coding(
        &selected,
        &CredentialStore::new("unused-fixture-store"),
        &SearchSettings {
            native: true,
            fallbacks: vec![],
        },
    )
    .unwrap();
    assert!(tools.native.is_none());
}

#[tokio::test]
async fn native_deepseek_uses_profile_credentials_but_does_not_claim_model_execution() {
    let (url, task) = server(response(
        "200 OK",
        &json!({"results":[evidence()]}).to_string(),
    ))
    .await;
    let native = SearchRoute::new(
        SearchBackend::Deepseek,
        &url,
        Some(ModelRef {
            provider: "deepseek".into(),
            model: "active-model".into(),
        }),
        Arc::new(|| Some("bound-key".into())),
        true,
    )
    .unwrap();
    let tools = SearchTools::new(Some(native), vec![]).unwrap();
    let output = tools
        .search(&json!({"query":"current docs"}), CancellationToken::new())
        .await;
    assert!(!output.is_error, "{}", output.value);
    assert_eq!(
        output.value["source"]["profile_model"]["model"],
        "active-model"
    );
    assert!(output.value["source"]["execution_model"].is_null());
    let request = task.await.unwrap();
    let (_, body) = request.split_once("\r\n\r\n").unwrap();
    assert!(
        serde_json::from_str::<Value>(body)
            .unwrap()
            .get("model")
            .is_none()
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer bound-key")
    );
}
