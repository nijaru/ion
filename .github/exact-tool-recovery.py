from pathlib import Path


def replace_one(path: str, old: str, new: str, label: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected 1 match, found {count}")
    file.write_text(text.replace(old, new, 1))


# Recovered effects must execute against the immutable registry reconstructed
# from the operation's durable capability snapshot, never a fresh catalog
# snapshot. This also respects any structural lane narrowing that now makes a
# previously admitted executor unavailable: recovery fails visibly instead of
# reacquiring authority.
replace_one(
    "crates/ion-core/src/runtime/recovery.rs",
    '''                            self.install_active(staged);
                            let tools = self.tools.snapshot();
                            self.emit_tool_started(
                                operation_id,
                                call.call_id,
                                &call.name,
                                target_summary_registry(&tools, &call.name, &call.arguments),
                            );
''',
    '''                            let tools = staged.tool_registry.clone();
                            self.install_active(staged);
                            self.emit_tool_started(
                                operation_id,
                                call.call_id,
                                &call.name,
                                target_summary_registry(&tools, &call.name, &call.arguments),
                            );
''',
    "replay-safe recovery registry",
)
replace_one(
    "crates/ion-core/src/runtime/recovery.rs",
    '''                                    self.install_active(staged);
                                    let tools = self.tools.snapshot();
                                    self.emit_tool_started(
                                        operation_id,
                                        call.call_id,
                                        &call.name,
                                        target_summary_registry(
                                            &tools,
                                            &call.name,
                                            &call.arguments,
                                        ),
                                    );
''',
    '''                                    let tools = staged.tool_registry.clone();
                                    self.install_active(staged);
                                    self.emit_tool_started(
                                        operation_id,
                                        call.call_id,
                                        &call.name,
                                        target_summary_registry(
                                            &tools,
                                            &call.name,
                                            &call.arguments,
                                        ),
                                    );
''',
    "reconcile recovery registry",
)
replace_one(
    "crates/ion-core/src/runtime/recovery.rs",
    '''                    if self.interactive_approvals {
                        let tools = self.tools.snapshot();
                        let target = target_summary_registry(&tools, &call.name, &call.arguments);
''',
    '''                    if self.interactive_approvals {
                        let tools = self
                            .active(operation_id)
                            .expect("parked approval operation is resident")
                            .tool_registry
                            .clone();
                        let target = target_summary_registry(&tools, &call.name, &call.arguments);
''',
    "approval recovery registry",
)

# The reconciliation fixture previously persisted an empty capability snapshot
# while manufacturing a pending write effect. That state could never arise
# through runtime admission and accidentally depended on recovery reacquiring
# `write` from the live catalog. Give the fixture the real native registry
# snapshot so it tests reconciliation rather than bypassing capability recovery.
replace_one(
    "crates/ion-core/src/tests/reconcile.rs",
    '''    let operation_id = OperationId::generate();
    let (mut machine, _) = OperationMachine::accept(operation_id, "go", Vec::new());
''',
    '''    let operation_id = OperationId::generate();
    let capability_snapshot = ToolRegistry::with_cwd(cwd).capability_snapshot();
    let (mut machine, _) = OperationMachine::accept(
        operation_id,
        "go",
        capability_snapshot.tools.clone(),
    );
''',
    "reconcile admitted capability fixture",
)
replace_one(
    "crates/ion-core/src/tests/reconcile.rs",
    '''            capability_snapshot_id: CapabilitySnapshot::new(Vec::new()).id.clone(),
            open_effect: None,
        },
        capability_snapshot: CapabilitySnapshot::new(Vec::new()),
''',
    '''            capability_snapshot_id: capability_snapshot.id.clone(),
            open_effect: None,
        },
        capability_snapshot,
''',
    "reconcile initial checkpoint snapshot",
)
replace_one(
    "crates/ion-core/src/tests/reconcile.rs",
    '''            capability_snapshot_id: CapabilitySnapshot::new(Vec::new()).id.clone(),
            open_effect: Some(effect.clone()),
        },
        capability_snapshot: CapabilitySnapshot::new(Vec::new()),
''',
    '''            capability_snapshot_id: capability_snapshot.id.clone(),
            open_effect: Some(effect.clone()),
        },
        capability_snapshot,
''',
    "reconcile pending checkpoint snapshot",
)

# Regression: if structural lane authority is narrowed after a crash but before
# reopen, the persisted operation snapshot alone is not permission to reacquire
# an executor. The reconstructed registry is snapshot identity intersected with
# the current lane selection, so recovery must report the missing tool rather
# than executing it from the live catalog.
anchor = '''#[tokio::test]
async fn effect_gate_crash_prefix_reopens_after_tool_execution() {
'''
test = '''#[tokio::test]
async fn replay_safe_recovery_does_not_reacquire_a_structurally_removed_tool() {
    let dir = std::env::temp_dir().join(format!(
        "ion-gate-removed-read-{}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("note.txt"), "must not be reread").expect("write");
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ToolExecution);
    let runtime = Runtime::start_with_effect_gate(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "read",
            json!({"path": "note.txt"}),
        )]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, _events) = session.subscribe().await.expect("subscribe");
    let submit_session = session.clone();
    let submit = tokio::spawn(async move { submit_session.submit_if_idle("read").await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("tool gate reached");

    runtime.crash();
    gate.release();
    let _ = submit.await.expect("submit task");
    drop(session);
    drop(runtime);

    let loaded = store.load(session_id).await.expect("load");
    let mut config = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("main lane")
        .config
        .clone();
    config.tools = crate::tool::ToolSelection::Only(std::collections::BTreeSet::from([
        "find".to_owned(),
    ]));
    store
        .set_lane_config(session_id, crate::session::lane::MAIN, config)
        .await
        .expect("remove read capability");

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("recovered\\n")]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));

    let loaded = store.load(session_id).await.expect("reload");
    assert!(loaded.entries.iter().any(|record| matches!(
        &record.entry,
        SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. }
        } if error.contains("unknown tool: read")
    )));
    assert!(!loaded.entries.iter().any(|record| matches!(
        &record.entry,
        SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. }
        } if output.contains("must not be reread")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(dir);
}

'''
replace_one(
    "crates/ion-core/src/tests/crash_recovery.rs",
    anchor,
    test + anchor,
    "exact recovery regression",
)

replace_one(
    "DESIGN.md",
    '''Shared-history and separately hosted admission both publish durable lane capability selections that may narrow but never exceed the control parent, and recovery re-applies that stored selection to the available executor catalog.''',
    '''Shared-history and separately hosted admission both publish durable lane capability selections that may narrow but never exceed the control parent. Recovery reconstructs an operation registry by intersecting its immutable capability snapshot with the lane's current structural selection; it never reacquires an executor from a later live catalog snapshot.''',
    "design exact recovery checkpoint",
)
