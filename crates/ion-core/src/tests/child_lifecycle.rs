//! Durable child-manager lifecycle tests.

use super::support::*;

fn child_handle(output: &str) -> String {
    output
        .strip_prefix("child handle: ")
        .expect("durable child handle")
        .to_owned()
}

#[tokio::test]
async fn completed_children_release_live_child_slots() {
    let store = SessionStore::open_in_memory().expect("store");
    let parent = crate::SessionId::generate();
    let (manager, tools) = crate::child_tools(
        crate::DelegateConfig {
            store: store.clone(),
            make_provider: Arc::new(|| {
                ScriptedProvider::new(vec![ScriptedMessage::text("child answer")])
            }),
            make_provider_for_model: None,
            max_active_children: 1,
            child_budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        parent,
    );

    let first = tools[0]
        .call(
            json!({"children": [{"objective": "first"}]}),
            CancellationToken::new(),
        )
        .await;
    assert!(!first.is_error, "first child failed: {first:?}");
    let first_handle = child_handle(&first.output);
    let waited = tools[2]
        .call(
            json!({"handle": first_handle}),
            CancellationToken::new(),
        )
        .await;
    assert!(!waited.is_error, "first wait failed: {waited:?}");
    assert!(waited.output.contains("finished"), "{waited:?}");

    let second = tools[0]
        .call(
            json!({"children": [{"objective": "second"}]}),
            CancellationToken::new(),
        )
        .await;
    assert!(
        !second.is_error,
        "a completed child must not consume the one live slot: {second:?}"
    );
    let second_handle = child_handle(&second.output);
    let waited = tools[2]
        .call(
            json!({"handle": second_handle}),
            CancellationToken::new(),
        )
        .await;
    assert!(!waited.is_error, "second wait failed: {waited:?}");

    manager.close().await.expect("close children");
    store.close().await.expect("close store");
}
