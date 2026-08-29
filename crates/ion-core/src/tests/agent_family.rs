use super::support::*;

#[tokio::test]
async fn lane_agents_are_structurally_read_only() {
    let provider = SharedLogProvider::default();
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = ToolCatalog::default();
    let runtime = Runtime::start_with_policy(
        provider.clone(),
        catalog.clone(),
        store.clone(),
        permissive_policy(),
    );
    let family = Arc::new(runtime.agent_family(1).await.expect("family"));
    crate::install_agent_tools(&catalog, Arc::clone(&family));
    let agent = family
        .admit_lane(family.root())
        .await
        .expect("agent admission");
    family
        .start(agent, "inspect only")
        .await
        .expect("agent start");

    timeout(Duration::from_secs(2), async {
        loop {
            if !provider.requests().is_empty() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("provider request");

    let requests = provider.requests();
    let names = requests[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["find", "read", "search"]);
    assert!(!names.iter().any(|name| matches!(
        *name,
        "write" | "edit" | "bash" | "spawn_agent" | "agent_start" | "agent_send"
    )));

    let loaded = store.load(runtime.session_id()).await.expect("load");
    let record = loaded
        .agents
        .iter()
        .find(|record| record.id == agent)
        .expect("agent record");
    let lane = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == record.lane_name)
        .expect("agent lane");
    assert_eq!(lane.config.tools, crate::tool::ToolSelection::read_only());

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}

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

#[tokio::test]
async fn agent_messages_use_durable_inbox_and_preserve_sender_provenance() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(180),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime =
        start_runtime_with_store(provider.clone(), ToolRegistry::default(), store.clone());
    let family = runtime.agent_family(1).await.expect("family");
    let root = family.root();
    let sender = family.admit_lane(root).await.expect("sender admission");
    let target = family.admit_lane(root).await.expect("target admission");
    let target_operation = family
        .start(target, "initial target work")
        .await
        .expect("target start");

    timeout(Duration::from_secs(2), async {
        loop {
            if !provider.requests().is_empty() {
                break;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first provider step started");

    assert_eq!(
        family
            .send(sender, target, "coordinate on the API boundary")
            .await
            .expect("active message delivery"),
        target_operation
    );
    family
        .wait_one(target, CancellationToken::new(), None)
        .await
        .expect("target completion");

    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "queued message must cause a continuation step"
    );
    assert!(requests[1].plan.messages.iter().any(|message| {
        matches!(
            message,
            crate::ContextMessage::User { content }
                if content == &format!(
                    "[Message from {sender}]\ncoordinate on the API boundary"
                )
        )
    }));

    let idle_target = family
        .admit_lane(root)
        .await
        .expect("idle target admission");
    let idle_operation = family
        .send(sender, idle_target, "start from this handoff")
        .await
        .expect("idle message delivery");
    assert!(matches!(
        family.status(idle_target).await.expect("idle target became active"),
        crate::AgentStatus::Active { operation_id, .. } if operation_id == idle_operation
    ));
    family
        .wait_one(idle_target, CancellationToken::new(), None)
        .await
        .expect("idle-message operation completion");

    let loaded = store
        .load(runtime.session_id())
        .await
        .expect("load messages");
    let messages = loaded
        .entries
        .iter()
        .filter_map(|entry| match &entry.entry {
            crate::SessionEntry::AgentMessage { from, text } => Some((*from, text.clone())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        vec![
            (sender, "coordinate on the API boundary".to_owned()),
            (sender, "start from this handoff".to_owned()),
        ]
    );

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn model_facing_agent_tools_use_family_authority_and_report_exact_result() {
    let provider = SharedLogProvider::default();
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store);
    let family = Arc::new(runtime.agent_family(1).await.expect("family"));
    let tools = crate::agent_tools(Arc::clone(&family));

    let spawn = tools[0]
        .call(
            json!({"objective": "inspect the shared branch"}),
            CancellationToken::new(),
        )
        .await;
    assert!(!spawn.is_error, "spawn failed: {spawn:?}");
    let handle = spawn
        .output
        .lines()
        .find_map(|line| line.strip_prefix("agent handle: "))
        .expect("durable agent handle")
        .to_owned();

    let waited = tools[3]
        .call(json!({"handle": handle}), CancellationToken::new())
        .await;
    assert!(!waited.is_error, "wait failed: {waited:?}");
    assert!(waited.output.contains("finished"), "{waited:?}");
    assert!(waited.output.contains("working"), "{waited:?}");

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn idle_agent_message_respects_execution_capacity() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(500),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store);
    let family = runtime.agent_family(1).await.expect("family");
    let root = family.root();
    let first = family.admit_lane(root).await.expect("first agent");
    let second = family.admit_lane(root).await.expect("second agent");

    family
        .send(root, first, "start from a message")
        .await
        .expect("first message starts execution");
    assert!(matches!(
        family.send(root, second, "must remain blocked").await,
        Err(crate::AgentError::Capacity)
    ));
    assert_eq!(
        family.status(second).await.expect("second status"),
        crate::AgentStatus::Admitted
    );

    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn unified_agent_host_tools_route_lane_fresh_and_fork_without_child_namespace() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        SharedLogProvider::default(),
        ToolRegistry::default(),
        store.clone(),
    );
    let family = Arc::new(runtime.agent_family(2).await.expect("family"));
    let (children, _legacy_child_tools) = crate::child_tools(
        crate::DelegateConfig {
            store: store.clone(),
            make_provider: Arc::new(|| {
                ScriptedProvider::new(vec![ScriptedMessage::text("session agent answer")])
            }),
            make_provider_for_model: None,
            max_active_children: 2,
            child_budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            cwd: std::env::current_dir().expect("cwd"),
        },
        runtime.session_id(),
    );
    let tools = crate::agent_host_tools(Arc::clone(&family), Arc::clone(&children));
    let names = tools
        .iter()
        .map(|tool| tool.spec().name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "spawn_agent",
            "agent_start",
            "agent_status",
            "agent_wait",
            "agent_cancel",
            "agent_resume",
            "agent_send",
        ]
    );
    assert!(names.iter().all(|name| !name.contains("child")));

    let spawn = |arguments| {
        let tools = &tools;
        async move {
            tools
                .iter()
                .find(|tool| tool.spec().name == "spawn_agent")
                .expect("spawn tool")
                .call(arguments, CancellationToken::new())
                .await
        }
    };
    let handle_from = |output: &str| {
        output
            .lines()
            .find_map(|line| line.strip_prefix("agent handle: "))
            .expect("agent handle")
            .to_owned()
    };
    let wait = |handle: String| {
        let tools = &tools;
        async move {
            tools
                .iter()
                .find(|tool| tool.spec().name == "agent_wait")
                .expect("wait tool")
                .call(json!({"handle": handle}), CancellationToken::new())
                .await
        }
    };

    let lane = spawn(json!({"objective": "lane work"})).await;
    assert!(!lane.is_error, "lane spawn failed: {lane:?}");
    let lane_handle = handle_from(&lane.output);
    let lane_wait = wait(lane_handle).await;
    assert!(!lane_wait.is_error, "lane wait failed: {lane_wait:?}");
    assert!(lane_wait.output.contains("working"), "{lane_wait:?}");

    let fresh = spawn(json!({"objective": "fresh work", "topology": "fresh"})).await;
    assert!(!fresh.is_error, "fresh spawn failed: {fresh:?}");
    assert!(!fresh.output.contains("child"), "{fresh:?}");
    let fresh_handle = handle_from(&fresh.output);
    let fresh_agent =
        crate::AgentId::parse(fresh_handle.strip_prefix("agent-").expect("agent prefix"))
            .expect("fresh agent id");
    let fresh_session = crate::SessionId::from_uuid(fresh_agent.as_uuid());
    let fresh_loaded = store.load(fresh_session).await.expect("fresh session");
    assert_eq!(
        fresh_loaded.session.control_parent_session_id,
        Some(runtime.session_id())
    );
    assert_eq!(fresh_loaded.session.fork_source_session_id, None);
    assert_eq!(fresh_loaded.agents.len(), 1);
    assert_eq!(fresh_loaded.agents[0].id, fresh_agent);
    assert_eq!(
        fresh_loaded.agents[0].family_session_id,
        runtime.session_id()
    );
    assert_eq!(
        fresh_loaded.agents[0].control_parent_id,
        Some(crate::AgentId::root(runtime.session_id()))
    );
    assert!(matches!(
        fresh_loaded.agents[0].history,
        crate::store::AgentHistory::Fresh
    ));
    let fresh_wait = wait(fresh_handle).await;
    assert!(!fresh_wait.is_error, "fresh wait failed: {fresh_wait:?}");
    assert!(
        fresh_wait.output.contains("session agent answer"),
        "{fresh_wait:?}"
    );
    assert!(!fresh_wait.output.contains("child "), "{fresh_wait:?}");

    let fork = spawn(json!({"objective": "fork work", "topology": "fork"})).await;
    assert!(!fork.is_error, "fork spawn failed: {fork:?}");
    let fork_handle = handle_from(&fork.output);
    let fork_agent =
        crate::AgentId::parse(fork_handle.strip_prefix("agent-").expect("agent prefix"))
            .expect("fork agent id");
    let fork_session = crate::SessionId::from_uuid(fork_agent.as_uuid());
    let fork_loaded = store.load(fork_session).await.expect("fork session");
    assert_eq!(
        fork_loaded.session.control_parent_session_id,
        Some(runtime.session_id())
    );
    assert_eq!(
        fork_loaded.session.fork_source_session_id,
        Some(runtime.session_id())
    );
    let fork_source_entry = fork_loaded.session.fork_source_entry_id;
    assert_eq!(fork_loaded.agents.len(), 1);
    assert_eq!(fork_loaded.agents[0].id, fork_agent);
    assert_eq!(
        fork_loaded.agents[0].family_session_id,
        runtime.session_id()
    );
    assert_eq!(
        fork_loaded.agents[0].control_parent_id,
        Some(crate::AgentId::root(runtime.session_id()))
    );
    assert!(matches!(
        fork_loaded.agents[0].history,
        crate::store::AgentHistory::Fork {
            source_session_id,
            source_entry_id,
        } if source_session_id == runtime.session_id() && source_entry_id == fork_source_entry
    ));
    let fork_wait = wait(fork_handle).await;
    assert!(!fork_wait.is_error, "fork wait failed: {fork_wait:?}");
    assert!(
        fork_wait.output.contains("session agent answer"),
        "{fork_wait:?}"
    );

    children.close().await.expect("close session agents");
    runtime.session().close().await.expect("close root");
    runtime.join().await.expect("join root");
    store.close().await.expect("close store");
}
