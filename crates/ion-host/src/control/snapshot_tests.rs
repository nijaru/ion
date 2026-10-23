use super::*;

#[tokio::test]
async fn snapshots_retain_bounded_recovery_without_consuming_owner_results() {
    let model = Model::new(300);
    let mut fixture = Fixture::new(model.clone(), false);
    let control = &mut fixture.control;
    let key = ion_core::RequestKey::new("snapshot".into(), "root".into()).unwrap();
    let root = match control
        .submit_request(
            &key,
            Message::user_input("root".into(), []),
            meta("root metadata"),
        )
        .unwrap()
    {
        RequestAdmission::Started { id, .. } => id,
        _ => panic!("fresh admission"),
    };
    // Synchronous durable acceptance precedes the asynchronous Host callback.
    // The view must not equate absent acknowledgement with absent admission.
    let early = control.snapshot(|m| m.literal.clone()).unwrap();
    assert_eq!(early.session.unfinished_turn, Some(1));
    assert_eq!(early.state.operation.unwrap().turn, Some(1));
    assert!(matches!(early.inputs[0].phase, InputCustodyPhase::InFlight));
    assert_eq!(early.inputs[0].id, root);
    for i in 1..MAX_ADMISSIONS {
        control
            .submit(
                Delivery::FollowUp,
                UserInput::Message(Message::user_input(format!("literal ${i}"), [])),
                meta(&format!("queued {i}")),
            )
            .unwrap();
    }
    let queued = control.snapshot(|m| m.literal.clone()).unwrap();
    for input in queued.inputs.iter().filter(|input| input.id != root) {
        assert_eq!(input.phase, InputCustodyPhase::Prepared);
        assert!(matches!(input.original, Some(UserInput::Message(_))));
        assert!(input.withdrawal.is_none());
    }
    control.clear_queue();
    model.wait_calls(1).await;
    model.release();
    tokio::time::timeout(Duration::from_secs(5), async {
        while control.is_busy() {
            control.maintain().await;
        }
    })
    .await
    .unwrap();
    let snapshot = control.snapshot(|m| m.literal.clone()).unwrap();
    assert!(snapshot.cursor.read > early.cursor.read);
    assert_eq!(snapshot.cursor.owner, early.cursor.owner);
    assert_eq!(
        snapshot.cursor.committed as usize,
        snapshot.session.entries.len()
    );
    assert!(!snapshot.state.busy);
    assert!(snapshot.inputs.is_empty());
    assert_eq!(
        snapshot
            .updates
            .iter()
            .filter(|u| matches!(u, UpdateSnapshot::Returned { .. }))
            .count(),
        MAX_ADMISSIONS - 1
    );
    assert!(snapshot.updates.iter().any(|u| matches!(u, UpdateSnapshot::Returned { input: UserInput::Message(message), metadata, .. } if message == &Message::user_input("literal $1".into(), []) && metadata == "queued 1")));
    assert!(snapshot.updates.iter().any(|u| matches!(u, UpdateSnapshot::Settled { turn: Some(1), accepted, .. } if accepted[0].0 == root && accepted[0].1.turn == 1 && accepted[0].2 == "root metadata")));
    let rejected = control
        .submit(
            Delivery::FollowUp,
            prompt("new literal"),
            meta("new metadata"),
        )
        .unwrap_err();
    assert_eq!(rejected.metadata.literal, "new metadata");
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    // Repeated reads neither acknowledge results nor free the reserved slots.
    let again = control.snapshot(|_| ()).unwrap();
    assert_eq!(again.cursor.committed, snapshot.cursor.committed);
    assert!(again.cursor.read > snapshot.cursor.read);
    assert_eq!(again.updates.len(), snapshot.updates.len());
    let mut returned = 0;
    while let Some(update) = control.take_update() {
        if let ControlUpdate::Returned(input) = update {
            assert_eq!(input.metadata.marker.get(), 7);
            returned += 1;
        }
    }
    assert_eq!(returned, MAX_ADMISSIONS - 1);
    assert!(control.snapshot(|_| ()).unwrap().updates.is_empty());
    // Explicit transfer frees capacity; no receipt ring silently evicts originals.
    control
        .submit(
            Delivery::FollowUp,
            UserInput::Message(Message::user_input("new work".into(), [])),
            meta("new work"),
        )
        .unwrap();
    model.release();
    while control.is_busy() {
        control.maintain().await;
    }
    assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    control.shutdown().await;
}

