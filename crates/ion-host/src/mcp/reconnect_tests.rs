//! Real shared-transport replacement, not replay or name-based rebinding.
use super::*;
use ion_ai::ToolCall;
use std::path::PathBuf;

const PEER: &str = r#"
import json, os, signal, sys, time
from pathlib import Path
root = Path(sys.argv[1])
value = os.environ['ION_RECONNECT_VALUE']
def record(event):
    with (root / 'events').open('a') as out:
        out.write(json.dumps({'event': event, 'value': value, 'pid': os.getpid()}) + '\n')
def stop(*_):
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
record('start')
try:
    for line in sys.stdin:
        request = json.loads(line)
        if 'id' not in request:
            continue
        method = request['method']
        record('request:' + method)
        if method == 'initialize':
            if value == 'hang':
                record('waiting')
                continue
            result = {'protocolVersion': request['params']['protocolVersion'],
                      'capabilities': {'tools': {}, 'resources': {}},
                      'serverInfo': {'name': 'reconnect', 'version': value}}
        elif method == 'tools/list':
            if value == 'collision':
                result = {'tools': [{'name': 'probe', 'inputSchema': {}}] * 2}
                print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
                continue
            if value == 'fail':
                print(json.dumps({'jsonrpc': '2.0', 'id': request['id'],
                                  'error': {'code': -32603, 'message': 'fixture listing failure'}}), flush=True)
                continue
            result = {'tools': [{'name': 'probe', 'inputSchema': {'type': 'object'},
                      'outputSchema': {'type': 'object', 'properties': {'generation': {'const': value}}}}]}
            if value == 'catalog':
                result['tools'].extend([{'name': name, 'inputSchema': {'type':'object'}} for name in ['direct', 'hidden']])
        elif method == 'tools/call':
            record('call')
            if request['params'].get('arguments', {}).get('wait'):
                record('waiting')
                continue
            result = {'content': [], 'structuredContent': {'generation': value}}
        elif method == 'resources/read':
            record('read')
            result = {'contents': [{'uri': 'mem://generation', 'text': value}]}
        else:
            continue
        print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
finally:
    if value == 'collision':
        record('closing')
        time.sleep(0.3)
    record('exit')
"#;

struct Fixture {
    root: PathBuf,
    config: McpConfig,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ion-mcp-reconnect-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let config = McpConfig::new(&root);
        config
            .add(
                "peer",
                McpServer::Stdio(McpStdioServer {
                    enabled: true,
                    description: None,
                    cwd: None,
                    env: [(
                        "ION_RECONNECT_VALUE".into(),
                        McpValue::Command {
                            command: "python3".into(),
                            args: vec![
                                "-c".into(),
                                "from pathlib import Path; print(Path('value').read_text())".into(),
                            ],
                        },
                    )]
                    .into(),
                    command: "python3".into(),
                    args: vec![
                        "-c".into(),
                        PEER.into(),
                        root.to_string_lossy().into_owned(),
                    ],
                    exposure: McpExposure::Direct,
                    tool_exposure: Default::default(),
                }),
            )
            .unwrap();
        Self { root, config }
    }
    fn value(&self, value: &str) {
        fs::write(self.root.join("value"), value).unwrap();
    }
    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("events"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    async fn wait(&self, event: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.events().iter().any(|entry| entry["event"] == event) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fixture did not observe the request");
    }
    async fn connect(&self) -> McpStartup {
        McpTools::connect(&self.config, &self.root, CancellationToken::new()).await
    }
    fn tools(source: &Arc<McpTools>) -> ion_core::ToolSet {
        ion_core::ToolSet::new([source.clone() as Arc<dyn ToolSource>])
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: "probe".into(),
        name: name.into(),
        arguments,
        raw_arguments: None,
    }
}

