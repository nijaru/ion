use super::*;

struct InterleavingMetadata(Option<Box<dyn Fn() + Send>>);
impl Serialize for InterleavingMetadata {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let Some(interleave) = &self.0 {
            interleave();
        }
        serializer.serialize_str("metadata")
    }
}
fn literal(text: &str) -> Message {
    Message::user_input(text.into(), [])
}

#[tokio::test]
async fn live_producer_and_controller_share_fifo_admission_and_capacity() {
    for full in [false, true] {
        let model = Model::new(1);
        let mut f = Fixture::<InterleavingMetadata>::with_metadata(model.clone(), false);
        f.control
            .submit(
                Delivery::Start,
                UserInput::Message(literal("root")),
                InterleavingMetadata(None),
            )
            .unwrap();
        model.wait_calls(1).await;
        let port = f.control.steering().unwrap();
        if full {
            for _ in 0..MAX_ADMISSIONS - 2 {
                port.submit(literal("previous"), InterleavingMetadata(None))
                    .unwrap();
            }
        }
        let producer = port.clone();
        let outcome = f.control.submit(
            Delivery::Steer(f.control.current_operation().unwrap()),
            UserInput::Message(literal("controller")),
            InterleavingMetadata(Some(Box::new(move || {
                producer
                    .submit(literal("producer"), InterleavingMetadata(None))
                    .unwrap();
            }))),
        );
        if full {
            assert!(
                outcome.is_err(),
                "racing producer must not overfill custody"
            );
            assert_eq!(lock(&f.control.custody).occupied, MAX_ADMISSIONS);
            f.control.request_stop();
        } else {
            outcome.unwrap();
            model.release();
            model.wait_calls(2).await;
            {
                let requests = model.requests.lock().unwrap();
                let users: Vec<_> = requests[1]
                    .messages
                    .iter()
                    .filter(|message| message.role == Role::User)
                    .flat_map(|message| message.content.iter())
                    .filter_map(|part| match part {
                        Content::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(users, ["root", "producer", "controller"]);
            }
            model.release();
            tokio::time::timeout(Duration::from_secs(5), async {
                while f.control.is_busy() {
                    drop(f.control.maintain().await);
                }
            })
            .await
            .unwrap();
        }
        f.control.shutdown().await;
    }
}

#[tokio::test]
async fn joined_inbox_reconciliation_preserves_consumed_guidance_after_acknowledgement_loss() {
    let model = Model::new(1);
    let mut f = Fixture::new(model.clone(), false);
    f.control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    let message = literal("committed, never replay");
    let receipt = f
        .control
        .steering()
        .unwrap()
        .submit(message.clone(), meta("guidance"))
        .unwrap();
    drop(f.control.maintain().await);
    model.release();
    model.wait_calls(2).await;
    assert!(
        f.control
            .session()
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry,
        ion_core::SessionEntry::Steering { input, .. } if input == &message))
    );
    // Inject a lost Host acknowledgement, not a failed Session commit: Core's
    // actual inbox has consumed persisted input while the original remains in
    // Host custody. Joined reconciliation must use that ownership evidence.
    {
        let mut custody = lock(&f.control.custody);
        let input = custody.inputs.get_mut(&receipt.id).unwrap();
        input.acceptance = None;
        input.original = Some(UserInput::Message(message));
        input
            .steering_status
            .as_ref()
            .unwrap()
            .publish(SteeringStatus::Queued);
        custody.steering_order.push_back(receipt.id);
        custody.updates.retain(
            |update| !matches!(update, ControlUpdate::Accepted { id, .. } if *id == receipt.id),
        );
    }
    model.release();
    let (returned, settled) = settle(&mut f.control).await;
    assert!(returned.is_empty());
    assert!(settled.accepted.iter().any(|input| input.id == receipt.id));
    assert!(matches!(receipt.status(), SteeringStatus::Committed { .. }));
    f.control.shutdown().await;
}

#[tokio::test]
async fn live_steering_receipts_follow_commit_return_and_operation_fences() {
    let model = Model::new(1);
    let mut f = Fixture::new(model.clone(), false);
    f.control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    let port = f.control.steering().unwrap();
    let text = literal("/literal \"guidance\" 🦀");
    let receipt = port.submit(text.clone(), meta("guidance")).unwrap();
    assert!(matches!(receipt.status(), SteeringStatus::Queued));
    // Owner waiting is cancellation-safe; the permit and input survive its loss.
    tokio::select! {
        biased;
        _ = std::future::ready(()) => {}
        _ = f.control.maintain() => panic!("abandoned wait was polled"),
    }
    drop(f.control.maintain().await);
    model.release();
    model.wait_calls(2).await;
    assert!(matches!(
        receipt.status(),
        SteeringStatus::Committed {
            acceptance: InputAcceptance { turn: 1, .. }
        }
    ));
    model.release();
    wait_committed_end(&f.control).await;
    assert!(
        f.control.is_busy(),
        "Host has not joined the Core operation yet"
    );
    assert!(port.submit(literal("too late"), meta("late")).is_err());
    settle(&mut f.control).await;
    f.control
        .submit(Delivery::Start, prompt("next"), meta("next"))
        .unwrap();
    model.wait_calls(3).await;
    assert!(port.submit(literal("stale grant"), meta("stale")).is_err());
    let current = f.control.steering().unwrap();
    let returned = current
        .submit(text.clone(), meta("return original"))
        .unwrap();
    f.control.request_stop();
    let report = f.control.shutdown().await;
    assert!(matches!(returned.status(),
        SteeringStatus::Returned { input: UserInput::Message(message), reason: ReturnSnapshot::Stopped } if message == text));
    assert!(report.updates.iter().any(|update| matches!(update,
        ControlUpdate::Returned(input) if input.id == returned.id)));
    assert!(current.submit(literal("closed"), meta("closed")).is_err());
}
