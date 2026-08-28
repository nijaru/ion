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
    let active_agent = family
        .admit_lane(root)
        .await
        .expect("active agent admission");
    let waiting_agent = family
        .admit_lane(root)
        .await
        .expect("waiting agent admission");

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
        family
            .start(waiting_agent, "must remain capacity-bound")
            .await,
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

#[tokio::test]
async fn agent_waits_are_event_driven_and_completion_is_non_consuming() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(220),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store);
    let family = runtime.agent_family(2).await.expect("family");
    let root = family.root();
    let first = family.admit_lane(root).await.expect("first admission");
    let second = family.admit_lane(root).await.expect("second admission");
    let first_operation = family
        .start(first, "first wait")
        .await
        .expect("first start");
    let second_operation = family
        .start(second, "second wait")
        .await
        .expect("second start");

    let cancelled = CancellationToken::new();
    let cancelled_for_wait = cancelled.clone();
    let cancelled_wait = family.wait_one(first, cancelled_for_wait, None);
    tokio::pin!(cancelled_wait);
    tokio::select! {
        result = &mut cancelled_wait => panic!("wait completed before explicit cancellation: {result:?}"),
        () = sleep(Duration::from_millis(30)) => cancelled.cancel(),
    }
    assert!(matches!(
        cancelled_wait.await,
        Err(crate::AgentError::WaitCancelled)
    ));

    let deadline = std::time::Instant::now() + Duration::from_millis(30);
    assert!(matches!(
        family
            .wait_one(second, CancellationToken::new(), Some(deadline))
            .await,
        Err(crate::AgentError::WaitDeadlineElapsed)
    ));

    let completed = family
        .wait_all(&[first, second], CancellationToken::new(), None)
        .await
        .expect("wait all");
    assert_eq!(completed.len(), 2);
    assert!(matches!(
        &completed[0].1,
        crate::AgentStatus::Finished { operation_id, .. } if *operation_id == first_operation
    ));
    assert!(matches!(
        &completed[1].1,
        crate::AgentStatus::Finished { operation_id, .. } if *operation_id == second_operation
    ));

    // Earlier cancelled/timed-out waiters did not consume completion.
    assert!(matches!(
        family
            .wait_one(first, CancellationToken::new(), None)
            .await
            .expect("repeat wait"),
        crate::AgentStatus::Finished { operation_id, .. } if operation_id == first_operation
    ));

    let third = family.admit_lane(root).await.expect("third admission");
    let third_operation = family
        .start(third, "third wait")
        .await
        .expect("third start");
    let (winner, winner_status) = family
        .wait_any(&[third, first], CancellationToken::new(), None)
        .await
        .expect("wait any");
    assert_eq!(winner, first);
    assert!(matches!(
        winner_status,
        crate::AgentStatus::Finished { operation_id, .. } if operation_id == first_operation
    ));
    assert!(matches!(
        family.status(third).await.expect("third remains active"),
        crate::AgentStatus::Active { operation_id, .. } if operation_id == third_operation
    ));

    family
        .wait_one(third, CancellationToken::new(), None)
        .await
        .expect("third finishes");
    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn agent_wait_set_rejects_missing_execution_and_duplicates() {
    let runtime = tool_runtime();
    let family = runtime.agent_family(1).await.expect("family");
    let root = family.root();
    let idle = family.admit_lane(root).await.expect("idle admission");

    assert!(matches!(
        family
            .wait_one(idle, CancellationToken::new(), None)
            .await,
        Err(crate::AgentError::NotRunning(agent)) if agent == idle
    ));
    assert!(matches!(
        family.wait_all(&[], CancellationToken::new(), None).await,
        Err(crate::AgentError::EmptyWaitSet)
    ));
    assert!(matches!(
        family
            .wait_any(&[idle, idle], CancellationToken::new(), None)
            .await,
        Err(crate::AgentError::DuplicateWaitTarget(agent)) if agent == idle
    ));

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}
