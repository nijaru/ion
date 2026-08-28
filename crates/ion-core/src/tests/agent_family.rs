use super::support::*;

#[tokio::test]
async fn retained_agent_identity_is_separate_from_execution_capacity() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(500),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store.clone());
    let family = runtime.agent_family(1).await.expect("family");
    let root = family.root();
    assert_eq!(root, crate::AgentId::root(runtime.session_id()));

    let first = family.admit_lane(root).await.expect("first admission");
    let second = family.admit_lane(root).await.expect("second admission");
    assert_ne!(first, second);

    let first_operation = family
        .start(first, "first objective")
        .await
        .expect("first start");
    assert!(matches!(
        family.start(second, "second objective").await,
        Err(crate::AgentError::Capacity)
    ));
    // The second identity was durably admitted even though it has no execution
    // permit yet.
    assert_eq!(
        family.status(second).await.expect("second status"),
        crate::AgentStatus::Admitted
    );

    sleep(Duration::from_millis(650)).await;
    assert!(matches!(
        family.status(first).await.expect("first terminal"),
        crate::AgentStatus::Finished { operation_id, .. } if operation_id == first_operation
    ));

    let second_operation = family
        .start(second, "second objective")
        .await
        .expect("capacity released after terminal observation");
    assert_ne!(first_operation, second_operation);
    assert!(matches!(
        family.status(second).await.expect("second active"),
        crate::AgentStatus::Active { operation_id, .. } if operation_id == second_operation
    ));

    let loaded = store.load(runtime.session_id()).await.expect("family load");
    assert_eq!(loaded.agents.len(), 3);
    assert!(loaded.agents.iter().any(|agent| agent.id == first));
    assert!(loaded.agents.iter().any(|agent| agent.id == second));

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}
