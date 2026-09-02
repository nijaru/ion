//! ACP frontend integration tests: drive `acp::serve` over in-memory
//! duplex streams with a scripted provider, asserting the v1 wire
//! contract end to end (initialize, session/new, session/prompt with
//! streamed updates).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use ion::scripted_provider_factory;
use ion_core::{AllowlistPolicy, SessionStore};

struct TestClient {
    write: tokio::io::DuplexStream,
    read: tokio::io::ReadHalf<tokio::io::DuplexStream>,
    buf: Vec<u8>,
}

impl TestClient {
    async fn send(&mut self, message: Value) {
        self.write
            .write_all(format!("{message}\n").as_bytes())
            .await
            .expect("write request");
        self.write.flush().await.expect("flush");
    }

    /// Next JSON-RPC message the agent emits, with a guard timeout.
    async fn recv(&mut self) -> Value {
        let deadline = Duration::from_secs(5);
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=pos).collect();
                return serde_json::from_slice(&line).expect("valid json line");
            }
            let mut chunk = [0u8; 4096];
            let read = tokio::time::timeout(deadline, self.read.read(&mut chunk))
                .await
                .expect("recv timeout")
                .expect("read");
            assert!(read > 0, "agent closed the connection");
            self.buf.extend_from_slice(&chunk[..read]);
        }
    }

    /// Skip updates until the response for `id` arrives; returns the
    /// collected update payloads in order.
    async fn collect_until_response(&mut self, id: u64) -> (Value, Vec<Value>) {
        let mut updates = Vec::new();
        loop {
            let message = self.recv().await;
            if message.get("id").and_then(|v| v.as_u64()) == Some(id)
                && (message.get("result").is_some() || message.get("error").is_some())
            {
                return (message, updates);
            }
            if message.get("method").and_then(|v| v.as_str()) == Some("session/update") {
                updates.push(message["params"]["update"].clone());
            }
        }
    }

    fn text_chunks(updates: &[Value]) -> String {
        updates
            .iter()
            .filter(|u| u["sessionUpdate"] == "agent_message_chunk")
            .filter_map(|u| u["content"]["text"].as_str())
            .collect()
    }
}

async fn start_agent() -> TestClient {
    let (client, _task) =
        start_agent_with_store(Arc::new(SessionStore::open_in_memory().expect("store"))).await;
    client
}

async fn start_agent_with_store(
    store: Arc<SessionStore>,
) -> (TestClient, tokio::task::JoinHandle<std::io::Result<()>>) {
    let (client_write, server_in) = tokio::io::duplex(64 * 1024);
    let (server_out, client_read) = tokio::io::duplex(64 * 1024);
    // The server only ever writes its output stream.
    let (_unused_read, server_write) = tokio::io::split(server_out);
    let config = ion::AcpConfig {
        make_provider: scripted_provider_factory(vec![
            ion_core::ScriptedMessage::text("hello "),
            ion_core::ScriptedMessage::text("world"),
        ]),
        store,
        policy: Arc::new(AllowlistPolicy::new(["read"])),
        trust_project: false,
        agents_enabled: false,
    };
    let task = tokio::spawn(async move { ion::acp::serve(server_in, server_write, config).await });
    let (read, _unused_write) = tokio::io::split(client_read);
    (
        TestClient {
            write: client_write,
            read,
            buf: Vec::new(),
        },
        task,
    )
}

#[tokio::test]
async fn acp_initialize_session_and_prompt_turn() {
    let mut client = start_agent().await;

    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": { "protocolVersion": 1, "clientCapabilities": {} },
        }))
        .await;
    let init = client.recv().await;
    assert_eq!(init["result"]["protocolVersion"], 1);
    assert_eq!(init["result"]["agentInfo"]["name"], "ion");

    let cwd = std::env::temp_dir();
    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": { "cwd": cwd.to_string_lossy(), "mcpServers": [] },
        }))
        .await;
    let created = client.recv().await;
    let session_id = created["result"]["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_owned();

    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "say hello" }],
            },
        }))
        .await;
    let (response, updates) = client.collect_until_response(2).await;
    assert_eq!(response["result"]["stopReason"], "end_turn", "{response}");
    assert_eq!(TestClient::text_chunks(&updates), "hello world");
}

#[tokio::test]
async fn acp_load_replays_durable_history_and_accepts_a_new_prompt() {
    let store = Arc::new(SessionStore::open_in_memory().expect("store"));
    let (mut client, first_task) = start_agent_with_store(Arc::clone(&store)).await;
    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": { "protocolVersion": 1, "clientCapabilities": {} },
        }))
        .await;
    let _ = client.recv().await;
    // Deliberately choose a workspace other than the process cwd. ACP owns
    // this identity; the runtime must persist it for the later load.
    let cwd = std::env::temp_dir();
    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "session/new",
            "params": { "cwd": cwd.to_string_lossy(), "mcpServers": [] },
        }))
        .await;
    let session_id = client.recv().await["result"]["sessionId"]
        .as_str()
        .expect("session id")
        .to_owned();
    client
        .send(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "persist this" }],
            },
        }))
        .await;
    let (prompt_response, _) = client.collect_until_response(2).await;
    assert_eq!(prompt_response["result"]["stopReason"], "end_turn");
    drop(client);
    first_task
        .await
        .expect("first server task")
        .expect("first server");

    let (mut resumed, second_task) = start_agent_with_store(store).await;
    resumed
        .send(json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": { "protocolVersion": 1, "clientCapabilities": {} },
        }))
        .await;
    let init = resumed.recv().await;
    assert_eq!(init["result"]["agentCapabilities"]["loadSession"], true);
    resumed
        .send(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "session/load",
            "params": { "sessionId": session_id, "cwd": cwd.to_string_lossy(), "mcpServers": [] },
        }))
        .await;
    let (load_response, updates) = resumed.collect_until_response(3).await;
    assert_eq!(load_response["result"], json!({}));
    assert!(updates.iter().any(|update| {
        update["sessionUpdate"] == "user_message_chunk"
            && update["content"]["text"] == "persist this"
    }));
    assert_eq!(TestClient::text_chunks(&updates), "hello world");

    resumed
        .send(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "session/prompt",
            "params": {
                "sessionId": session_id,
                "prompt": [{ "type": "text", "text": "continue" }],
            },
        }))
        .await;
    let (response, updates) = resumed.collect_until_response(4).await;
    assert_eq!(response["result"]["stopReason"], "end_turn");
    assert_eq!(TestClient::text_chunks(&updates), "hello world");
    drop(resumed);
    second_task
        .await
        .expect("second server task")
        .expect("second server");
}

#[tokio::test]
async fn acp_unknown_method_is_a_jsonrpc_error() {
    let mut client = start_agent().await;
    client
        .send(json!({ "jsonrpc": "2.0", "id": 7, "method": "nope/method" }))
        .await;
    let response = client.recv().await;
    assert_eq!(response["error"]["code"], -32601);
}
