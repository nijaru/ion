//! Runtime budget tests.

use super::support::*;

// ---- Runtime budgets (§20.5) ----

#[tokio::test]
async fn model_step_budget_fails_the_operation_visibly() {
    // The model keeps requesting tool calls; the budget stops the loop
    // after one step.
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo one" }),
        },
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo two" }),
        },
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_budgeted(
        provider,
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
        crate::RuntimeBudget {
            max_model_steps: Some(1),
            max_tool_calls: None,
        },
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(
            |e| matches!(e, RuntimeEvent::OperationFailed { message, .. }
                if message.contains("budget"))
        ),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let loaded = store.load(session_id).await.expect("load");
    // The failed operation is durable: it appears in the operation
    // records with a Failed outcome, not just as a live event.
    assert!(
        loaded.operations.iter().any(|op| {
            serde_json::to_string(&op.latest)
                .map(|text| text.contains("budget"))
                .unwrap_or(false)
        }),
        "failed outcome persisted"
    );
}

#[tokio::test]
async fn tool_call_budget_denies_further_tools_model_visibly() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo one" }),
        },
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo two" }),
        },
        ScriptedMessage::text("done"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_budgeted(
        provider,
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
        crate::RuntimeBudget {
            max_model_steps: None,
            max_tool_calls: Some(1),
        },
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "the model can finish its turn after denials: {recorded:?}"
    );
    let session_id = runtime.session_id();
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Exactly one tool effect was admitted; the second call settled as
    // a model-visible denial.
    let loaded = store.load(session_id).await.expect("load");
    let tool_intents = loaded
        .entries
        .iter()
        .filter(|record| {
            let entry = &record.entry;
            serde_json::to_string(entry)
                .map(|text| text.contains("echo two"))
                .unwrap_or(false)
        })
        .count();
    assert_eq!(tool_intents, 1, "second call denied, first admitted");
}
