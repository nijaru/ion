use super::*;
use rmcp::model::CallToolRequestParams;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
};

// Native root deliberately remains alive after stdin EOF. The PID check below
// detects both surviving processes and zombies, independently of SDK results.
const SERVER: &str = r#"
import json, os, sys, time
from pathlib import Path
Path('pid').write_text(str(os.getpid()))
mode = sys.argv[1]
for line in sys.stdin:
    request = json.loads(line)
    if 'id' not in request:
        continue
    method = request['method']
    if method == 'initialize':
        if mode == 'init-timeout':
            continue
        if mode == 'init-error':
            print(json.dumps({'jsonrpc': '2.0', 'id': request['id'],
                              'error': {'code': -32603, 'message': 'fixture init fault'}}), flush=True)
            continue
        result = {'protocolVersion': request['params']['protocolVersion'],
                  'capabilities': {'tools': {}}, 'serverInfo': {'name': 'owned', 'version': '1'}}
    elif method == 'tools/list':
        if mode == 'list-timeout':
            continue
        if mode == 'list-error':
            os.write(2, b'LIST_FAILURE_DIAGNOSTIC\n')
        result = {'tools': 'invalid' if mode == 'list-error' else
                  [{'name': 'log', 'inputSchema': {'type': 'object'}}] if mode == 'stderr-flood' else []}
    elif method == 'tools/call' and mode == 'stderr-flood':
        os.write(2, b'LOG_PREFIX\n\x1b[2J' + b'x' * (1024 * 1024))
        result = {'content': [{'type': 'text', 'text': 'logging completed'}]}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
    if mode == 'blocked-send':
        time.sleep(60)
Path('eof').touch()
if mode == 'delayed-exit':
    time.sleep(0.3)
    Path('graceful').touch()
else:
    time.sleep(60)
"#;

