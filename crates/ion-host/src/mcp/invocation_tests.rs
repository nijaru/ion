use super::*;
use ion_ai::ToolCall;
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
