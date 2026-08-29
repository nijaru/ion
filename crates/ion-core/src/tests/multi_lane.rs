use super::support::*;

#[tokio::test]
async fn two_lanes_run_slow_provider_effects_concurrently_under_one_writer() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(500),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime =
        start_runtime_with_store(provider.clone(), ToolRegistry::default(), store.clone());
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe_all().await.expect("subscribe all");

    let main_operation = session
        .submit_if_idle("main prompt")
        .await
        .expect("main operation");
    timeout(Duration::from_millis(200), async {
        loop {
            if provider.requests().len() == 1 {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("main provider request started");

    session.create_lane("worker").await.expect("worker lane");
    let worker_operation = session
        .submit_if_idle_on_lane("worker", "worker prompt")
        .await
        .expect("worker operation");
    assert_ne!(main_operation, worker_operation);

    // The first provider effect cannot settle before 500 ms. Requiring the
    // second request within 200 ms proves the session writer admitted another
    // lane while the first slow effect remained in flight.
    timeout(Duration::from_millis(200), async {
        loop {
            if provider.requests().len() == 2 {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("worker provider request started concurrently");

    let requests = provider.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.operation_id == main_operation)
    );
    assert!(
        requests
            .iter()
            .any(|request| request.operation_id == worker_operation)
    );

    let worker_request = requests
        .iter()
        .find(|request| request.operation_id == worker_operation)
        .expect("worker request");
    let worker_user_messages = worker_request
        .plan
        .messages
        .iter()
        .filter_map(|message| match message {
            ContextMessage::User { content } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(worker_user_messages.contains(&"main prompt"));
    assert!(worker_user_messages.contains(&"worker prompt"));

    let mut finished = Vec::new();
    timeout(Duration::from_secs(2), async {
        while finished.len() < 2 {
            match events.recv().await.expect("runtime event") {
                RuntimeEvent::OperationFinished { operation_id, .. }
                    if (operation_id == main_operation || operation_id == worker_operation)
                        && !finished.contains(&operation_id) =>
                {
                    finished.push(operation_id);
                }
                _ => {}
            }
        }
    })
    .await
    .expect("both lanes finish");

    let loaded = store.load(session_id).await.expect("load session");
    let main = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == crate::session::lane::MAIN)
        .expect("main lane");
    let worker = loaded
        .lanes
        .iter()
        .find(|lane| lane.name == "worker")
        .expect("worker lane");
    assert!(main.state.current_operation.is_none());
    assert!(worker.state.current_operation.is_none());
    assert_ne!(main.state.leaf, worker.state.leaf);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn frontend_subscription_projects_only_the_main_lane() {
    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(250),
        ..SharedLogProvider::default()
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store);
    let session = runtime.session();

    session.create_lane("worker").await.expect("worker lane");
    let (snapshot, mut frontend_events) = session.subscribe().await.expect("frontend subscribe");
    assert!(matches!(snapshot.operation, OperationStatus::Idle));
    let (_snapshot, mut all_events) = session.subscribe_all().await.expect("family subscribe");

    let worker_operation = session
        .submit_if_idle_on_lane("worker", "worker prompt")
        .await
        .expect("worker operation");
    let observed_worker = timeout(Duration::from_secs(1), async {
        loop {
            if let RuntimeEvent::OperationStarted { operation_id, .. } =
                all_events.recv().await.expect("all-lane event")
            {
                break operation_id;
            }
        }
    })
    .await
    .expect("family observer sees worker start");
    assert_eq!(observed_worker, worker_operation);

    let snapshot = session.snapshot().await.expect("main snapshot");
    assert!(matches!(snapshot.operation, OperationStatus::Idle));
    let main_operation = session
        .submit_if_idle("main prompt")
        .await
        .expect("main operation");
    let observed_main = timeout(Duration::from_secs(1), async {
        loop {
            if let RuntimeEvent::OperationStarted { operation_id, .. } =
                frontend_events.recv().await.expect("frontend event")
            {
                break operation_id;
            }
        }
    })
    .await
    .expect("frontend observer sees main start");
    assert_eq!(observed_main, main_operation);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
