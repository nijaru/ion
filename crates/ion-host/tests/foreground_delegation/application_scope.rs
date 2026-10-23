use super::*;
use ion_host::BackgroundOwner;
use std::time::Duration;

#[tokio::test]
async fn idle_invalidation_waits_cover_reservation_release_and_closure() {
    let owner = BackgroundOwner::new(1).unwrap();
    let revision = owner.revision();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), owner.changed(revision))
            .await
            .is_err()
    );
    let permit = background::reserve(&owner).unwrap();
    let reserved = owner.changed(revision).await;
    assert_eq!(owner.summary().reserved, 1);
    drop(permit);
    let released = owner.changed(reserved).await;
    assert_eq!(owner.summary().reserved, 0);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), owner.changed(released))
            .await
            .is_err()
    );
    owner.join_shutdown().await.unwrap();
    assert_ne!(owner.changed(released).await, released);
}

#[tokio::test]
async fn cancelled_result_wait_retains_original_answer_until_acknowledgement() {
    let owner = Arc::new(BackgroundOwner::new(1).unwrap());
    let mut f = Fixture::with_owner(
        Wait::Join,
        true,
        false,
        ion_core::DelegationContext::Fresh,
        Some(owner.clone()),
        None,
    );
    f.start();
    background::drain(&mut f.control).await;
    let child = owner.jobs()[0].child;
    let selected = [child];
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            owner.wait(&selected, Duration::from_secs(5)),
        )
        .await
        .is_err()
    );
    assert!(owner.child(child).is_some());
    f.effect.release.notify_one();
    let mut report = owner.wait(&selected, Duration::from_secs(5)).await.unwrap();
    assert!(!report.wait_expired);
    let joined = report.children.pop().unwrap();
    assert!(joined.joined);
    assert!(
        matches!(&joined.receipt.outcome, Some(DelegationOutcome::Joined { answer: Some(answer), error: None, .. }) if answer == "two")
    );
    let revision = owner.revision();
    assert_eq!(owner.take(child).unwrap().unwrap().receipt, joined.receipt);
    assert_ne!(owner.changed(revision).await, revision);
    assert!(owner.wait(&selected, Duration::ZERO).await.is_err());
    let permit = background::reserve(&owner).unwrap();
    drop(permit);
    owner.join_shutdown().await.unwrap();
    f.control.shutdown().await;
    let root = f.root.clone();
    drop((f, owner));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn application_failure_stops_parent_without_a_host_result_reader() {
    let owner = Arc::new(BackgroundOwner::new(1).unwrap());
    let mut f = Fixture::with_owner(
        Wait::HoldAfterTransfer,
        true,
        false,
        ion_core::DelegationContext::Fresh,
        Some(owner.clone()),
        None,
    );
    f.control = SessionSetup::new(
        f.session.clone(),
        SessionCatalog::new(f.root.clone(), None),
        Arc::new(App {
            agent: f.control.agent().clone(),
            instructions: "application root",
            owner: Some(Arc::new(ion_host::ApplicationOwner::new(
                owner.clone(),
                Arc::new(ion_host::ProcessOwner::new(1).unwrap()),
            ))),
        }),
        None,
        None,
        None,
    )
    .unwrap()
    .into_control();
    let mut started = f.effect.started.subscribe();
    f.start();
    started.wait_for(|started| *started).await.unwrap();
    f.trigger("delegation_settled");
    f.effect.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !owner.snapshot().failed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // No Host next/maintain drives this propagation: the application's actual
    // cancellation scope stops the Core request independently of all readers.
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(
        f.parent.requests().len(),
        1,
        "a caught error must not permit another model request"
    );
    let report = f.control.shutdown().await;
    assert!(report.updates.iter().any(
        |update| matches!(update, ControlUpdate::Settled(settlement) if matches!(settlement.outcome, OperationOutcome::Turn(Err(_))))
    ));
    owner.join_shutdown().await.unwrap_err();
    let child = owner.jobs()[0].child;
    assert!(owner.take(child).unwrap().is_err());
    assert!(
        owner.check().is_err(),
        "acknowledging the error does not clear failure"
    );
    assert!(
        f.control
            .submit(
                ion_host::Delivery::Start,
                ion_host::UserInput::Message(Message::user_input("forbidden".into(), [])),
                ()
            )
            .is_err()
    );
    let root = f.root.clone();
    drop((f, owner));
    std::fs::remove_dir_all(root).unwrap();
}
