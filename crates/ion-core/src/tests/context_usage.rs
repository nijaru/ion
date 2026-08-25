//! Context usage tests.

use super::support::*;

// ---- Context projection and usage ledger (DESIGN.md §32 Step 4 slice 1) ----

fn plan_of(entries: &[SessionEntry]) -> crate::context::ContextPlan {
    crate::context::project(entries, 1)
}

#[test]
fn projector_is_deterministic_and_pairs_tool_calls() {
    let entries = vec![
        SessionEntry::UserMessage {
            text: "goal".to_owned(),
        },
        SessionEntry::AssistantMessage {
            text: "reading".to_owned(),
        },
        SessionEntry::ToolCall {
            call: ToolCall {
                operation_id: OperationId::generate(),
                call_id: 7,
                name: "read".to_owned(),
                arguments: serde_json::json!({ "path": "a.txt" }),
            },
        },
        SessionEntry::ToolResult {
            result: ToolResult::Ok {
                call_id: 7,
                output: "contents".to_owned(),
                artifact: None,
            },
        },
    ];
    let first = plan_of(&entries);
    let second = plan_of(&entries);
    assert_eq!(first, second, "the same entries must project identically");
    assert_eq!(first.messages.len(), 3);
    let crate::context::ContextMessage::Assistant { tool_calls, .. } = &first.messages[1] else {
        panic!("tool call must attach to the assistant message");
    };
    assert_eq!(tool_calls.len(), 1);
    let crate::context::ContextMessage::Tool { call_id, content } = &first.messages[2] else {
        panic!("tool result must project as a tool message");
    };
    assert_eq!((*call_id, content.as_str()), (7, "contents"));
}

#[tokio::test]
async fn trusted_resources_enter_future_model_steps_only_when_host_supplies_them() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("AGENTS.md"), "project rules").expect("resource");
    let trusted = load_trusted_resources(root.path(), true).expect("trusted resources");
    assert_eq!(trusted.len(), 1);

    let provider = SharedLogProvider::default();
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_policy_and_resources(
        provider.clone(),
        ToolRegistry::default(),
        store,
        Arc::new(crate::policy::DefaultPolicy),
        trusted,
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        provider
            .requests()
            .first()
            .expect("model request")
            .plan
            .system
            .contains("[Trusted project resource: AGENTS.md]\nproject rules")
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn usage_persists_with_the_settlement() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::Usage(crate::provider::TokenUsage {
                input: 100,
                output: 20,
                cache_read: 60,
                cache_write: 0,
            }),
            ScriptedMessage::text("done"),
        ]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let rows = store.usage(session_id).await.expect("usage rows");
    assert_eq!(
        rows.len(),
        1,
        "usage must persist exactly once at the settlement boundary"
    );
    assert_eq!(rows[0].input_tokens, 100);
    assert_eq!(rows[0].output_tokens, 20);
    assert_eq!(rows[0].cache_read_tokens, 60);
    assert_eq!(rows[0].cache_write_tokens, 0);
    assert_eq!(rows[0].step, 1);
}

#[tokio::test]
async fn usage_survives_a_failed_operation() {
    // §27.2: usage is independent of operation success — a failed
    // step's tokens still land in the ledger via the settlement commit.
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::Usage(crate::provider::TokenUsage {
                input: 5,
                output: 1,
                cache_read: 0,
                cache_write: 0,
            }),
            ScriptedMessage::Fail {
                message: "boom".to_owned(),
            },
        ]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    // The step fails visibly; the usage row must still be committed.
    collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let rows = store.usage(session_id).await.expect("usage rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].input_tokens, 5);
}

#[tokio::test]
async fn cache_expectation_records_cold_start_then_stable_prefix_reuse() {
    let db = temp_db("cache-expectation");
    let store = SessionStore::open(&db).expect("store");
    let runtime = Runtime::start_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("first"),
            ScriptedMessage::text("second"),
        ])
        .with_prompt_cache(true),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("one").await.expect("first submit");
    collect_until_terminal(&mut events)
        .await
        .expect("first collect");
    session.submit_if_idle("two").await.expect("second submit");
    collect_until_terminal(&mut events)
        .await
        .expect("second collect");
    let session_id = runtime.session_id();
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let connection = rusqlite::Connection::open(&db).expect("open db");
    let expectations: Vec<String> = connection
        .prepare("SELECT cache_expectation FROM model_steps ORDER BY created_at")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    assert_eq!(expectations, ["cold_start", "prefix_reuse_expected"]);
    let fingerprints: Vec<String> = connection
        .prepare("SELECT context_fingerprint FROM model_steps ORDER BY created_at")
        .expect("prepare fingerprints")
        .query_map([], |row| row.get(0))
        .expect("query fingerprints")
        .collect::<Result<Vec<_>, _>>()
        .expect("fingerprint rows");
    assert_eq!(fingerprints.len(), 2);
    assert_eq!(fingerprints[0], fingerprints[1]);
    let _ = store.load(session_id).await.expect("load");
}
