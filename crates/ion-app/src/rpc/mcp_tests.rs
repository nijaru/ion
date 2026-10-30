//! RPC acknowledges local admission separately from retained MCP completion.
use super::*;

async fn with_mcp(fixture: &mut Fixture) -> (Control, Arc<ion_host::McpTools>) {
    let original = fixture.take();
    let services = original.services.clone();
    let cwd = original.host.session().cwd().unwrap().to_owned();
    fs::write(cwd.join("mode"), "ready").unwrap();
    let config = ion_host::McpConfig::new(services.config_root());
    config
        .add(
            "peer",
            ion_host::McpServer::Stdio(ion_host::McpStdioServer {
                enabled: true,
                description: None,
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec![
                    "-c".into(),
                    r#"
import json, time
from pathlib import Path
mode = Path('mode').read_text()
try:
    for line in __import__('sys').stdin:
        request = json.loads(line)
        if 'id' not in request:
            continue
        if request['method'] == 'initialize':
            if mode == 'hang':
                Path('waiting').touch()
                continue
            result = {'protocolVersion': request['params']['protocolVersion'],
                      'capabilities': {'tools': {}}, 'serverInfo': {'name': 'peer', 'version': '1'}}
        elif request['method'] == 'tools/list':
            result = {'tools': []}
        else:
            continue
        print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}), flush=True)
finally:
    if mode == 'hang':
        time.sleep(0.1)
        Path('joined').touch()
"#
                    .into(),
                ],
                exposure: ion_host::McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup =
        ion_host::McpTools::connect(&config, &cwd, tokio_util::sync::CancellationToken::new())
            .await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let tools = startup.tools.unwrap();
    let session = original.host.session().clone();
    let model = original.host.agent().model().clone();
    drop(original);
    let setup = SessionSetup::new(
        session,
        services.sessions(Some(cwd)),
        Arc::new(
            ion_host::CodingApplication::new(services.clone(), None).with_mcp_tools(tools.clone()),
        ),
        Some(&model),
        None,
        None,
    )
    .unwrap();
    (Control::new(setup, services), tools)
}

#[tokio::test]
async fn reconnect_ack_and_completion_share_correlation_and_reliable_reply_custody() {
    let mut fixture = Fixture::new();
    let (mut control, tools) = with_mcp(&mut fixture).await;
    let before = control.host.session().view().unwrap().entries;
    let credits = ReplyCredits::new(1);
    let (ack, credit) = control
        .command(
            json!({"id":"replace", "type":"reconnect_mcp", "server":"peer"}),
            credits.acquire().unwrap(),
        )
        .unwrap();
    assert_eq!(ack["success"], true);
    assert_eq!(ack["data"]["disposition"], "admitted");
    let operation = ack["data"]["operation"].clone();
    let mut bytes = Vec::new();
    let mut output = Output::new(&mut bytes);
    output.enqueue_reliable(ack, credit).unwrap();
    assert!(credits.acquire().is_none());
    loop {
        let update = control.host.next().await;
        let finished = matches!(update, ControlUpdate::McpReconnectFinished(_));
        control.update(update, &mut output).unwrap();
        if finished {
            break;
        }
    }
    assert!(
        credits.acquire().is_none(),
        "queued completion retains credit"
    );
    output.finish().await.unwrap();
    drop(output);
    assert!(credits.acquire().is_some());
    assert!(control.turn_reply.is_none());
    let frames = records(&bytes);
    let finished = frames
        .iter()
        .find(|record| record["type"] == "mcp_reconnect_finished")
        .unwrap();
    assert_eq!(finished["id"], "replace");
    assert_eq!(finished["operation"], operation);
    assert_eq!(finished["success"], true);
    assert_eq!(finished["server"], "peer");
    let after = control.host.session().view().unwrap().entries;
    control.host.shutdown().await;
    tools.shutdown().await.unwrap();
    assert_eq!(after, before, "management must not fabricate Core facts");
}

#[tokio::test]
async fn eof_cancels_joins_and_delivers_the_admitted_reconnect_result() {
    let mut fixture = Fixture::new();
    let (mut control, tools) = with_mcp(&mut fixture).await;
    fs::write(fixture.root.join("work/mode"), "hang").unwrap();
    let credits = ReplyCredits::new(1);
    let (ack, credit) = control
        .command(
            json!({"id":"stop", "type":"reconnect_mcp", "server":"peer"}),
            credits.acquire().unwrap(),
        )
        .unwrap();
    assert_eq!(ack["success"], true);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fixture.root.join("work/waiting").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let mut bytes = Vec::new();
    let mut output = Output::new(&mut bytes);
    output.enqueue_reliable(ack, credit).unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        connection::connection(
            control,
            CommandReader::new(BufReader::new(tokio::io::empty())),
            output,
            std::future::pending(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    tools.shutdown().await.unwrap();
    assert!(fixture.root.join("work/joined").exists());
    let frames = records(&bytes);
    let finished = frames
        .iter()
        .find(|record| record["type"] == "mcp_reconnect_finished")
        .unwrap();
    assert_eq!(finished["id"], "stop");
    assert_eq!(finished["success"], false);
    assert_eq!(finished["stop_requested"], true);
    assert!(credits.acquire().is_some());
}
