use super::*;
use conversion::convert_tool_result;
use ion_ai::ToolCall;
use rmcp::model::CallToolResult;
use std::path::PathBuf;

const SERVER: &str = r#"
import json, os, sys, time
from pathlib import Path
mode = sys.argv[1]
Path('pid').write_text(str(os.getpid()))
calls = 0
for line in sys.stdin:
    request = json.loads(line)
    if request.get('method') == 'notifications/cancelled':
        Path('cancelled').write_text(json.dumps(request['params']))
        continue
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        capabilities = {'tools': {}}
        if mode == 'shutdown':
            capabilities['resources'] = {}
        result = {'protocolVersion': request['params']['protocolVersion'],
                  'capabilities': capabilities, 'serverInfo': {'name': 'custody', 'version': '1'}}
    elif method == 'tools/list':
        result = {'tools': [{'name': 'probe', 'inputSchema': {'type': 'object'}}]}
    elif method == 'resources/read':
        with Path('calls').open('a') as observed:
            observed.write(json.dumps(request) + '\n')
        continue
    elif method == 'tools/call':
        calls += 1
        with Path('calls').open('a') as observed:
            observed.write(json.dumps(request) + '\n')
        if request['params'].get('arguments', {}).get('wait'):
            if mode == 'parallel':
                Path('blocked').touch()
                while not Path('resume').exists():
                    time.sleep(0.01)
            continue
        if mode == 'mrtr' and calls == 1:
            result = {'resultType': 'input_required', 'requestState': 'continue'}
        else:
            result = {'content': [], 'structuredContent': {'calls': calls}}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
    if mode == 'pressure' and method == 'tools/list':
        Path('blocked').touch()
        while not Path('resume').exists():
            time.sleep(0.01)
"#;

