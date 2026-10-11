use super::*;
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
        result = {'tools': 'invalid' if mode == 'list-error' else []}
    else:
        continue
    print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
    if mode == 'blocked-send':
        time.sleep(60)
Path('eof').touch()
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
                        command: "python3".into(),
                        args: vec!["-c".into(), SERVER.into(), mode.into()],
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
            command: "python3".into(),
            args: vec!["-c".into(), SERVER.into(), "blocked-send".into()],
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
                url: format!("http://{address}/mcp"), bearer_token_env: None,
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
