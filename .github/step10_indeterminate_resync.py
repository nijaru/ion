from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    assert text.count(old) == 1, f"{label} context changed"
    return text.replace(old, new, 1)


runtime = Path("crates/ion-core/src/runtime/mod.rs")
text = runtime.read_text()
text = replace_once(
    text,
    """    /// Persisted indeterminate outcomes that must remain visible to a
    /// frontend attaching after startup recovery.
    indeterminate_warning: Option<IndeterminateWarning>,
""",
    """    /// Main-lane indeterminate outcome that must remain visible to a
    /// frontend attaching after startup recovery. Shared-history worker
    /// outcomes remain observable through Family/durable operation state and
    /// must not leak into the public main-lane snapshot.
    main_indeterminate_warning: Option<IndeterminateWarning>,
""",
    "runtime warning field",
)
text = replace_once(
    text,
    """            events,
            main_events,
            indeterminate_warning: None,
""",
    """            events,
            main_events,
            main_indeterminate_warning: None,
""",
    "runtime warning init",
)
text = replace_once(
    text,
    """                OperationOutcome::Indeterminate => {
                    self.indeterminate_warning = Some(IndeterminateWarning {
                        operation_id,
                        message: INDETERMINATE_MESSAGE.to_owned(),
                    });
                    self.emit(RuntimeEvent::OperationIndeterminate {
""",
    """                OperationOutcome::Indeterminate => {
                    if self.operation_lane_name(operation_id) == Some(crate::session::lane::MAIN) {
                        self.main_indeterminate_warning = Some(IndeterminateWarning {
                            operation_id,
                            message: INDETERMINATE_MESSAGE.to_owned(),
                        });
                    }
                    self.emit(RuntimeEvent::OperationIndeterminate {
""",
    "indeterminate terminal state",
)
text = replace_once(
    text,
    """            indeterminate: self.indeterminate_warning.clone(),
""",
    """            indeterminate: self.main_indeterminate_warning.clone(),
""",
    "snapshot warning",
)
runtime.write_text(text)


tui = Path("crates/ion/src/tui.rs")
text = tui.read_text()
old = """    fn undo_edit(&mut self) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.composer = snapshot.composer;
            self.cursor = snapshot.cursor.min(self.composer.chars().count());
            self.preferred_column = None;
            self.exit_history_browse();
        }
        self.break_edit_group();
    }
}
"""
new = """    fn undo_edit(&mut self) {
        if let Some(snapshot) = self.undo_stack.pop() {
            self.composer = snapshot.composer;
            self.cursor = snapshot.cursor.min(self.composer.chars().count());
            self.preferred_column = None;
            self.exit_history_browse();
        }
        self.break_edit_group();
    }

    /// Queue the authoritative snapshot warning. Lag resynchronization rebuilds
    /// the presentation transcript from durable entries, so this is deliberately
    /// re-queued on every fresh snapshot rather than deduplicated against lines
    /// that may just have been discarded with the old transcript.
    fn surface_indeterminate_warning(&mut self, warning: Option<&ion_core::IndeterminateWarning>) {
        if let Some(warning) = warning {
            self.pending_scrollback.push(
                Line::from(format!(
                    "! indeterminate operation {}: {}",
                    warning.operation_id, warning.message
                ))
                .yellow()
                .bold(),
            );
        }
    }
}
"""
text = replace_once(text, old, new, "UiState warning helper")
old = """    if let Some(warning) = &snapshot.indeterminate {
        state.pending_scrollback.push(
            Line::from(format!(
                "! indeterminate operation {}: {}",
                warning.operation_id, warning.message
            ))
            .yellow()
            .bold(),
        );
    }
"""
new = """    state.surface_indeterminate_warning(snapshot.indeterminate.as_ref());
"""
text = replace_once(text, old, new, "initial snapshot warning")
old = """        // Restore the runtime-owned projection of the latest durable usage;
        // frontend resynchronization never reads the store directly.
        self.usage = snapshot.latest_usage;
        match &snapshot.live {
"""
new = """        // Restore the runtime-owned projection of the latest durable usage;
        // frontend resynchronization never reads the store directly.
        self.usage = snapshot.latest_usage;
        // Indeterminate recovery may have completed before the frontend saw its
        // live event. The snapshot is authoritative for this warning too.
        self.surface_indeterminate_warning(snapshot.indeterminate.as_ref());
        match &snapshot.live {
"""
text = replace_once(text, old, new, "lag resync warning")