#[tokio::test]
async fn passive_inspection_captures_exposure_without_requests_or_launch_values() {
    let fixture = Fixture::new();
    fixture.value("catalog");
    let mut definition = fixture
        .config
        .list()
        .unwrap()
        .servers
        .remove("peer")
        .unwrap();
    if let McpServer::Stdio(server) = &mut definition {
        server.tool_exposure = [
            ("*".into(), McpExposure::Hidden),
            ("probe".into(), McpExposure::Deferred),
            ("direct".into(), McpExposure::Direct),
        ]
        .into();
        server
            .env
            .insert("SECRET".into(), McpValue::Text("MCP_STATUS_SECRET".into()));
    }
    fixture.config.remove("peer").unwrap();
    fixture.config.add("peer", definition).unwrap();
    let source = fixture.connect().await.tools.unwrap();
    let before = fixture.events();
    let first = source.snapshot();
    for _ in 0..20 {
        assert_eq!(source.snapshot(), first);
    }
    assert_eq!(first[0].status, McpServerStatus::Available);
    assert_eq!(
        first[0].tools,
        McpToolCounts {
            direct: 1,
            deferred: 1,
            hidden: 1
        }
    );
    assert!(first[0].resources);
    let encoded = serde_json::to_string(&first).unwrap();
    assert!(
        !encoded.contains("MCP_STATUS_SECRET") && !encoded.contains(fixture.root.to_str().unwrap())
    );
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        fixture.events(),
        before,
        "inspection must not perform protocol requests"
    );
    fixture.value("two");
    source.reconnect("peer").unwrap().join().await.unwrap();
    assert_eq!(
        source.snapshot()[0].tools,
        McpToolCounts {
            direct: 0,
            deferred: 1,
            hidden: 0
        }
    );
    source.shutdown().await.unwrap();
    let final_view = source.snapshot();
    assert_eq!(final_view[0].status, McpServerStatus::Closing);
    assert_eq!(final_view[0].tools, McpToolCounts::default());
    assert!(!final_view[0].resources);
}

