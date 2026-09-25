//! Loopback wire + native read through the public Session continuation.
use super::tests::{
    block, block_stop, delta, finish, http, identity, read_request, sse, start, stop, text_response,
};
use super::*;
use crate::{
    DriveExit, DrivePolicy, InputSender, LiveToolAuthority, NativeReadBoundary, ProviderBinding,
    ProviderCapabilities, ReturnedModelPolicy, Session, SubmitTurnRequest, SubmittedTurn,
    ToolBoundaries, ToolBoundary, TurnOutcome, workspace_registry::WorkspaceRegistry,
};
use std::{fs, io::Write, net::TcpListener, time::Duration};

#[tokio::test]
async fn session_native_read_replays_paired_result_and_reopen_does_not_reexecute() {
    let root = std::env::temp_dir().join(format!("ion-anthropic-read-{}", crate::SessionId::new()));
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
                start(),
                block(
                    0,
                    json!({"type":"tool_use","id":"toolu_original","name":"read","input":{}}),
                ),
                delta(
                    0,
                    json!({"type":"input_json_delta","partial_json":"{\"path\":\"data.txt\","}),
                ),
                delta(
                    0,
                    json!({"type":"input_json_delta","partial_json":"\"limit\":32}"}),
                ),
                block_stop(0),
                finish("tool_use", 3),
                stop(),
            ]),
            sse(&text_response()),
        ];
        let mut requests = Vec::new();
        for body in responses {
            let (mut socket, _) = listener.accept().unwrap();
            requests.push(read_request(&mut socket));
            socket.write_all(http(&body).as_bytes()).unwrap();
        }
        requests
    });
    let identity = identity(origin.clone());
    let provider = Arc::new(
        AnthropicMessages::test_local(
            identity.clone(),
            &format!("{origin}/v1/messages"),
            Arc::new(|| Some("test-key".into())),
        )
        .unwrap(),
    );
    let mut config = crate::config::tests::config();
    config.providers = vec![ProviderBinding {
        id: identity.binding.clone(),
        model: ion_ai::ModelRef {
            provider: "anthropic".into(),
            model: "claude-test".into(),
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
    let messages = requests[1].1["messages"].as_array().unwrap();
    let call_id = messages
        .iter()
        .flat_map(|m| m["content"].as_array().unwrap())
        .find_map(|b| (b["type"] == "tool_use").then(|| b["id"].as_str().unwrap()))
        .unwrap();
    assert!(call_id.starts_with("ion_"));
    assert_ne!(call_id, "toolu_original");
    assert!(messages.iter().any(|m| m["role"] == "user"
        && m["content"].as_array().unwrap().iter().any(|b| {
            b["type"] == "tool_result"
                && b["tool_use_id"] == call_id
                && b["content"].as_str().unwrap().contains("native data")
        })));
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
