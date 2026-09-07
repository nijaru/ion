use super::support::*;

#[tokio::test]
async fn configuration_update_blocks_new_work_but_not_observation_or_close() {
    let root = tempfile::tempdir().unwrap();
    let catalog = ToolCatalog::with_cwd(root.path());
    let configuration = catalog.configuration().clone();
    let runtime = Runtime::start_with_policy(
        ScriptedProvider::new(vec![]),
        catalog,
        SessionStore::open_in_memory().unwrap(),
        permissive_policy(),
    );
    let session = runtime.session();
    session.snapshot().await.unwrap();
    let update = configuration.try_update().unwrap();
    assert_eq!(
        session.submit_if_idle("blocked").await,
        Err(CommandError::ConfigurationBusy)
    );
    assert_eq!(
        session.run_shell("echo blocked", false).await,
        Err(CommandError::ConfigurationBusy)
    );
    assert_eq!(
        session.create_lane("blocked").await,
        Err(CommandError::ConfigurationBusy)
    );
    assert!(session.snapshot().await.unwrap().entries.is_empty());
    assert!(!session.cancel_shell().await.unwrap());
    session.close().await.unwrap();
    runtime.join().await.unwrap();
    update.unchanged();
}

#[tokio::test]
async fn sibling_operation_and_shell_exclude_configuration_until_settlement() {
    let root = tempfile::tempdir().unwrap();
    let catalog = ToolCatalog::with_cwd(root.path());
    let configuration = catalog.configuration().clone();
    let runtime = Runtime::start_with_policy(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "too late",
        )]),
        catalog,
        SessionStore::open_in_memory().unwrap(),
        permissive_policy(),
    );
    let session = runtime.session();
    session.create_lane("worker").await.unwrap();
    let (_, mut events) = session.subscribe_all().await.unwrap();
    let operation = session
        .submit_if_idle_on_lane("worker", "work")
        .await
        .unwrap();
    assert!(matches!(
        configuration.try_update(),
        Err(CommandError::ConfigurationBusy)
    ));
    session.cancel(operation).await.unwrap();
    collect_until_terminal(&mut events).await.unwrap();
    // Terminal delivery and task return are distinct: wait for the effect's
    // lease to leave before opening the next host update.
    timeout(Duration::from_secs(2), async {
        loop {
            if let Ok(update) = configuration.try_update() {
                update.finish();
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    session.run_shell("sleep 30", false).await.unwrap();
    assert!(matches!(
        configuration.try_update(),
        Err(CommandError::ConfigurationBusy)
    ));
    session.cancel_shell().await.unwrap();
    timeout(Duration::from_secs(2), async {
        loop {
            if matches!(
                events.recv().await.unwrap(),
                RuntimeEvent::ShellSettled { .. }
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    configuration.try_update().unwrap().finish();
    session.close().await.unwrap();
    runtime.join().await.unwrap();
}

#[tokio::test]
async fn recovery_cannot_bypass_configuration_update() {
    let root = tempfile::tempdir().unwrap();
    let catalog = ToolCatalog::with_cwd(root.path());
    let configuration = catalog.configuration().clone();
    let store = SessionStore::open_in_memory().unwrap();
    let gate = EffectGate::new(EffectBoundary::ModelExecution);
    let runtime = Runtime::start_with_effect_gate(
        ScriptedProvider::new(vec![]),
        catalog.clone(),
        store.clone(),
        gate.clone(),
    );
    let id = runtime.session_id();
    let session = runtime.session();
    session.snapshot().await.unwrap();
    let submit = tokio::spawn(async move { session.submit_if_idle("work").await });
    gate.wait_until_reached().await;
    assert!(matches!(
        configuration.try_update(),
        Err(CommandError::ConfigurationBusy)
    ));
    runtime.crash();
    gate.release();
    let _ = submit.await.unwrap();
    drop(runtime);
    let update = configuration.try_update().unwrap();
    assert!(matches!(
        Runtime::open_session(
            ScriptedProvider::new(vec![]),
            catalog.clone(),
            store.clone(),
            id
        )
        .await,
        Err(RuntimeError::Command(CommandError::ConfigurationBusy))
    ));
    update.finish();
    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "later",
        )]),
        catalog,
        store,
        id,
    )
    .await
    .unwrap();
    // The transfer lease protects the window before the session task restores.
    assert!(matches!(
        configuration.try_update(),
        Err(CommandError::ConfigurationBusy)
    ));
    let session = runtime.session();
    session.snapshot().await.unwrap();
    session.close().await.unwrap();
    runtime.join().await.unwrap();
    configuration.try_update().unwrap().finish();
}

#[tokio::test]
async fn hosted_admission_and_live_child_share_the_parent_configuration_fence() {
    let root = tempfile::tempdir().unwrap();
    let catalog = ToolCatalog::with_cwd(root.path());
    let configuration = catalog.configuration().clone();
    let store = SessionStore::open_in_memory().unwrap();
    let runtime = Runtime::start_with_store(ScriptedProvider::new(vec![]), catalog, store.clone());
    let family = Arc::new(runtime.agent_family(2).await.unwrap());
    let hosted = crate::hosted_agent_runtimes(
        crate::HostedAgentConfig {
            configuration: configuration.clone(),
            store,
            make_provider: Arc::new(|| {
                ScriptedProvider::new(vec![ScriptedMessage::delayed(
                    Duration::from_secs(30),
                    "later",
                )])
            }),
            make_provider_for_model: None,
            max_active: 2,
            budget: crate::RuntimeBudget::unbounded(),
            trusted_resources: Vec::new(),
            policy: permissive_policy(),
            cwd: root.path().to_path_buf(),
        },
        runtime.session_id(),
    );
    let tools = crate::agent_host_tools(family, hosted.clone());
    let spawn = tools
        .iter()
        .find(|tool| tool.spec().name == "spawn_agent")
        .unwrap();
    let update = configuration.try_update().unwrap();
    let denied = spawn
        .call(
            json!({"objective": "blocked", "topology": "fresh"}),
            CancellationToken::new(),
        )
        .await;
    assert!(denied.is_error);
    assert!(
        denied.output.contains("configuration is busy"),
        "{denied:?}"
    );
    update.finish();
    let started = spawn
        .call(
            json!({"objective": "work", "topology": "fresh"}),
            CancellationToken::new(),
        )
        .await;
    assert!(!started.is_error, "{started:?}");
    assert!(matches!(
        configuration.try_update(),
        Err(CommandError::ConfigurationBusy)
    ));
    hosted.close().await.unwrap();
    configuration.try_update().unwrap().finish();
    runtime.session().close().await.unwrap();
    runtime.join().await.unwrap();
}