#[tokio::test]
async fn replacement_joins_shared_requests_and_never_retargets_issued_capabilities() {
    let fixture = Fixture::new();
    fixture.value("one");
    let startup = fixture.connect().await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let source = startup.tools.unwrap();
    let tools = Fixture::tools(&source);
    let issued = tools.snapshot();
    let child_catalog = tools.snapshot();
    let pending = tokio::spawn(async move {
        child_catalog
            .execute_untracked(
                &call("mcp__4_peer__probe", json!({"wait":true})),
                CancellationToken::new(),
            )
            .await
    });
    fixture.wait("waiting").await;
    fixture.value("two");
    let result = source.reconnect("peer").unwrap().join().await;
    let interrupted = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    let fresh = tools.snapshot();
    let old_call = issued
        .execute_untracked(
            &call("mcp__4_peer__probe", json!({})),
            CancellationToken::new(),
        )
        .await;
    let old_resource = issued
        .execute_untracked(
            &call(
                "read_mcp_resource",
                json!({"server":"peer", "uri":"mem://generation"}),
            ),
            CancellationToken::new(),
        )
        .await;
    let new_call = fresh
        .execute_untracked(
            &call("mcp__4_peer__probe", json!({})),
            CancellationToken::new(),
        )
        .await;
    source.shutdown().await.unwrap();
    result.unwrap();
    assert!(interrupted.is_error, "{}", interrupted.value);
    assert!(
        interrupted.value["error"]
            .as_str()
            .unwrap()
            .contains("effects may be unknown")
    );
    assert!(old_call.is_error && old_resource.is_error);
    assert!(!new_call.is_error, "{}", new_call.value);
    assert_eq!(new_call.value["structured_content"]["generation"], "two");
    assert_eq!(
        issued
            .definition("mcp__4_peer__probe")
            .unwrap()
            .result_schema
            .as_ref()
            .unwrap()
            .schema["properties"]["generation"]["const"],
        "one"
    );
    assert_eq!(
        fresh
            .definition("mcp__4_peer__probe")
            .unwrap()
            .result_schema
            .as_ref()
            .unwrap()
            .schema["properties"]["generation"]["const"],
        "two"
    );
    let events = fixture.events();
    for started in events.iter().filter(|entry| entry["event"] == "start") {
        let process = tokio::process::Command::new("ps")
            .args(["-p", &started["pid"].to_string(), "-o", "pid="])
            .output()
            .await
            .unwrap();
        assert!(process.stderr.is_empty(), "ps failed: {process:?}");
        assert!(
            process.stdout.is_empty(),
            "owned generation was not reaped: {started}"
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(|entry| entry["event"] == "call")
            .count(),
        2,
        "no replay or old-capability dispatch: {events:?}"
    );
    let old_exit = events
        .iter()
        .position(|entry| entry["event"] == "exit" && entry["value"] == "one")
        .unwrap();
    let new_start = events
        .iter()
        .position(|entry| entry["event"] == "start" && entry["value"] == "two")
        .unwrap();
    assert!(
        old_exit < new_start,
        "replacement opened before retirement: {events:?}"
    );
}

#[tokio::test]
async fn initial_and_replacement_failures_remain_explicitly_recoverable() {
    let fixture = Fixture::new();
    // A missing helper input rejects startup; its trusted definition survives.
    let startup = fixture.connect().await;
    assert_eq!(startup.diagnostics.len(), 1);
    let source = startup
        .tools
        .expect("admitted unavailable definitions must survive");
    let tools = Fixture::tools(&source);
    assert!(tools.snapshot().specs().is_empty());
    assert_eq!(source.snapshot()[0].status, McpServerStatus::Unavailable);
    fixture.value("one");
    source.reconnect("peer").unwrap().join().await.unwrap();
    assert!(!tools.snapshot().specs().is_empty());
    fixture.value("fail");
    let failed = source.reconnect("peer").unwrap().join().await;
    assert!(failed.is_err());
    assert!(
        tools.snapshot().specs().is_empty(),
        "failed replacement must not retain old tools or resources"
    );
    fixture.value("three");
    source.reconnect("peer").unwrap().join().await.unwrap();
    let output = tools
        .snapshot()
        .execute_untracked(
            &call("mcp__4_peer__probe", json!({})),
            CancellationToken::new(),
        )
        .await;
    source.shutdown().await.unwrap();
    assert_eq!(output.value["structured_content"]["generation"], "three");
}

#[tokio::test]
async fn dropping_reconnect_retains_cleanup_and_shutdown_fences_publication() {
    let fixture = Fixture::new();
    // Begin with an unavailable descriptor, then hang the replacement handshake.
    let source = fixture.connect().await.tools.unwrap();
    fixture.value("hang");
    let reconnect = source.reconnect("peer").unwrap();
    fixture.wait("waiting").await;
    assert_eq!(source.snapshot()[0].status, McpServerStatus::Reconnecting);
    assert!(
        source.reconnect("peer").is_err(),
        "same-server overlap must be rejected"
    );
    drop(reconnect);
    source.shutdown().await.unwrap();
    assert_eq!(source.snapshot()[0].status, McpServerStatus::Closing);
    assert!(Fixture::tools(&source).snapshot().specs().is_empty());
    assert!(source.reconnect("peer").is_err());
    assert_eq!(
        fixture
            .events()
            .iter()
            .filter(|entry| entry["event"] == "exit")
            .count(),
        1
    );
}

#[tokio::test]
async fn reconnect_uses_captured_admission_not_current_configuration() {
    let fixture = Fixture::new();
    fixture.value("one");
    let source = fixture.connect().await.tools.unwrap();
    let mut changed = fixture
        .config
        .list()
        .unwrap()
        .servers
        .remove("peer")
        .unwrap();
    if let McpServer::Stdio(server) = &mut changed {
        server.enabled = false;
    }
    fixture.config.add("later", changed).unwrap();
    assert_eq!(
        source
            .snapshot()
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>(),
        ["peer"]
    );
    let denied = source.reconnect("later").is_err();
    // Current file changes do not revoke or redefine an admitted connection.
    fixture.config.remove("peer").unwrap();
    fixture.value("two");
    let reconnected = source.reconnect("peer").unwrap().join().await;
    source.shutdown().await.unwrap();
    assert!(denied);
    reconnected.unwrap();
    assert_eq!(
        fixture
            .events()
            .iter()
            .filter(|entry| entry["event"] == "start")
            .count(),
        2
    );
}

#[tokio::test]
async fn startup_cancellation_joins_successes_accumulated_before_a_failed_peer() {
    let fixture = Fixture::new();
    fixture.value("one");
    let mut collision = fixture
        .config
        .list()
        .unwrap()
        .servers
        .remove("peer")
        .unwrap();
    if let McpServer::Stdio(server) = &mut collision {
        server.env.insert(
            "ION_RECONNECT_VALUE".into(),
            McpValue::Text("collision".into()),
        );
    }
    fixture.config.add("z_collision", collision).unwrap();
    let stop = CancellationToken::new();
    let (startup, ()) = tokio::join!(
        McpTools::connect(&fixture.config, &fixture.root, stop.clone()),
        async {
            fixture.wait("closing").await;
            stop.cancel();
        },
    );
    assert!(startup.tools.is_none());
    let events = fixture.events();
    for value in ["one", "collision"] {
        assert!(
            events
                .iter()
                .any(|entry| entry["event"] == "exit" && entry["value"] == value),
            "unjoined {value}: {events:?}"
        );
    }
}
