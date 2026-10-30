//! Management runs beside Core work and retains its own metadata/results.
use super::*;
use crate::{McpConfig, McpExposure, McpServer, McpStdioServer, McpTools};

async fn install<M: Serialize + Send + 'static>(fixture: &mut Fixture<M>) -> Arc<McpTools> {
    fs::create_dir_all(fixture.host.config_root()).unwrap();
    let config = McpConfig::new(fixture.host.config_root());
    fs::write(fixture.root.join("peer-mode"), "ready").unwrap();
    config
        .add(
            "peer",
            McpServer::Stdio(McpStdioServer {
                enabled: true,
                description: None,
                cwd: None,
                env: Default::default(),
                command: "python3".into(),
                args: vec![
                    "-c".into(),
                    r#"
import json, sys, time
from pathlib import Path
root = Path(sys.argv[1])
mode = (root / 'peer-mode').read_text()
try:
    for line in sys.stdin:
        request = json.loads(line)
        if 'id' not in request:
            continue
        if request['method'] == 'initialize':
            if mode == 'hang':
                (root / 'waiting').touch()
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
        (root / 'hang-closing').touch()
        time.sleep(0.3)
        (root / 'hang-exit').touch()
"#
                    .into(),
                    fixture.root.to_string_lossy().into_owned(),
                ],
                exposure: McpExposure::Direct,
                tool_exposure: Default::default(),
            }),
        )
        .unwrap();
    let startup = McpTools::connect(&config, &fixture.root, CancellationToken::new()).await;
    assert!(startup.diagnostics.is_empty(), "{:?}", startup.diagnostics);
    let tools = startup.tools.unwrap();
    let mut setup = crate::CodingApplication::new(fixture.host.clone(), None)
        .with_mcp_tools(tools.clone())
        .setup(
            fixture.control.session().clone(),
            fixture.selected.clone(),
            None,
            None,
            fixture.host.resources(&fixture.root).unwrap(),
        )
        .unwrap();
    setup.runtime.agent = fixture.control.agent().clone();
    fixture.control = setup.into_control();
    tools
}

#[tokio::test]
async fn reconnect_preserves_core_work_and_reliable_metadata_until_transfer() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    let tools = install(&mut fixture).await;
    fixture
        .control
        .submit(Delivery::Start, prompt("active"), meta("turn"))
        .map_err(|e| e.reason)
        .unwrap();
    model.wait_calls(1).await;
    let core = fixture.control.current_operation();
    let before = fixture.control.session().view().unwrap().entries;
    let maintenance = fixture
        .control
        .reconnect_mcp("peer", meta("reconnect literal 雪"))
        .map_err(|(_, e)| e)
        .unwrap();
    assert_eq!(fixture.control.current_operation(), core);
    assert_eq!(
        fixture.control.state().mcp_reconnect.map(|state| state.id),
        Some(maintenance)
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.control.state().mcp_reconnect.is_some() {
            fixture.control.maintain().await;
        }
    })
    .await
    .unwrap();
    let snapshot = fixture.control.snapshot(|m| m.literal.clone()).unwrap();
    assert!(snapshot.updates.iter().any(|update| matches!(update,
        UpdateSnapshot::McpReconnectFinished { metadata, result: Ok(_), .. }
        if metadata == "reconnect literal 雪")));
    let (rejected, _) = fixture
        .control
        .reconnect_mcp("peer", meta("second"))
        .unwrap_err();
    assert_eq!(rejected.literal, "second");
    assert_eq!(fixture.control.session().view().unwrap().entries, before);
    assert_eq!(fixture.control.current_operation(), core);
    assert!(!fixture.control.state().stop_requested);
    let finished = loop {
        if let Some(ControlUpdate::McpReconnectFinished(finished)) = fixture.control.take_update() {
            break finished;
        }
    };
    assert!(finished.result.is_ok());
    assert_eq!(finished.operation, maintenance);
    assert_eq!(finished.metadata.literal, "reconnect literal 雪");
    assert_eq!(finished.metadata.marker.get(), 7);
    model.release();
    let (_, settled) = settle(&mut fixture.control).await;
    assert!(matches!(settled.outcome, OperationOutcome::Turn(Ok(_))));
    fixture.control.shutdown().await;
    tools.shutdown().await.unwrap();
}

