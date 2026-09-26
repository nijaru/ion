//! Loopback wire + native file read through the public Session continuation.
use super::*;
use crate::{
    DriveExit, DrivePolicy, InputSender, LiveToolAuthority, NativeReadBoundary, ProviderBinding,
    ProviderCapabilities, ReturnedModelPolicy, Session, SubmitTurnRequest, SubmittedTurn,
    ToolBoundaries, ToolBoundary, TurnOutcome, workspace_registry::WorkspaceRegistry,
};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};

fn sse(chunks: &[Value]) -> String {
    let mut body = String::new();
    for chunk in chunks {
        body.push_str("data: ");
        body.push_str(&chunk.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn read_request(stream: &mut std::net::TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let (end, content_length) = loop {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "request ended before headers");
        bytes.extend_from_slice(&chunk[..n]);
        assert!(bytes.len() < 1024 * 1024, "request escaped test capacity");
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .expect("JSON request has a content length");
            break (end + 4, content_length);
        }
    };
    while bytes.len() - end < content_length {
        let n = stream.read(&mut chunk).unwrap();
        assert!(n > 0, "request body ended early");
        bytes.extend_from_slice(&chunk[..n]);
    }
    String::from_utf8(bytes[end..end + content_length].to_vec()).unwrap()
}

#[tokio::test]
async fn loopback_provider_calls_native_read_and_replays_its_result_without_reexecution() {
    let root = std::env::temp_dir().join(format!("ion-wire-read-{}", crate::SessionId::new()));
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("data.txt"), "native data").unwrap();
    let mut registry = WorkspaceRegistry::open(root.join("host")).unwrap();
    let binding = registry.bind("workspace", &workspace, "native-v1").unwrap();
    let reader = Arc::new(NativeReadBoundary::new(&registry, binding.clone(), 64).unwrap());
    reader.set_live_authority(LiveToolAuthority::Allow);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let responses = [
            sse(&[
                json!({"model":"gpt-test","choices":[{"index":0,"delta":{"tool_calls":[
                    {"index":0,"id":"call_1","function":{"name":"read","arguments":"{\"path\":\"data.txt\",\"limit\":32}"}}
                ]},"finish_reason":"tool_calls"}]}),
                json!({"model":"gpt-test","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":3}}),
            ]),
            sse(&[
                json!({"model":"gpt-test","choices":[{"index":0,"delta":{"content":"read confirmed"},"finish_reason":"stop"}]}),
                json!({"model":"gpt-test","choices":[],"usage":{"prompt_tokens":7,"completion_tokens":2}}),
            ]),
        ];
        let mut requests = Vec::new();
        for body in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            requests.push(request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).unwrap();
            stream.write_all(body.as_bytes()).unwrap();
        }
        requests
    });
    let identity = ModelBoundaryIdentity {
        binding: crate::ProviderBindingId::new("openai-test").unwrap(),
        adapter: crate::SemanticCompatibilityId::new("openai-compatible-v1").unwrap(),
        request_encoding: crate::SemanticCompatibilityId::new("chat-completions-v1").unwrap(),
        egress: EgressRealm::Remote(origin.clone()),
    };
    let provider = Arc::new(
        OpenAiCompatible::new(
            identity.clone(),
            &origin,
            Arc::new(|| Some("test-key".into())),
        )
        .unwrap(),
    );
    let mut config = crate::config::tests::config();
    config.providers = vec![ProviderBinding {
        id: identity.binding.clone(),
        model: ion_ai::ModelRef {
            provider: "openai".into(),
            model: "gpt-test".into(),
        },
        adapter: identity.adapter.clone(),
        request_encoding: identity.request_encoding.clone(),
        replay_family: None,
        capabilities: ProviderCapabilities {
            max_input_tokens: 100_000,
            max_output_tokens: 8192,
            tools: true,
            parallel_tool_calls: false,
            structured_output: false,
            replay: false,
            reasoning: false,
        },
        returned_model: ReturnedModelPolicy::Exact,
        start_receipts: crate::StartReceiptCapability::None,
        egress: identity.egress.clone(),
    }];
    config.default_provider = identity.binding;
    config.authority.egress_realms.push(identity.egress);
    config.workspace = binding;
    config.tools = vec![reader.tool_binding().clone()];
    config.initial_tools = vec![reader.tool_binding().id.clone()];
    config.controls.parallel_tool_calls = false;
    let database = root.join("session.sqlite");
    let session = Session::create(&database, config).await.unwrap().session;
    let handle = session.handle();
    let turn = match handle
        .submit_turn(SubmitTurnRequest {
            conversation: session.primary_conversation(),
            sender: InputSender::User,
            request_key: None,
            text: "Read data.txt and confirm it".into(),
            admitted_at_unix_ms: 1,
            wall_deadline_unix_ms: None,
        })
        .await
        .unwrap()
    {
        SubmittedTurn::Created(started) => started.turn.id,
        SubmittedTurn::Replayed { .. } => panic!("new submit replayed"),
    };
    let models = crate::ModelBoundaries::new(
        [provider as Arc<dyn ModelBoundary>],
        Arc::new(|_: &ProviderBinding| Ok(())),
    )
    .unwrap();
    let tools = ToolBoundaries::new([reader as Arc<dyn ToolBoundary>]).unwrap();
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(10),
            handle.resume_with_tools(turn, models, tools, DrivePolicy::default())
        )
        .await
        .unwrap()
        .unwrap(),
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    let second: Value = serde_json::from_str(&requests[1]).unwrap();
    let messages = second["messages"].as_array().unwrap();
    let call_id = messages
        .iter()
        .find_map(|message| message["tool_calls"][0]["id"].as_str())
        .unwrap();
    assert!(call_id.starts_with("ion_"));
    assert!(messages.iter().any(|message| {
        message["role"] == "tool"
            && message["tool_call_id"] == call_id
            && message["content"]
                .as_str()
                .unwrap_or("")
                .contains("native data")
    }));
    session.close().await.unwrap();
    fs::remove_file(workspace.join("data.txt")).unwrap();
    let reopened = Session::open(&database).await.unwrap();
    assert!(matches!(
        reopened
            .handle()
            .resume(turn, crate::ModelBoundaries::default())
            .await
            .unwrap(),
        DriveExit::Settled(TurnOutcome::Completed { .. })
    ));
    reopened.close().await.unwrap();
    drop(registry);
    fs::remove_dir_all(root).unwrap();
}