async fn fixture(mode: &str) -> (PathBuf, Arc<McpTools>, ion_core::ToolSet) {
    let root = std::env::temp_dir().join(format!("ion-mcp-invoke-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    config
        .add(
            "custody",
            McpServer::Stdio(McpStdioServer {
                command: "python3".into(),
                args: vec!["-c".into(), SERVER.into(), mode.into()],
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &root).await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let source = startup.tools.unwrap();
    let tools = ion_core::ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]);
    (root, source, tools)
}

async fn wait_file(path: &Path) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn cancellation_notifies_the_issued_request_and_keeps_a_healthy_connection() {
    let (root, source, tools) = fixture("cancel").await;
    let issued = tools.snapshot();
    let name = issued.specs()[0].name.clone();
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let task_name = name.clone();
    let work = tokio::spawn(async move {
        issued
            .execute_untracked(
                &ToolCall {
                    id: "stopped".into(),
                    name: task_name,
                    arguments: json!({"wait":true}),
                    raw_arguments: None,
                },
                task_stop,
            )
            .await
    });
    wait_file(&root.join("calls")).await;
    stop.cancel();
    let output = tokio::time::timeout(Duration::from_secs(12), work)
        .await
        .unwrap()
        .unwrap();
    let issued = tools.snapshot();
    let next = issued
        .execute_untracked(
            &ToolCall {
                id: "next".into(),
                name,
                arguments: json!({}),
                raw_arguments: None,
            },
            CancellationToken::new(),
        )
        .await;
    source.shutdown().await.unwrap();
    let requests: Vec<Value> = fs::read_to_string(root.join("calls"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let cancelled = fs::read(root.join("cancelled"))
        .ok()
        .map(|bytes| serde_json::from_slice::<Value>(&bytes).unwrap());
    fs::remove_dir_all(root).unwrap();
    assert!(output.is_error, "{}", output.value);
    assert!(!next.is_error, "{}", next.value);
    assert_eq!(requests.len(), 2);
    assert_eq!(
        cancelled.expect("issued request was not cancelled")["requestId"],
        requests[0]["id"]
    );
}

#[tokio::test]
async fn public_shutdown_retires_unanswered_tools_and_resources_without_caller_cancellation() {
    for resource in [false, true] {
        let (root, source, tools) = fixture("shutdown").await;
        let call = ToolCall {
            id: "unanswered".into(),
            name: if resource {
                "read_mcp_resource".into()
            } else {
                "mcp__7_custody__probe".into()
            },
            arguments: if resource {
                json!({"server":"custody", "uri":"fixture://unanswered"})
            } else {
                json!({"wait":true})
            },
            raw_arguments: None,
        };
        let issued = tools.snapshot();
        let late_issued = tools.snapshot();
        let work_call = call.clone();
        let stop = CancellationToken::new();
        let work_stop = stop.clone();
        let mut work =
            tokio::spawn(async move { issued.execute_untracked(&work_call, work_stop).await });
        let entered = tokio::time::timeout(Duration::from_secs(2), async {
            while !root.join("calls").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        // Retain both closing futures on timeout. Dropping shutdown mid-cleanup
        // would itself abandon its owned join; cancellation here is only rescue
        // for the old deadlock, not part of the successful contract.
        let closing = async { tokio::join!(source.shutdown(), source.shutdown()) };
        tokio::pin!(closing);
        let early = tokio::time::timeout(Duration::from_secs(2), &mut closing).await;
        let independent = early.is_ok();
        let closed = match early {
            Ok(result) => result,
            Err(_) => {
                stop.cancel();
                tokio::time::timeout(Duration::from_secs(5), &mut closing)
                    .await
                    .expect("cancelled invocation did not release shutdown")
            }
        };
        let joined = tokio::time::timeout(Duration::from_secs(2), &mut work).await;
        if joined.is_err() {
            work.abort();
            let _ = work.await;
        }
        let late = late_issued
            .execute_untracked(&call, CancellationToken::new())
            .await;
        let requests = fs::read_to_string(root.join("calls")).unwrap();
        let pid = fs::read_to_string(root.join("pid")).unwrap();
        let process = tokio::process::Command::new("ps")
            .args(["-p", &pid, "-o", "pid="])
            .output()
            .await
            .unwrap();
        fs::remove_dir_all(root).unwrap();

        entered.expect("request did not reach the real server");
        closed.0.unwrap();
        closed.1.unwrap();
        let output = joined.expect("invocation outlived shutdown").unwrap();
        assert!(process.stderr.is_empty(), "ps failed: {process:?}");
        assert!(
            process.stdout.is_empty(),
            "MCP root PID {pid} was not reaped"
        );
        assert!(
            independent,
            "public shutdown waited for caller cancellation (resource: {resource})"
        );
        assert!(!stop.is_cancelled());
        assert!(
            output.is_error && output.value["error"].as_str().unwrap().contains("unknown"),
            "{}",
            output.value
        );
        assert!(
            late.is_error && late.value["error"].as_str().unwrap().contains("is closed"),
            "{}",
            late.value
        );
        assert_eq!(
            requests.lines().count(),
            1,
            "shutdown or a stale capability reissued the request"
        );
    }
}

#[tokio::test]
async fn input_required_does_not_issue_an_uncommitted_continuation() {
    let (root, source, tools) = fixture("mrtr").await;
    let issued = tools.snapshot();
    let output = issued
        .execute_untracked(
            &ToolCall {
                id: "one".into(),
                name: issued.specs()[0].name.clone(),
                arguments: json!({}),
                raw_arguments: None,
            },
            CancellationToken::new(),
        )
        .await;
    source.shutdown().await.unwrap();
    let count = fs::read_to_string(root.join("calls"))
        .unwrap()
        .lines()
        .count();
    fs::remove_dir_all(root).unwrap();
    assert_eq!(
        count, 1,
        "one Ion invocation issued multiple tools/call requests"
    );
    assert!(
        output.is_error,
        "unsupported continuation became success: {}",
        output.value
    );
}

#[tokio::test]
async fn cancellation_settles_backpressured_local_issuance_before_returning() {
    let (root, source, tools) = fixture("pressure").await;
    wait_file(&root.join("blocked")).await;
    let issued = tools.snapshot();
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let work = tokio::spawn(async move {
        issued
            .execute_untracked(
                &ToolCall {
                    id: "pressured".into(),
                    name: issued.specs()[0].name.clone(),
                    arguments: json!({"payload":"x".repeat(4 * 1024 * 1024)}),
                    raw_arguments: None,
                },
                task_stop,
            )
            .await
    });
    // The root is deliberately not reading; a complete request cannot fit in
    // the native pipe. Release it only after the executor claims settlement.
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.cancel();
    let output = tokio::time::timeout(Duration::from_secs(12), work)
        .await
        .unwrap()
        .unwrap();
    fs::write(root.join("resume"), b"").unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let issued_after_settlement = root.join("calls").exists();
    source.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(output.is_error, "{}", output.value);
    assert!(
        !issued_after_settlement,
        "local tools/call issuance continued after executor settlement"
    );
}

#[tokio::test]
async fn forced_retirement_joins_an_independently_running_invocation() {
    let (root, source, tools) = fixture("parallel").await;
    let issued = tools.snapshot();
    let other = tokio::spawn(async move {
        issued
            .execute_untracked(
                &ToolCall {
                    id: "independent".into(),
                    name: issued.specs()[0].name.clone(),
                    arguments: json!({"wait":true}),
                    raw_arguments: None,
                },
                CancellationToken::new(),
            )
            .await
    });
    wait_file(&root.join("blocked")).await;
    let issued = tools.snapshot();
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let work = tokio::spawn(async move {
        issued
            .execute_untracked(
                &ToolCall {
                    id: "pressured".into(),
                    name: issued.specs()[0].name.clone(),
                    arguments: json!({"payload":"x".repeat(4 * 1024 * 1024)}),
                    raw_arguments: None,
                },
                task_stop,
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop.cancel();
    let stopped = tokio::time::timeout(Duration::from_secs(12), work)
        .await
        .unwrap()
        .unwrap();
    let retired = tokio::time::timeout(Duration::from_secs(1), other)
        .await
        .unwrap()
        .unwrap();
    let calls = fs::read_to_string(root.join("calls"))
        .unwrap()
        .lines()
        .count();
    source.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(stopped.is_error && retired.is_error);
    assert_eq!(calls, 1, "retirement issued the incomplete second request");
}

#[tokio::test]
async fn cancellation_closes_a_pending_http_post_before_executor_settlement() {
    use std::process::Stdio;
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::Command,
    };
    let root = std::env::temp_dir().join(format!("ion-mcp-http-call-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let server = r#"
import json
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        method = request.get('method')
        if method == 'tools/call':
            Path('called').write_text(json.dumps(request))
            try:
                closed = not self.connection.recv(1)
            except ConnectionResetError:
                closed = True
            if closed:
                Path('closed').touch()
            return
        if method == 'initialize':
            result = {'protocolVersion': request['params']['protocolVersion'],
                      'capabilities': {'tools': {}}, 'serverInfo': {'name': 'http', 'version': '1'}}
        elif method == 'tools/list':
            result = {'tools': [{'name': 'probe', 'inputSchema': {'type': 'object'}}]}
        else:
            self.send_response(202)
            self.end_headers()
            return
        payload = json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload)))
        self.send_header('Mcp-Session-Id', 'fixture')
        self.end_headers()
        self.wfile.write(payload)
    def do_GET(self):
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        self.wfile.flush()
        Path('get-open').touch()
        try:
            closed = not self.connection.recv(1)
        except ConnectionResetError:
            closed = True
        if closed:
            Path('get-closed').touch()
    def do_DELETE(self):
        self.send_response(200)
        self.end_headers()
    def log_message(self, *args): pass
server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
server.serve_forever()
"#;
    let mut process = Command::new("python3")
        .args(["-c", server])
        .current_dir(&root)
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut port = String::new();
    BufReader::new(process.stdout.take().unwrap())
        .read_line(&mut port)
        .await
        .unwrap();
    let config = McpConfig::new(&root);
    config
        .add(
            "http",
            McpServer::Http(McpHttpServer {
                url: format!("http://127.0.0.1:{}/mcp", port.trim()),
                bearer_token_env: None,
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &root).await;
    let source = startup.tools.unwrap();
    let issued = ion_core::ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    let work = tokio::spawn(async move {
        issued
            .execute_untracked(
                &ToolCall {
                    id: "http".into(),
                    name: issued.specs()[0].name.clone(),
                    arguments: json!({}),
                    raw_arguments: None,
                },
                task_stop,
            )
            .await
    });
    wait_file(&root.join("called")).await;
    wait_file(&root.join("get-open")).await;
    stop.cancel();
    let output = tokio::time::timeout(Duration::from_secs(12), work)
        .await
        .unwrap()
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(1), async {
        while !root.join("closed").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok();
    // A healthy notification GET belongs to the connection, not the stopped
    // invocation. Check its disposal at the correct ownership boundary.
    source.shutdown().await.unwrap();
    let get_closed = tokio::time::timeout(Duration::from_secs(1), async {
        while !root.join("get-closed").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok();
    process.kill().await.unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(output.is_error, "{}", output.value);
    assert!(
        closed,
        "local HTTP POST remained pending after executor settlement"
    );
    assert!(
        get_closed,
        "connection shutdown did not join its notification SSE body"
    );
}

#[tokio::test]
async fn oversized_http_event_preserves_the_framing_reason_for_tools_and_resources() {
    use std::process::Stdio;
    use tokio::{
        io::{AsyncBufReadExt, BufReader},
        process::Command,
    };
    let root = std::env::temp_dir().join(format!("ion-mcp-http-sse-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let server = r#"
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        method = request.get('method')
        if method in ('tools/call', 'resources/read', 'resources/list', 'resources/templates/list'):
            with Path('calls').open('a') as observed:
                observed.write(method + '\n')
            body = b'data: ' + b'x' * (17 * 1024 * 1024) + b'\n\n'
            try:
                self.send_response(200)
                self.send_header('Content-Type', 'text/event-stream')
                self.send_header('Mcp-Session-Id', 'fixture')
                self.end_headers()
                self.wfile.write(body)
                self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError):
                pass
            return
        if method == 'initialize':
            result = {'protocolVersion': request['params']['protocolVersion'],
                      'capabilities': {'tools': {}, 'resources': {}}, 'serverInfo': {'name': 'http', 'version': '1'}}
        elif method == 'tools/list':
            result = {'tools': [{'name': 'probe', 'inputSchema': {'type': 'object'}}]}
        else:
            self.send_response(202)
            self.send_header('Content-Length', '0')
            self.end_headers()
            return
        payload = json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(payload)))
        self.send_header('Mcp-Session-Id', 'fixture')
        self.end_headers()
        self.wfile.write(payload)
    def log_message(self, *args): pass

server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
server.serve_forever()
"#;
    let mut process = Command::new("python3")
        .args(["-c", server])
        .current_dir(&root)
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut port = String::new();
    BufReader::new(process.stdout.take().unwrap())
        .read_line(&mut port)
        .await
        .unwrap();
    let config = McpConfig::new(&root);
    config
        .add(
            "http",
            McpServer::Http(McpHttpServer {
                url: format!("http://127.0.0.1:{}/mcp", port.trim()),
                bearer_token_env: None,
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let mut outputs = Vec::new();
    for (name, arguments) in [
        ("mcp__4_http__probe", json!({})),
        (
            "read_mcp_resource",
            json!({"server":"http", "uri":"fixture://oversized"}),
        ),
        ("list_mcp_resources", json!({"server":"http"})),
        ("list_mcp_resource_templates", json!({"server":"http"})),
    ] {
        // A framing failure retires its connection. Each operation gets a fresh
        // connection to the same real server, not a retry of the failed request.
        let startup = McpTools::connect(&config, &root).await;
        assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
        let source = startup.tools.unwrap();
        let issued =
            ion_core::ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]).snapshot();
        let call = ToolCall {
            id: "http-sse".into(),
            name: name.into(),
            arguments,
            raw_arguments: None,
        };
        let mut work = issued.execute_untracked(&call, CancellationToken::new());
        assert!(futures_util::poll!(&mut work).is_pending());
        let server = &source.servers[0];
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let lease = server.client.read().await;
                let connection = lease.as_ref().unwrap();
                if connection.transport_failure().is_some() {
                    break;
                }
                drop(lease);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("oversized response did not retire the transport");
        // The invocation still holds its read lease and has not observed the
        // failed response. Queue retirement's writer before polling it again.
        let closing = source.shutdown();
        tokio::pin!(closing);
        assert!(futures_util::poll!(&mut closing).is_pending());
        tokio::time::timeout(Duration::from_secs(2), async {
            while server.client.try_read().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("retirement did not queue its connection write lease");
        let (closed, output) = tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(closing, work)
        })
        .await
        .expect("oversized SSE response left the call waiting");
        closed.unwrap();
        outputs.push((name, output));
    }
    process.kill().await.unwrap();
    let calls = fs::read_to_string(root.join("calls")).unwrap();
    fs::remove_dir_all(root).unwrap();
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        [
            "tools/call",
            "resources/read",
            "resources/list",
            "resources/templates/list",
        ]
    );
    // The SDK reports a generic transport closure for a terminated stream;
    // the recorded framing reason must reach every model-facing operation.
    for (name, output) in outputs {
        assert!(output.is_error, "{name}: {:?}", output.value);
        let error = output.value["error"].as_str().unwrap_or_default();
        assert!(error.contains("framing failed"), "{name}: {error}");
    }
}

#[test]
fn partial_conversion_keeps_supported_text_structured_and_outcome() {
    let result: CallToolResult = serde_json::from_value(json!({
        "content": [
            {"type": "text", "text": "kept"},
            {"type": "audio", "data": "AAAA", "mimeType": "audio/wav"},
            {"type": "resource_link", "uri": "mem://report", "name": "report"},
            {"type": "text", "text": "also kept"}
        ],
        "structuredContent": {"rows": 2}
    }))
    .unwrap();
    let output = convert_tool_result("mixed", result);
    assert!(!output.is_error, "{}", output.value);
    assert_eq!(output.value["is_error"], false, "{}", output.value);
    assert_eq!(output.value["content"], "kept\nalso kept");
    assert_eq!(output.value["structured_content"], json!({"rows": 2}));
    assert_eq!(output.value["retained_blocks"], 2);
    assert_eq!(output.value["omitted_blocks"], 2);
    let notices = output.value["conversion_notices"].as_array().unwrap();
    assert_eq!(notices.len(), 2);
    assert!(notices[0].as_str().unwrap().contains("block 1"));
    assert!(notices[1].as_str().unwrap().contains("block 2"));
    assert!(notices[1].as_str().unwrap().contains("resource link"));
    assert!(output.images.is_empty());
    let raw = output.value["raw_result_path"].as_str().unwrap();
    assert!(
        output.value["raw_result_error"].is_null(),
        "{}",
        output.value
    );
    let saved: Value = serde_json::from_slice(&fs::read(raw).unwrap()).unwrap();
    assert_eq!(saved["content"].as_array().unwrap().len(), 4);
    assert_eq!(saved["content"][2]["uri"], "mem://report");
    assert_eq!(saved["structuredContent"], json!({"rows": 2}));
    fs::remove_file(raw).unwrap();
}

#[test]
fn invalid_image_omission_keeps_following_content() {
    let result: CallToolResult = serde_json::from_value(json!({
        "content": [
            {"type": "image", "data": "%%%", "mimeType": "image/png"},
            {"type": "text", "text": "still here"}
        ]
    }))
    .unwrap();
    let output = convert_tool_result("picture", result);
    assert!(!output.is_error, "{}", output.value);
    assert_eq!(output.value["content"], "still here");
    assert!(
        output.images.is_empty(),
        "invalid image must not be delivered"
    );
    let notices = output.value["conversion_notices"].as_array().unwrap();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].as_str().unwrap().contains("block 0"));
    assert!(notices[0].as_str().unwrap().contains("base64"));
    assert_eq!(output.value["retained_blocks"], 1);
    assert_eq!(output.value["omitted_blocks"], 1);
    let raw = output.value["raw_result_path"].as_str().unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(raw).unwrap()).unwrap();
    assert_eq!(saved["content"].as_array().unwrap().len(), 2);
    fs::remove_file(raw).unwrap();
}

#[test]
fn server_execution_error_is_not_replaced_by_conversion_notices() {
    let result: CallToolResult = serde_json::from_value(json!({
        "content": [
            {"type": "text", "text": "partial"},
            {"type": "audio", "data": "AAAA", "mimeType": "audio/wav"}
        ],
        "isError": true
    }))
    .unwrap();
    let output = convert_tool_result("broken", result);
    assert!(output.is_error, "{}", output.value);
    assert_eq!(output.value["is_error"], true);
    assert!(
        output.value["content"]
            .as_str()
            .unwrap()
            .contains("partial")
    );
    assert_eq!(output.value["omitted_blocks"], 1);
    let raw = output.value["raw_result_path"].as_str().unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(raw).unwrap()).unwrap();
    assert_eq!(saved["isError"], true);
    fs::remove_file(raw).unwrap();
}

#[test]
fn truncated_conversion_keeps_notices_and_retrieval_references() {
    let full_text = "start\n".to_owned() + &"é".repeat(40 * 1024) + "\nend";
    let result: CallToolResult = serde_json::from_value(json!({
        "content": [
            {"type": "text", "text": full_text},
            {"type": "audio", "data": "AAAA", "mimeType": "audio/wav"}
        ],
        "structuredContent": {"rows": 1},
        "isError": false
    }))
    .unwrap();
    let output = convert_tool_result("big", result);
    assert!(!output.is_error, "{}", output.value);
    assert_eq!(output.value["truncated"], true);
    assert!(output.value["content"].as_str().unwrap().len() < 64 * 1024);
    // The outcome flag, notices and retrieval references stay top-level even
    // when the normalized body is truncated to an inline preview.
    assert_eq!(output.value["is_error"], false);
    assert_eq!(output.value["omitted_blocks"], 1);
    assert!(
        output.value["conversion_notices"][0]
            .as_str()
            .unwrap()
            .contains("block 1")
    );
    let raw = output.value["raw_result_path"].as_str().unwrap();
    let full = output.value["full_output_path"].as_str().unwrap();
    assert!(output.value["full_output_error"].is_null());
    assert_ne!(raw, full, "raw and normalized artifacts must be distinct");
    let normalized: Value = serde_json::from_slice(&fs::read(full).unwrap()).unwrap();
    assert_eq!(normalized["content"], full_text);
    assert_eq!(normalized["structured_content"], json!({"rows": 1}));
    assert_eq!(normalized["is_error"], false);
    assert_eq!(
        normalized["conversion_notices"],
        output.value["conversion_notices"]
    );
    assert_eq!(normalized["raw_result_path"].as_str().unwrap(), raw);
    let raw_saved: Value = serde_json::from_slice(&fs::read(raw).unwrap()).unwrap();
    assert_eq!(raw_saved["content"].as_array().unwrap().len(), 2);
    fs::remove_file(raw).unwrap();
    fs::remove_file(full).unwrap();
}

#[test]
fn oversized_structured_content_is_not_retained_inline() {
    // The inline preview must stay bounded even when the large payload is
    // structured content rather than text. The artifact keeps the full value.
    let rows: Vec<Value> = (0..8000)
        .map(|index| json!({ "index": index, "row": "x" }))
        .collect();
    let result: CallToolResult =
        serde_json::from_value(json!({ "content": [], "structuredContent": { "rows": rows } }))
            .unwrap();
    let output = convert_tool_result("wide", result);
    let inline = serde_json::to_string(&output.value).unwrap();
    assert!(
        inline.len() < 64 * 1024,
        "inline value stayed {} bytes",
        inline.len()
    );
    assert_eq!(output.value["truncated"], true);
    assert_eq!(output.value["structured_content"], Value::Null);
    assert_eq!(output.value["structured_content_omitted"], true);
    assert!(
        output.value["content"]
            .as_str()
            .unwrap()
            .contains("Structured content")
    );
    let full = output.value["full_output_path"].as_str().unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(full).unwrap()).unwrap();
    assert_eq!(
        saved["structured_content"]["rows"]
            .as_array()
            .unwrap()
            .len(),
        8000
    );
    fs::remove_file(full).unwrap();
}

#[test]
fn omission_notices_stay_bounded_for_many_unsupported_blocks() {
    let blocks: Vec<Value> = (0..64)
        .map(|_| json!({"type": "audio", "data": "AAAA", "mimeType": "audio/wav"}))
        .collect();
    let result: CallToolResult = serde_json::from_value(json!({"content": blocks})).unwrap();
    let output = convert_tool_result("many", result);
    assert!(!output.is_error, "{}", output.value);
    assert_eq!(output.value["omitted_blocks"], 64);
    assert_eq!(output.value["retained_blocks"], 0);
    let notices = output.value["conversion_notices"].as_array().unwrap();
    assert!(notices.len() <= 9, "{} inline notices", notices.len());
    let inline: usize = notices
        .iter()
        .map(|notice| notice.as_str().unwrap().len())
        .sum();
    assert!(
        inline < 4 * 1024,
        "inline warnings expanded to {inline} bytes"
    );
    let raw = output.value["raw_result_path"].as_str().unwrap();
    fs::remove_file(raw).unwrap();
}

#[test]
fn failed_artifact_write_reports_error_and_removes_partial_file() {
    use std::io::Write;
    let root = std::env::temp_dir().join(format!("ion-mcp-artifact-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let path = root.join("raw.json");
    let failure = conversion::write_private_artifact(path.clone(), |file| {
        file.write_all(b"{\"content\": [")?;
        Err(anyhow::anyhow!("simulated artifact failure"))
    });
    let error = format!(
        "{:#}",
        failure.expect_err("failed write must not report a path")
    );
    assert!(error.contains("simulated artifact failure"), "{error}");
    assert!(
        !path.exists(),
        "partial artifact must not survive a failed write"
    );
    fs::remove_dir_all(root).unwrap();
}
