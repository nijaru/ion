//! The join budget changes custody, not execution identity or cleanup obligations.
use super::*;
use ion_host::BackgroundOwner;
use std::time::Duration;

async fn started(f: &Fixture) {
    let mut started = f.effect.started.subscribe();
    tokio::time::timeout(Duration::from_secs(5), started.wait_for(|value| *value))
        .await
        .unwrap()
        .unwrap();
}
async fn stopped(f: &Fixture) {
    let mut stopped = f.effect.stopped.subscribe();
    tokio::time::timeout(Duration::from_secs(5), stopped.wait_for(|value| *value))
        .await
        .unwrap()
        .unwrap();
}
fn remove(f: Fixture, owner: Arc<BackgroundOwner>) {
    let root = f.root.clone();
    drop((f, owner));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test(start_paused = true)]
async fn completion_releases_reservation_without_transfer_and_overflow_never_admits() {
    for budget in [Duration::from_secs(60), Duration::MAX] {
        let owner = Arc::new(BackgroundOwner::new(1).unwrap());
        let mut f = Fixture::with_owner(
            Wait::Bounded(budget),
            false,
            false,
            None,
            Some(owner.clone()),
            None,
        );
        f.start();
        let report = f.settle().await;
        assert!(report.cleanup_errors.is_empty());
        let links = f.session.view().unwrap().delegations();
        if budget == Duration::MAX {
            assert!(
                links.is_empty(),
                "invalid clock budget refuses before intent"
            );
            assert_eq!(f.child.requests().len(), 0);
        } else {
            assert_eq!(links.len(), 1);
            assert!(!links[0].transferred);
            assert!(matches!(&links[0].outcome, Some(DelegationOutcome::Joined {
                execution_timed_out: false, error: None, answer: Some(answer), ..
            }) if answer == "two"));
            assert_eq!(f.effect.calls.load(Ordering::SeqCst), 1);
        }
        assert!(owner.snapshot().runs.is_empty());
        assert_eq!(owner.snapshot().reserved, 0);
        drop(background::reserve(&owner).unwrap());
        owner.join_shutdown().await.unwrap();
        remove(f, owner);
    }
}

#[tokio::test(start_paused = true)]
async fn expiry_moves_same_live_run_and_grant_without_refreshing_execution_deadline() {
    let owner = Arc::new(BackgroundOwner::new(1).unwrap());
    let mut f = Fixture::with_owner(
        Wait::Bounded(Duration::from_secs(2)),
        true,
        false,
        None,
        Some(owner.clone()),
        Some(10_000),
    );
    f.start();
    started(&f).await;
    let before = f.session.view().unwrap().delegations()[0].clone();
    assert!(!before.transferred);
    assert_eq!(owner.snapshot().reserved, 1);
    assert!(background::reserve(&owner).is_err());
    let steering = f.delegate.steering.lock().unwrap().clone().unwrap();
    let request = ion_core::RequestKey::new("join".into(), "guidance".into()).unwrap();
    let input = Message::user_input("literal pending guidance".into(), []);
    let queued = steering
        .steer(
            &request,
            &input,
            &tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(queued.status, ion_host::SteeringStatus::Queued));
    tokio::time::advance(Duration::from_secs(2)).await;
    background::drain(&mut f.control).await;
    let transferred = f.session.view().unwrap().delegations()[0].clone();
    assert!(transferred.transferred);
    assert_eq!(
        before.record, transferred.record,
        "custody, not run identity, changed"
    );
    let id = transferred.record.run;
    assert_eq!(owner.snapshot().reserved, 0);
    assert_eq!(owner.run(id).unwrap().steering[0].id, queued.id);
    assert_eq!(
        owner
            .steer(
                id,
                &request,
                &input,
                &tokio_util::sync::CancellationToken::new()
            )
            .await
            .unwrap()
            .id,
        queued.id
    );
    f.control.request_stop();
    assert!(f.control.shutdown().await.cleanup_errors.is_empty());
    assert!(
        !*f.effect.stopped.borrow(),
        "parent stop after commit cannot cross custody"
    );
    tokio::time::advance(Duration::from_secs(7)).await;
    assert!(!*f.effect.stopped.borrow());
    tokio::time::advance(Duration::from_secs(1)).await;
    stopped(&f).await;
    assert!(
        !owner.run(id).unwrap().joined,
        "actual native/tool cleanup still blocks join"
    );
    assert!(owner.take(id).is_none());
    f.effect.release.notify_one();
    owner.drain().await.unwrap();
    let result = owner.take(id).unwrap().unwrap();
    assert!(
        matches!(&result.receipt.outcome, Some(DelegationOutcome::Joined {
        execution_timed_out: true, receipt: Some(receipt), error: Some(_), answer: None, ..
    }) if receipt.end == Some(TurnEndReason::Cancelled))
    );
    assert!(
        matches!(&result.steering[0].status, ion_host::SteeringStatus::Returned { input: ion_host::UserInput::Message(returned), .. } if *returned == input)
    );
    assert_eq!(f.effect.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.child.requests().len(), 1);
    assert_eq!(
        CodingSession::open(&transferred.record.path).unwrap().id(),
        transferred.record.child
    );
    owner.join_shutdown().await.unwrap();
    remove(f, owner);
}

