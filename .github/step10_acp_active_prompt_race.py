from pathlib import Path

path = Path("crates/ion/src/acp.rs")
text = path.read_text()

old = """                            finish_prompt(&output, Some(request_id), stop, active_prompt).await;\n"""
new = """                            finish_prompt(\n                                &output,\n                                Some(request_id),\n                                stop,\n                                operation_id,\n                                active_prompt,\n                            )\n                            .await;\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one prompt completion call, found {text.count(old)}")
text = text.replace(old, new)

old = """async fn finish_prompt<W>(\n    output: &Arc<Mutex<W>>,\n    id: Option<Value>,\n    stop: TurnStop,\n    active_prompt: Arc<Mutex<Option<(Value, OperationId)>>>,\n) where\n    W: AsyncWrite + Unpin + Send + 'static,\n{\n    active_prompt.lock().await.take();\n"""
new = """async fn finish_prompt<W>(\n    output: &Arc<Mutex<W>>,\n    id: Option<Value>,\n    stop: TurnStop,\n    operation_id: OperationId,\n    active_prompt: Arc<Mutex<Option<(Value, OperationId)>>>,\n) where\n    W: AsyncWrite + Unpin + Send + 'static,\n{\n    let mut active = active_prompt.lock().await;\n    if active\n        .as_ref()\n        .is_some_and(|(_, active_operation_id)| *active_operation_id == operation_id)\n    {\n        active.take();\n    }\n    drop(active);\n"""
if text.count(old) != 1:
    raise SystemExit(f"expected one finish_prompt body, found {text.count(old)}")
text = text.replace(old, new)

needle = """    #[tokio::test]\n    async fn lagged_pump_classifies_missed_completion_from_snapshot() {\n"""
insert = """    #[tokio::test]\n    async fn finish_prompt_clears_only_its_own_active_turn() {\n        let old = OperationId::generate();\n        let newer = OperationId::generate();\n        let active_prompt = Arc::new(Mutex::new(Some((json!(2), newer))));\n        let output = Arc::new(Mutex::new(tokio::io::sink()));\n\n        finish_prompt(\n            &output,\n            Some(json!(1)),\n            TurnStop::EndTurn,\n            old,\n            Arc::clone(&active_prompt),\n        )\n        .await;\n        assert_eq!(\n            active_prompt.lock().await.as_ref().map(|(_, id)| *id),\n            Some(newer),\n            "an older prompt must not erase cancellation ownership for a newer turn",\n        );\n\n        finish_prompt(\n            &output,\n            Some(json!(2)),\n            TurnStop::Cancelled,\n            newer,\n            Arc::clone(&active_prompt),\n        )\n        .await;\n        assert!(\n            active_prompt.lock().await.is_none(),\n            "a prompt must release its own cancellation ownership when it finishes",\n        );\n    }\n\n    #[tokio::test]\n    async fn lagged_pump_classifies_missed_completion_from_snapshot() {\n"""
if text.count(needle) != 1:
    raise SystemExit(f"expected one test insertion point, found {text.count(needle)}")
text = text.replace(needle, insert)

path.write_text(text)
