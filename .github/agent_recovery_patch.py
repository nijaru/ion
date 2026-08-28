from pathlib import Path


def replace(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"anchor missing in {path}: {old[:100]!r}")
    p.write_text(text.replace(old, new, 1))


# All durable lanes are already loaded and operation residency is keyed by
# OperationId. Do not discard/fence non-main operations before the generic
# recovery pass gets them.
replace(
    "crates/ion-core/src/runtime/mod.rs",
    '''        for operation in loaded.operations {\n            if operation.lane_name != crate::session::lane::MAIN {\n                if !matches!(&operation.latest.1.state, OperationState::Finished(_)) {\n                    error!(\n                        session = %self.session_id,\n                        operation = %operation.id,\n                        lane = %operation.lane_name,\n                        "single-lane runtime cannot host an open non-main operation; fencing"\n                    );\n                    self.closed = true;\n                    return;\n                }\n                continue;\n            }\n            let (state_seq, payload) = operation.latest;\n''',
    '''        for operation in loaded.operations {\n            let (state_seq, payload) = operation.latest;\n''',
)

# Make recovery diagnostics reflect operation-addressed residency rather than
# the historical main-only implementation.
p = Path("crates/ion-core/src/runtime/recovery.rs")
text = p.read_text().replace('expect("main operation residency exists")', 'expect("operation residency exists")')
p.write_text(text)

# Crash/reopen regression: the running descendant must be reconstructed and
# consume one execution permit; a second retained identity remains admitted but
# cannot start until the recovered operation finishes.
p = Path("crates/ion-core/src/tests/agent_family.rs")
text = p.read_text()
text += r'''

#[tokio::test]
async fn family_reattaches_open_lane_agent_and_execution_permit_after_crash() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ModelExecution);
    let runtime = Runtime::start_with_effect_gate(
        SharedLogProvider::default(),
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let family = Arc::new(runtime.agent_family(1).await.expect("family"));
    let root = family.root();
    let active_agent = family.admit_lane(root).await.expect("active agent admission");
    let waiting_agent = family.admit_lane(root).await.expect("waiting agent admission");

    let start_family = Arc::clone(&family);
    let start = tokio::spawn(async move {
        start_family
            .start(active_agent, "survive process loss")
            .await
    });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("agent model intent reached execution boundary");

    let loaded = store.load(session_id).await.expect("pre-crash load");
    let active_record = loaded
        .agents
        .iter()
        .find(|agent| agent.id == active_agent)
        .expect("active agent record");
    let active_lane = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == active_record.lane_name)
        .expect("active agent lane");
    let operation_id = active_lane
        .state
        .current_operation
        .expect("active operation is durable");
    assert!(matches!(
        loaded
            .operations
            .iter()
            .find(|operation| operation.id == operation_id)
            .expect("durable active operation")
            .latest
            .1
            .state,
        OperationState::AssistantEffectPending
    ));

    runtime.crash();
    gate.release();
    let _ = start.await.expect("start task");
    drop(family);
    drop(runtime);

    let recovered_provider = SharedLogProvider {
        settle_delay: Duration::from_millis(700),
        ..SharedLogProvider::default()
    };
    let runtime = Runtime::open_session(
        recovered_provider.clone(),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let family = runtime.agent_family(1).await.expect("reattach family");

    assert!(matches!(
        family.status(active_agent).await.expect("recovered active status"),
        crate::AgentStatus::Active {
            operation_id: recovered_operation,
            ..
        } if recovered_operation == operation_id
    ));
    assert_eq!(
        family.status(waiting_agent).await.expect("waiting status"),
        crate::AgentStatus::Admitted
    );
    assert!(matches!(
        family.start(waiting_agent, "must remain capacity-bound").await,
        Err(crate::AgentError::Capacity)
    ));

    timeout(Duration::from_secs(2), async {
        loop {
            if matches!(
                family.status(active_agent).await.expect("active status"),
                crate::AgentStatus::Finished {
                    operation_id: finished,
                    ..
                } if finished == operation_id
            ) {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("recovered agent finished");
    assert_eq!(recovered_provider.requests().len(), 1);

    family
        .start(waiting_agent, "capacity is reusable")
        .await
        .expect("released recovered permit");

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}
'''
p.write_text(text)