#[tokio::test(start_paused = true)]
async fn abandoned_bounded_wait_keeps_foreground_joining_without_stopping_or_transferring() {
    let owner = Arc::new(BackgroundOwner::new(1).unwrap());
    let mut f = Fixture::with_owner(
        Wait::AbandonBounded(Duration::from_secs(2)),
        true,
        false,
        None,
        Some(owner.clone()),
        None,
    );
    f.start();
    started(&f).await;
    tokio::time::advance(Duration::from_secs(62)).await;
    // Let closure and the old deadline both become observable by the gateway.
    tokio::task::yield_now().await;
    assert!(f.control.is_busy());
    assert_eq!(f.parent.requests().len(), 1);
    assert!(
        !*f.effect.stopped.borrow(),
        "dropping a waiter does not cancel accepted work"
    );
    assert!(!f.session.view().unwrap().delegations()[0].transferred);
    assert!(owner.snapshot().runs.is_empty());
    assert_eq!(owner.snapshot().reserved, 0);
    f.effect.release.notify_one();
    assert!(f.settle().await.cleanup_errors.is_empty());
    let result = f.session.view().unwrap().delegations()[0].clone();
    assert!(!result.transferred);
    assert!(matches!(
        result.outcome,
        Some(DelegationOutcome::Joined {
            answer: Some(_),
            error: None,
            ..
        })
    ));
    assert_eq!(f.effect.calls.load(Ordering::SeqCst), 1);
    owner.join_shutdown().await.unwrap();
    remove(f, owner);
}

#[tokio::test]
async fn stop_and_failed_transfers_join_started_work_before_parent_release() {
    // Parent stop before expiry, cancellation/owner-close at transfer, and an
    // actual durable transfer write fault all retain the already-started callback.
    for (mode, stop_before, storage_fault) in [
        (Wait::Bounded(Duration::from_secs(2)), true, false),
        (Wait::StopAtHandoff, false, false),
        (Wait::CloseAtHandoff, false, false),
        (Wait::Bounded(Duration::from_secs(2)), false, true),
    ] {
        let owner = Arc::new(BackgroundOwner::new(1).unwrap());
        let mut f = Fixture::with_owner(mode, true, storage_fault, None, Some(owner.clone()), None);
        f.start();
        started(&f).await;
        tokio::time::pause();
        if stop_before {
            f.control.request_stop();
        }
        if storage_fault {
            f.trigger("delegation_transferred");
        }
        tokio::time::advance(Duration::from_secs(2)).await;
        stopped(&f).await;
        assert!(
            f.control.is_busy(),
            "parent cannot leave while cleanup is held"
        );
        assert_eq!(f.parent.requests().len(), 1);
        assert_eq!(f.effect.calls.load(Ordering::SeqCst), 1);
        assert!(!f.session.view().unwrap().delegations()[0].transferred);
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(
            f.control.is_busy(),
            "expiry cannot override stop or failed transfer"
        );
        tokio::time::resume();
        f.effect.release.notify_one();
        let report = f.settle().await;
        let receipt = f.session.view().unwrap().delegations()[0].clone();
        assert!(!receipt.transferred);
        assert!(matches!(
            receipt.outcome,
            Some(DelegationOutcome::Joined {
                execution_timed_out: false,
                receipt: Some(_),
                answer: None,
                ..
            })
        ));
        assert!(owner.snapshot().runs.is_empty());
        assert_eq!(owner.snapshot().reserved, 0);
        assert!(report.cleanup_errors.is_empty());
        if storage_fault {
            assert!(report.updates.iter().any(|update| matches!(update,
                ControlUpdate::Settled(settlement) if matches!(settlement.outcome,
                    OperationOutcome::Turn(Err(CodingAgentError::Session(ion_core::CodingSessionError::Sqlite(_)))))
            )), "caught transfer fault must retain its typed Session cause");
            assert_eq!(f.session.view().unwrap().last_end, None);
            assert_eq!(
                f.effect.calls.load(Ordering::SeqCst),
                1,
                "Code Mode cannot continue a dependent effect"
            );
        }
        owner.join_shutdown().await.unwrap();
        remove(f, owner);
    }
}