#[tokio::test]
async fn acknowledged_metadata_keeps_its_byte_reservation_until_settlement_transfer() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    let message = Message::user_input("root".into(), []);
    let input = UserInput::Message(message.clone());
    let metadata = meta(&"m".repeat(8192));
    let retained = serde_json::to_vec(&(&message, &(&input, &metadata)))
        .unwrap()
        .len();
    // Room for the separately admitted root message, but not this 2 KiB probe
    // while the original metadata reservation remains in Host custody.
    let budget = InputBudget::new(retained + 1024);
    fixture.control.budget = budget.clone();
    let probe = || {
        budget.admit(
            Message::user_input("p".repeat(2048), []),
            (),
            AgentLimits::default(),
        )
    };
    let root = fixture
        .control
        .submit(Delivery::Start, input, metadata)
        .unwrap();
    model.wait_calls(1).await;
    let snapshot = fixture.control.snapshot(|_| ()).unwrap();
    assert!(matches!(
        snapshot.inputs[0].phase,
        InputCustodyPhase::Acknowledged(_)
    ));
    assert!(matches!(
        probe(),
        Err(CodingAgentError::InputQueueFull { .. })
    ));

    model.release();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.control.is_busy() {
            fixture.control.maintain().await;
        }
    })
    .await
    .unwrap();
    assert!(matches!(
        probe(),
        Err(CodingAgentError::InputQueueFull { .. })
    ));
    let mut accepted = Vec::new();
    while let Some(update) = fixture.control.take_update() {
        if let ControlUpdate::Settled(settlement) = update {
            accepted.extend(settlement.accepted);
        }
    }
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0].id, root);
    assert_eq!(accepted[0].metadata.literal, "m".repeat(8192));
    assert!(
        probe().is_ok(),
        "only explicit transfer releases Host byte custody"
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn cancelled_shutdown_preserves_inspectable_recovery_until_explicit_transfer() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    let control = &mut fixture.control;
    control
        .submit(
            Delivery::Start,
            UserInput::Message(Message::user_input("root".into(), [])),
            meta("root"),
        )
        .unwrap();
    control
        .submit(
            Delivery::FollowUp,
            prompt("unexpanded literal"),
            meta("returned"),
        )
        .unwrap();
    model.wait_calls(1).await;
    let mut waiting = Box::pin(control.join_shutdown());
    assert!(futures_util::poll!(waiting.as_mut()).is_pending());
    drop(waiting);
    let closing = control.snapshot(|m| m.literal.clone()).unwrap();
    assert!(closing.state.closed);
    assert!(closing.state.stop_requested);
    assert!(!closing.state.shutdown_complete);
    control.join_shutdown().await;
    let joined = control.snapshot(|m| m.literal.clone()).unwrap();
    assert!(joined.state.shutdown_complete);
    assert!(!joined.state.busy);
    assert!(
        joined.updates.iter().any(
            |u| matches!(u, UpdateSnapshot::Returned { metadata, .. } if metadata == "returned")
        )
    );
    assert!(
        joined
            .updates
            .iter()
            .any(|u| matches!(u, UpdateSnapshot::Settled { turn: Some(1), .. }))
    );
    let report = control.shutdown().await;
    assert!(report.cleanup_errors.is_empty());
    assert!(report.updates.iter().any(
        |u| matches!(u, ControlUpdate::Returned(input) if input.metadata.literal == "returned")
    ));
    assert!(control.snapshot(|_| ()).unwrap().updates.is_empty());
}
