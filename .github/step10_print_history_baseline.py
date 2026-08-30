from pathlib import Path

path = Path("crates/ion/src/print.rs")
text = path.read_text()

old = """        let (_snapshot, events) = session.subscribe().await?;\n        let operation_id = session.submit_if_idle(prompt).await?;\n        self.run_operation(session, events, operation_id).await\n"""
new = """        let (snapshot, events) = session.subscribe().await?;\n        let history_baseline = print_text(&snapshot);\n        let operation_id = session.submit_if_idle(prompt).await?;\n        self.run_operation(session, events, operation_id, history_baseline)\n            .await\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one run subscription block, found {text.count(old)}")
text = text.replace(old, new)

old = """        mut events: EventSubscription,\n        operation_id: OperationId,\n    ) -> Result<(), RuntimeError> {\n"""
new = """        mut events: EventSubscription,\n        operation_id: OperationId,\n        history_baseline: String,\n    ) -> Result<(), RuntimeError> {\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one run_operation signature, found {text.count(old)}")
text = text.replace(old, new)

old = """                    self.reconstruct_after_lag(&snapshot, &mut emitted)?;\n"""
new = """                    self.reconstruct_after_lag(\n                        &snapshot,\n                        &history_baseline,\n                        &mut emitted,\n                    )?;\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one lag reconstruction call, found {text.count(old)}")
text = text.replace(old, new)

old = """    fn reconstruct_after_lag(\n        &mut self,\n        snapshot: &SessionSnapshot,\n        emitted: &mut String,\n    ) -> Result<(), RuntimeError> {\n        let authoritative = print_text(snapshot);\n        let Some(missing) = authoritative.strip_prefix(emitted.as_str()) else {\n            return Err(RuntimeError::OperationFailed(\n                \"print event stream lagged after non-durable partial output; exact output reconstruction is unavailable\"\n                    .to_owned(),\n            ));\n        };\n"""
new = """    fn reconstruct_after_lag(\n        &mut self,\n        snapshot: &SessionSnapshot,\n        history_baseline: &str,\n        emitted: &mut String,\n    ) -> Result<(), RuntimeError> {\n        let authoritative = print_text(snapshot);\n        let Some(current_turn) = authoritative.strip_prefix(history_baseline) else {\n            return Err(RuntimeError::OperationFailed(\n                \"print event stream lagged after its session-history baseline changed; exact output reconstruction is unavailable\"\n                    .to_owned(),\n            ));\n        };\n        let Some(missing) = current_turn.strip_prefix(emitted.as_str()) else {\n            return Err(RuntimeError::OperationFailed(\n                \"print event stream lagged after non-durable partial output; exact output reconstruction is unavailable\"\n                    .to_owned(),\n            ));\n        };\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one reconstruction body, found {text.count(old)}")
text = text.replace(old, new)

text = text.replace(
    """            .run_operation(&session, events, operation_id)\n""",
    """            .run_operation(&session, events, operation_id, String::new())\n""",
)
if text.count(".run_operation(&session, events, operation_id)") != 0:
    raise SystemExit("unpatched run_operation test call remains")

needle = """    #[tokio::test]\n    async fn lagged_print_reconstructs_live_draft_and_stays_attached() {\n"""
insert = """    #[tokio::test]\n    async fn lagged_print_reconstructs_only_current_turn_after_prior_history() {\n        let runtime = Runtime::start_with_store(\n            BurstProvider {\n                settle_delay: Duration::ZERO,\n            },\n            ToolRegistry::default(),\n            SessionStore::open_in_memory().expect(\"store\"),\n        );\n        let session = runtime.session();\n\n        let mut first_output = Vec::new();\n        PrintFrontend::new(&mut first_output)\n            .run(&session, \"first\")\n            .await\n            .expect(\"first turn\");\n        assert_eq!(first_output, vec![b'x'; 96]);\n\n        let (snapshot, events) = session.subscribe().await.expect(\"subscribe\");\n        let history_baseline = print_text(&snapshot);\n        assert_eq!(history_baseline.as_bytes(), vec![b'x'; 96]);\n        let operation_id = session.submit_if_idle(\"second\").await.expect(\"submit\");\n        wait_for_snapshot(&session, |snapshot| {\n            snapshot\n                .latest_settlement\n                .as_ref()\n                .is_some_and(|settlement| {\n                    settlement.operation_id == operation_id\n                        && settlement.outcome == OperationOutcome::Completed\n                })\n        })\n        .await;\n\n        let mut second_output = Vec::new();\n        PrintFrontend::new(&mut second_output)\n            .run_operation(&session, events, operation_id, history_baseline)\n            .await\n            .expect(\"lagged second turn should reconstruct\");\n        assert_eq!(second_output, vec![b'x'; 96]);\n\n        session.close().await.expect(\"close\");\n        runtime.join().await.expect(\"join\");\n    }\n\n    #[tokio::test]\n    async fn lagged_print_reconstructs_live_draft_and_stays_attached() {\n"""
if text.count(needle) != 1:
    raise SystemExit(f"expected one second-test insertion point, found {text.count(needle)}")
text = text.replace(needle, insert)

path.write_text(text)