#[tokio::test]
async fn stop_and_cancelled_waiters_keep_reconnect_cleanup_and_result_custody() {
    for owner_stop in [false, true] {
        let mut fixture = Fixture::new(Model::new(1), false);
        let tools = install(&mut fixture).await;
        fs::write(fixture.root.join("peer-mode"), "hang").unwrap();
        fixture
            .control
            .reconnect_mcp("peer", meta("retain"))
            .map_err(|(_, e)| e)
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.root.join("waiting").exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(10), fixture.control.next())
                .await
                .is_err()
        );
        let snapshot = fixture.control.snapshot(|m| m.literal.clone()).unwrap();
        assert_eq!(snapshot.mcp_reconnect.unwrap().metadata, "retain");
        let owner_interrupted = if owner_stop {
            fixture.control.owner().unwrap().request_shutdown();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !fixture.root.join("hang-closing").exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .is_ok()
        } else {
            fixture.control.request_stop();
            true
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(10), fixture.control.join_shutdown())
                .await
                .is_err()
        );
        let report = tokio::time::timeout(Duration::from_secs(5), fixture.control.shutdown())
            .await
            .unwrap();
        tools.shutdown().await.unwrap();
        assert!(fixture.root.join("hang-exit").exists());
        assert!(report.cleanup_errors.is_empty());
        let finished = report
            .updates
            .into_iter()
            .find_map(|update| match update {
                ControlUpdate::McpReconnectFinished(finished) => Some(finished),
                _ => None,
            })
            .unwrap();
        assert!(finished.result.is_err());
        assert!(finished.stop_requested);
        assert_eq!(finished.metadata.literal, "retain");
        assert!(
            owner_interrupted,
            "application shutdown must cancel maintenance without control polling"
        );
    }
}

#[tokio::test]
async fn management_authority_and_result_budget_precede_connection_effects() {
    let mut fixture = Fixture::new(Model::new(1), false);
    let (metadata, error) = fixture
        .control
        .reconnect_mcp("peer", meta("restricted"))
        .unwrap_err();
    assert_eq!(metadata.literal, "restricted");
    assert!(error.to_string().contains("not enabled"));
    let tools = install(&mut fixture).await;
    let (_, error) = fixture
        .control
        .reconnect_mcp("unknown", meta("invalid target"))
        .unwrap_err();
    assert!(error.to_string().contains("not admitted"));
    assert_eq!(
        lock(&fixture.control.custody).occupied,
        0,
        "failed admission must release the result slot"
    );
    fixture.control.budget = InputBudget::new(64);
    let (metadata, error) = fixture
        .control
        .reconnect_mcp("peer", meta(&"large".repeat(1024)))
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ion_core::CodingAgentError>(),
        Some(ion_core::CodingAgentError::InputQueueFull { .. })
    ));
    assert_eq!(metadata.literal.len(), 5120);
    assert!(!fixture.control.is_busy());
    assert!(fixture.control.session().view().unwrap().entries.is_empty());
    fixture.control.shutdown().await;
    tools.shutdown().await.unwrap();
}

#[tokio::test]
async fn reconnect_rechecks_and_reserves_capacity_after_reentrant_metadata_inspection() {
    let model = Model::new(1);
    let mut fixture = Fixture::<InterleavingMetadata>::with_metadata(model.clone(), false);
    let tools = install(&mut fixture).await;
    fixture
        .control
        .submit(Delivery::Start, prompt("root"), InterleavingMetadata(None))
        .unwrap();
    model.wait_calls(1).await;
    let port = fixture.control.steering().unwrap();
    for _ in 0..MAX_ADMISSIONS - 2 {
        port.submit(
            Message::user_input("earlier".into(), []),
            InterleavingMetadata(None),
        )
        .unwrap();
    }
    let (start, started) = std::sync::mpsc::channel();
    let (done, completed) = std::sync::mpsc::channel();
    let producer = std::thread::spawn(move || {
        started.recv().unwrap();
        let admitted = port
            .submit(
                Message::user_input("racing producer".into(), []),
                InterleavingMetadata(None),
            )
            .is_ok();
        done.send(admitted).unwrap();
        admitted
    });
    let outcome = fixture.control.reconnect_mcp(
        "peer",
        InterleavingMetadata(Some(Box::new(move || {
            start.send(()).unwrap();
            // Bound the probe even if an incorrect implementation serializes while
            // holding custody. Once it returns, the producer can always finish.
            let _ = completed.recv_timeout(Duration::from_millis(100));
        }))),
    );
    let admitted = producer.join().unwrap();
    let occupied = lock(&fixture.control.custody).occupied;
    fixture.control.shutdown().await;
    tools.shutdown().await.unwrap();
    assert!(admitted);
    assert!(
        outcome.is_err(),
        "the producer consumed the final reliable result slot"
    );
    assert_eq!(occupied, MAX_ADMISSIONS);
}