#[tokio::test]
async fn native_roots_are_reaped_after_shutdown_and_failed_startup() {
    let root = std::env::temp_dir().join(format!("ion-mcp-lifecycle-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let cases = [
        "ready",
        "init-error",
        "init-timeout",
        "list-error",
        "list-timeout",
    ];
    // Separate roots allow timeout faults to run concurrently without sharing
    // protocol state or PID files.
    let checks = cases.map(|mode| {
        let root = root.join(mode);
        async move {
            fs::create_dir(&root).unwrap();
            let config = McpConfig::new(&root);
            config
                .add(
                    "owned",
                    McpServer::Stdio(McpStdioServer {
                        enabled: true,
                        description: None,
                        cwd: None,
                        env: Default::default(),
                        command: "python3".into(),
                        args: vec!["-c".into(), SERVER.into(), mode.into()],
                        exposure: McpExposure::Direct,
                        tool_exposure: Default::default(),
                    }),
                )
                .unwrap();
            let startup = McpTools::connect(&config, &root).await;
            let diagnostics = startup.diagnostics;
            let ready = startup.tools.is_some();
            let cleanup = match startup.tools {
                Some(tools) => tools.shutdown().await,
                None => Ok(()),
            };
            let pid = fs::read_to_string(root.join("pid")).unwrap();
            let process = Command::new("ps")
                .args(["-p", &pid, "-o", "pid="])
                .output()
                .await
                .unwrap();
            let gone = process.stdout.is_empty();
            // On regression, signal only a root still running our exact
            // fixture, not an unrelated process that reused this PID.
            if !gone {
                let command = Command::new("ps")
                    .args(["-ww", "-p", &pid, "-o", "command="])
                    .output()
                    .await
                    .unwrap();
                if String::from_utf8_lossy(&command.stdout)
                    .contains("Path('pid').write_text(str(os.getpid()))")
                {
                    Command::new("kill")
                        .args(["-KILL", &pid])
                        .status()
                        .await
                        .unwrap();
                }
            }
            assert!(process.stderr.is_empty(), "ps failed: {process:?}");
            assert!(gone, "{mode}: root PID {pid} survived or was not reaped");
            cleanup.unwrap();
            assert!(root.join("eof").exists(), "{mode}: stdin did not close");
            assert_eq!(ready, mode == "ready", "{mode}: {diagnostics:?}");
            if mode != "ready" {
                assert_eq!(diagnostics.len(), 1, "{mode}: {diagnostics:?}");
                let phase = if mode.starts_with("init") {
                    "initialization"
                } else {
                    "tool listing"
                };
                assert!(diagnostics[0].contains(phase), "{mode}: {diagnostics:?}");
                if mode == "list-error" {
                    // A listing failure after successful initialization must
                    // still expose the server's captured stderr reference.
                    assert!(diagnostics[0].contains("stderr"), "{mode}: {diagnostics:?}");
                    assert!(
                        diagnostics[0].contains("ion-mcp-stderr-"),
                        "{mode}: {diagnostics:?}"
                    );
                }
                let fault = if mode.ends_with("timeout") {
                    "timed out"
                } else {
                    "failed"
                };
                assert!(diagnostics[0].contains(fault), "{mode}: {diagnostics:?}");
            }
        }
    });
    join_all(checks).await;
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn shutdown_reaps_root_while_protocol_send_is_backpressured() {
    let root = std::env::temp_dir().join(format!("ion-mcp-blocked-send-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let connection = Connection::start(
        "blocked",
        McpServer::Stdio(McpStdioServer {
            enabled: true,
            description: None,
            cwd: None,
            env: Default::default(),
            command: "python3".into(),
            args: vec!["-c".into(), SERVER.into(), "blocked-send".into()],
            exposure: McpExposure::Direct,
            tool_exposure: Default::default(),
        }),
        &root,
        ChangeHandler(Arc::new(AtomicU64::new(0))),
    )
    .await
    .unwrap();
    let arguments = serde_json::from_value(json!({"payload":"x".repeat(1024 * 1024)})).unwrap();
    let call = connection
        .client
        .call_tool(CallToolRequestParams::new("blocked").with_arguments(arguments));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), call)
            .await
            .is_err()
    );
    let closed = tokio::time::timeout(Duration::from_secs(5), connection.shutdown()).await;
    // The fixture's kill_on_drop also signals the owned root on a failed check.
    // A successful controlled shutdown must already have reaped it.
    closed
        .expect("protocol send prevented owned shutdown")
        .unwrap();
    let pid = fs::read_to_string(root.join("pid")).unwrap();
    let process = Command::new("ps")
        .args(["-p", &pid, "-o", "pid="])
        .output()
        .await
        .unwrap();
    assert!(process.stderr.is_empty(), "ps failed: {process:?}");
    assert!(
        process.stdout.is_empty(),
        "backpressured root PID {pid} was not reaped"
    );
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn stderr_is_bounded_private_and_drained_without_blocking_tool_results() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("ion-mcp-stderr-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    config
        .add(
            "owned",
            McpServer::Stdio(McpStdioServer {
                enabled: true,
                description: None,
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec!["-c".into(), SERVER.into(), "stderr-flood".into()],
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &root).await;
    let source = startup.tools.unwrap();
    let tools = ion_core::ToolSet::new([source.clone() as Arc<dyn CodingToolSource>]);
    let call = ion_ai::ToolCall {
        id: "logs".into(),
        name: "mcp__5_owned__log".into(),
        arguments: json!({}),
        raw_arguments: None,
    };
    let output = tools
        .snapshot()
        .execute_untracked(&call, CancellationToken::new())
        .await;
    source.shutdown().await.unwrap();
    let log = source.servers[0]
        .stderr
        .as_ref()
        .unwrap()
        .snapshot()
        .unwrap();
    let path = std::path::Path::new(log["path"].as_str().unwrap());
    let captured = fs::read(path).unwrap();
    let mode = fs::metadata(path).unwrap().permissions().mode();
    fs::remove_file(path).unwrap();
    fs::remove_dir_all(root).unwrap();
    assert!(!output.is_error, "{output:?}");
    assert_eq!(output.value["content"], "logging completed");
    assert!(
        output.value["server_stderr"]["owned"]["path"].is_string(),
        "{output:?}"
    );
    assert_eq!(captured.len(), 64 * 1024);
    assert!(captured.starts_with(b"LOG_PREFIX\n\x1b[2J"));
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(
        log["total_bytes"],
        1024 * 1024 + b"LOG_PREFIX\n\x1b[2J".len()
    );
    assert_eq!(log["retained_bytes"], 64 * 1024);
    assert_eq!(log["eof"], true);
    assert!(log["capture_error"].is_null());
}

#[tokio::test]
async fn cancelled_shutdown_waiter_does_not_abandon_graceful_close() {
    let root = std::env::temp_dir().join(format!("ion-mcp-close-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let config = McpConfig::new(&root);
    config
        .add(
            "owned",
            McpServer::Stdio(McpStdioServer {
                enabled: true,
                description: None,
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec!["-c".into(), SERVER.into(), "delayed-exit".into()],
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let source = McpTools::connect(&config, &root).await.tools.unwrap();
    let mut closing = Box::pin(source.shutdown());
    let released = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            tokio::select! {
                result = &mut closing => panic!("shutdown settled before delayed root exit: {result:?}"),
                () = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
            if root.join("eof").exists() { break; }
        }
    }).await;
    drop(closing);
    // Cleanup continues without a waiter; cancellation must not kill the root early.
    let graceful = tokio::time::timeout(Duration::from_secs(2), async {
        while !root.join("graceful").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok();
    let joined = source.shutdown().await;
    let pid = fs::read_to_string(root.join("pid")).unwrap();
    let process = Command::new("ps")
        .args(["-p", &pid, "-o", "pid="])
        .output()
        .await
        .unwrap();
    fs::remove_dir_all(root).unwrap();
    released.unwrap();
    joined.unwrap();
    assert!(
        graceful,
        "cancelled waiter interrupted the root's graceful close"
    );
    assert!(process.stderr.is_empty());
    assert!(process.stdout.is_empty(), "root PID {pid} was not reaped");
}

#[tokio::test]
async fn failed_http_startup_closes_pending_request_before_runtime_teardown() {
    let root = std::env::temp_dir().join(format!("ion-mcp-http-startup-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let checks = [false, true].map(|sse| {
        let root = root.join(if sse { "sse" } else { "headers" });
        async move {
            fs::create_dir(&root).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            // Keep the runtime and mock server alive after connect returns.
            // CLI exit alone could hide a worker detached during initialization.
            let mut server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                let received = socket.read(&mut bytes).await.unwrap();
                assert!(bytes[..received].starts_with(b"POST "));
                if sse {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
                }
                // Never complete the HTTP response. Successful startup cleanup
                // must close this socket while our server task is still alive.
                socket.read_to_end(&mut Vec::new()).await.unwrap();
            });
            let config = McpConfig::new(&root);
            config.add("stalled", McpServer::Http(McpHttpServer {
                enabled: true,
                description: None,
                url: format!("http://{address}/mcp"), bearer_token_env: None,
                headers: Default::default(),
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            })).unwrap();
            let startup = McpTools::connect(&config, &root).await;
            let closed = tokio::time::timeout(Duration::from_secs(1), &mut server).await;
            if closed.is_err() {
                server.abort();
                let _ = server.await;
            }
            assert!(closed.is_ok(), "HTTP startup detached a pending request (SSE: {sse})");
            closed.unwrap().unwrap();
            assert!(startup.tools.is_none());
            assert_eq!(startup.diagnostics.len(), 1);
            assert!(startup.diagnostics[0].contains("initialization timed out"), "{:?}", startup.diagnostics);
        }
    });
    join_all(checks).await;
    fs::remove_dir_all(root).unwrap();
}