marker = """    #[test]
    fn resync_after_lag_on_idle_clears_partial_draft() {
"""
assert text.count(marker) == 1, "TUI lag test marker changed"
regression = r'''    #[test]
    fn resync_after_lag_resurfaces_indeterminate_snapshot_warning() {
        let operation_id = OperationId::generate();
        let snapshot = SessionSnapshot {
            cursor: RuntimeCursor::default(),
            runtime_instance_id: RuntimeInstanceId::generate(),
            indeterminate: Some(ion_core::IndeterminateWarning {
                operation_id,
                message: "inspect it before retrying".to_owned(),
            }),
            reopen_entry_count: None,
            operation: OperationStatus::Idle,
            entries: Vec::new(),
            model_ref: "test-model".to_owned(),
            latest_usage: None,
            live: None,
        };
        let mut state = UiState::new();
        state.resync_after_lag(&snapshot);

        let rendered = state
            .pending_scrollback
            .iter()
            .flat_map(|line| line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(rendered.contains(&operation_id.to_string()));
        assert!(rendered.contains("inspect it before retrying"));
    }

'''
text = text.replace(marker, regression + marker, 1)
tui.write_text(text)


crash = Path("crates/ion-core/src/tests/crash_recovery.rs")
text = crash.read_text()
marker = """#[tokio::test]
async fn parked_approval_survives_process_loss_and_decides_after_reopen() {
"""
assert text.count(marker) == 1, "crash recovery insertion marker changed"
regression = r'''#[tokio::test]
async fn worker_indeterminate_does_not_leak_into_main_snapshot() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "bash",
            json!({"command":"sleep 30"}),
        )]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    session.create_lane("worker").await.expect("worker lane");
    let (_snapshot, mut all_events) = session.subscribe_all().await.expect("subscribe all");
    let worker_operation = session
        .submit_if_idle_on_lane("worker", "run")
        .await
        .expect("worker submit");
    loop {
        let event = timeout(Duration::from_secs(2), all_events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(
            event,
            RuntimeEvent::ToolStarted { operation_id, .. } if operation_id == worker_operation
        ) {
            break;
        }
    }

    runtime.crash();
    drop(runtime);
    drop(session);

    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let snapshot = session.snapshot().await.expect("main snapshot");
    assert!(snapshot.indeterminate.is_none());
    assert_eq!(snapshot.operation, OperationStatus::Idle);

    let loaded = store.load(session_id).await.expect("load");
    let worker = loaded
        .operations
        .iter()
        .find(|operation| operation.id == worker_operation)
        .expect("worker operation");
    assert_eq!(
        worker.latest.1.state,
        OperationState::Finished(OperationOutcome::Indeterminate)
    );

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

'''
crash.write_text(text.replace(marker, regression + marker, 1))


design = Path("DESIGN.md")
text = design.read_text()
old = "10. Finish the interactive frontend against the stable session/agent-host contract, with ACP as a first-class sibling client. Validate pure UI configuration before terminal/runtime/session acquisition; after terminal acquisition, explicitly restore the terminal before startup diagnostics and unwind acquired store/catalog/runtime ownership on failure. Keep the public frontend snapshot/event subscription coherent on `main` while family waits observe all lanes internally. Keep `SessionHandle` as the only runtime mutation path and preserve the established `TERMINAL.md` reducer/`TerminalSession` architecture rather than introducing another UI framework."
new = "10. Finish the interactive frontend against the stable session/agent-host contract, with ACP as a first-class sibling client. Validate pure UI configuration before terminal/runtime/session acquisition; after terminal acquisition, explicitly restore the terminal before startup diagnostics and unwind acquired store/catalog/runtime ownership on failure. Keep the public frontend snapshot/event subscription coherent on `main` while family waits observe all lanes internally, and make lag resynchronization reconstruct every correctness-visible snapshot warning as well as live operation state. Keep `SessionHandle` as the only runtime mutation path and preserve the established `TERMINAL.md` reducer/`TerminalSession` architecture rather than introducing another UI framework."
text = replace_once(text, old, new, "Step 10")
design.write_text(text)
