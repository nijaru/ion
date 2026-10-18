use super::*;
use ion_core::{BackgroundSlot, DelegationContext, ForkPoint, RequestKey};
use ion_host::BackgroundOwner;
use std::time::Duration;

pub(super) fn reserve(owner: &BackgroundOwner) -> Result<ion_host::BackgroundDestination, String> {
    let mut destination = owner.destination();
    destination.reserve()?;
    Ok(destination)
}

pub(super) async fn drain(control: &mut SessionControl) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while control.is_busy() {
            control.maintain().await;
        }
    })
    .await
    .unwrap();
    let mut settled = false;
    while let Some(update) = control.take_update() {
        if let ControlUpdate::Settled(settlement) = update {
            assert!(matches!(settlement.outcome, OperationOutcome::Turn(Ok(_))));
            settled = true;
        }
    }
    assert!(settled);
}

#[tokio::test]
async fn transfer_survives_parent_work_and_shutdown_and_retains_unread_results() {
    for context in [
        DelegationContext::Fresh,
        DelegationContext::Fork {
            point: ForkPoint::AfterTurn(1),
        },
    ] {
        let owner = Arc::new(BackgroundOwner::new(1).unwrap());
        let mut f = Fixture::with_owner(Wait::Repeat, true, false, context, Some(owner.clone()));
        if matches!(context, DelegationContext::Fork { .. }) {
            f.control
                .submit_request(
                    &RequestKey::new("source".into(), "history".into()).unwrap(),
                    Message::user_input("historical input marker".into(), []),
                    (),
                )
                .unwrap();
            drain(&mut f.control).await;
        }
        f.start();
        drain(&mut f.control).await;
        let mut started = f.effect.started.subscribe();
        started.wait_for(|started| *started).await.unwrap();
        let links = f.session.view().unwrap().delegations();
        assert_eq!(links.len(), 1);
        assert!(links[0].transferred);
        assert!(links[0].outcome.is_none());
        let child = links[0].record.child;
        assert_eq!(
            owner.snapshot().reserved,
            0,
            "exact/conflicting retries release reservations"
        );
        assert!(owner.take(child).is_none());
        f.control
            .submit_request(
                &RequestKey::new("later".into(), "parent".into()).unwrap(),
                Message::user_input("later work".into(), []),
                (),
            )
            .unwrap();
        drain(&mut f.control).await;
        assert!(
            f.session.view().unwrap().delegations()[0].outcome.is_none(),
            "live background is not orphaned by new parent work"
        );
        f.control.request_stop();
        let report = f.control.shutdown().await;
        assert!(report.cleanup_errors.is_empty());
        assert!(
            !*f.effect.stopped.borrow(),
            "parent stop/shutdown must not cross committed transfer"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), owner.maintain())
                .await
                .is_err()
        );
        assert!(
            reserve(&owner).is_err(),
            "running job consumes the single slot"
        );
        f.effect.release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !owner.snapshot().children[0].joined {
                owner.maintain().await;
            }
        })
        .await
        .unwrap();
        assert!(
            reserve(&owner).is_err(),
            "unread joined result still consumes capacity"
        );
        let receipt = owner.take(child).unwrap().unwrap();
        assert!(
            matches!(receipt.outcome, Some(DelegationOutcome::Joined { receipt: Some(ref receipt), error: None, answer: Some(_) }) if receipt.end == Some(TurnEndReason::Completed))
        );
        let reused = reserve(&owner).unwrap();
        assert!(
            reserve(&owner).is_err(),
            "cold reservations also occupy capacity"
        );
        drop(reused);
        assert!(owner.take(child).is_none());
        assert!(f.session.view().unwrap().delegations()[0].outcome.is_some());
        assert_eq!(f.effect.calls.load(Ordering::SeqCst), 1);
        owner.join_shutdown().await.unwrap();
        let root = f.root.clone();
        drop(f);
        drop(owner);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn owner_shutdown_is_joined_even_when_its_wait_is_cancelled() {
    let owner = Arc::new(BackgroundOwner::new(1).unwrap());
    let mut f = Fixture::with_owner(
        Wait::Join,
        true,
        false,
        DelegationContext::Fresh,
        Some(owner.clone()),
    );
    f.start();
    drain(&mut f.control).await;
    let mut started = f.effect.started.subscribe();
    started.wait_for(|started| *started).await.unwrap();
    let child = f.session.view().unwrap().delegations()[0].record.child;
    let mut stopped = f.effect.stopped.subscribe();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), owner.join_shutdown())
            .await
            .is_err()
    );
    stopped.wait_for(|stopped| *stopped).await.unwrap();
    assert!(reserve(&owner).is_err());
    assert!(!owner.snapshot().children[0].joined);
    assert!(owner.take(child).is_none());
    assert!(f.session.view().unwrap().delegations()[0].outcome.is_none());
    f.effect.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), owner.join_shutdown())
        .await
        .unwrap()
        .unwrap();
    let receipt = owner.take(child).unwrap().unwrap();
    assert!(
        matches!(receipt.outcome, Some(DelegationOutcome::Joined { receipt: Some(ref receipt), .. }) if receipt.end == Some(TurnEndReason::Cancelled))
    );
    assert!(
        !owner.snapshot().failed,
        "ordinary joined cancellation is not infrastructure failure"
    );
    let root = f.root.clone();
    drop(f);
    drop(owner);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn transfer_failure_cannot_be_caught_to_continue_and_creates_no_child() {
    let owner = Arc::new(BackgroundOwner::new(1).unwrap());
    let mut f = Fixture::with_owner(
        Wait::Join,
        false,
        false,
        DelegationContext::Fresh,
        Some(owner.clone()),
    );
    f.trigger("delegation_transferred");
    f.start();
    let report = f.settle().await;
    assert_eq!(f.effect.calls.load(Ordering::SeqCst), 0);
    assert!(!f.root.join("children").exists());
    assert!(owner.snapshot().children.is_empty());
    assert_eq!(owner.snapshot().reserved, 0);
    assert_eq!(f.parent.requests().len(), 1);
    assert!(report.updates.iter().any(|update| matches!(update, ControlUpdate::Settled(settlement)
        if matches!(settlement.outcome, OperationOutcome::Turn(Err(CodingAgentError::Session(_)))))));
    owner.join_shutdown().await.unwrap();
    let root = f.root.clone();
    drop(f);
    drop(owner);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn late_settlement_failure_closes_owner_without_any_result_reader() {
    let owner = Arc::new(BackgroundOwner::new(2).unwrap());
    let mut f = Fixture::with_owner(
        Wait::Join,
        true,
        false,
        DelegationContext::Fresh,
        Some(owner.clone()),
    );
    f.start();
    drain(&mut f.control).await;
    let mut started = f.effect.started.subscribe();
    started.wait_for(|started| *started).await.unwrap();
    f.trigger("delegation_settled");
    let child = f.session.view().unwrap().delegations()[0].record.child;
    f.effect.release.notify_one();
    // Do not poll maintain, join or any result. The lifetime failure guard closes
    // admission independently of those readers, after the actual failed commit.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !owner.snapshot().failed {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(reserve(&owner).is_err());
    assert!(f.session.view().unwrap().delegations()[0].outcome.is_none());
    owner.join_shutdown().await.unwrap_err();
    assert!(owner.take(child).unwrap().is_err());
    assert!(owner.snapshot().failed);
    assert!(
        reserve(&owner).is_err(),
        "consuming the error does not reopen the owner"
    );
    let root = f.root.clone();
    drop(f);
    drop(owner);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn panic_closes_owner_and_stops_siblings_without_a_result_reader() {
    let owner = Arc::new(BackgroundOwner::new(2).unwrap());
    let mut sibling = Fixture::with_owner(
        Wait::Join,
        true,
        false,
        DelegationContext::Fresh,
        Some(owner.clone()),
    );
    sibling.start();
    drain(&mut sibling.control).await;
    let mut started = sibling.effect.started.subscribe();
    started.wait_for(|started| *started).await.unwrap();
    let mut stopped = sibling.effect.stopped.subscribe();
    let mut panics = Fixture::with_owner(
        Wait::Panic,
        false,
        false,
        DelegationContext::Fresh,
        Some(owner.clone()),
    );
    panics.start();
    drain(&mut panics.control).await;
    tokio::time::timeout(Duration::from_secs(5), stopped.wait_for(|stopped| *stopped))
        .await
        .unwrap()
        .unwrap();
    assert!(owner.snapshot().failed);
    assert!(reserve(&owner).is_err());
    assert!(
        panics.session.view().unwrap().delegations()[0]
            .outcome
            .is_none()
    );
    sibling.effect.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), owner.join_shutdown())
        .await
        .unwrap()
        .unwrap_err();
    let snapshot = owner.snapshot();
    assert!(snapshot.children.iter().all(|child| child.joined));
    assert_eq!(
        snapshot
            .children
            .iter()
            .filter(|child| child.error.is_some())
            .count(),
        1
    );
    for child in snapshot.children {
        let _result = owner.take(child.receipt.record.child).unwrap();
    }
    assert!(owner.snapshot().failed);
    let roots = [sibling.root.clone(), panics.root.clone()];
    drop((sibling, panics, owner));
    for root in roots {
        std::fs::remove_dir_all(root).unwrap();
    }
}

pub(super) struct BeforeTransfer {
    pub(super) permit: ion_host::BackgroundDestination,
    pub(super) stop: Option<tokio_util::sync::CancellationToken>,
    pub(super) close: Option<Arc<BackgroundOwner>>,
}
impl ion_core::BackgroundSlot for BeforeTransfer {
    fn owner(&self) -> uuid::Uuid {
        self.permit.owner()
    }
    fn reserve(&mut self) -> Result<(), String> {
        self.permit.reserve()
    }
    fn install(
        self: Box<Self>,
        job: ion_core::BackgroundJob,
    ) -> Result<ion_core::DelegationReceipt, ion_core::BackgroundStartError> {
        if let Some(stop) = self.stop {
            stop.cancel();
        }
        if let Some(owner) = self.close {
            owner.request_shutdown();
        }
        Box::new(self.permit).install(job)
    }
}

#[tokio::test]
async fn cancellation_and_owner_close_before_transfer_refuse_cold_work() {
    for wait in [Wait::StopBeforeTransfer, Wait::CloseBeforeTransfer] {
        let owner = Arc::new(BackgroundOwner::new(1).unwrap());
        let mut f = Fixture::with_owner(
            wait,
            false,
            false,
            DelegationContext::Fresh,
            Some(owner.clone()),
        );
        f.start();
        let report = f.settle().await;
        assert!(report.cleanup_errors.is_empty());
        let links = f.session.view().unwrap().delegations();
        assert_eq!(links.len(), 1, "the barrier runs after committed intent");
        assert!(!links[0].transferred);
        assert!(matches!(
            links[0].outcome,
            Some(DelegationOutcome::Joined { receipt: None, .. })
        ));
        assert!(!f.root.join("children").exists());
        assert_eq!(f.effect.calls.load(Ordering::SeqCst), 0);
        assert_eq!(owner.snapshot().reserved, 0);
        assert!(owner.snapshot().children.is_empty());
        let expected = if matches!(wait, Wait::StopBeforeTransfer) {
            TurnEndReason::Cancelled
        } else {
            TurnEndReason::Completed
        };
        assert_eq!(f.session.view().unwrap().last_end, Some((1, expected)));
        owner.join_shutdown().await.unwrap();
        let root = f.root.clone();
        drop((f, owner));
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn cancelling_the_active_parent_after_transfer_does_not_cancel_the_child() {
    let owner = Arc::new(BackgroundOwner::new(1).unwrap());
    let mut f = Fixture::with_owner(
        Wait::HoldAfterTransfer,
        true,
        false,
        DelegationContext::Fresh,
        Some(owner.clone()),
    );
    let mut started = f.effect.started.subscribe();
    f.start();
    started.wait_for(|started| *started).await.unwrap();
    assert!(f.session.view().unwrap().delegations()[0].transferred);
    assert!(f.control.is_busy());
    f.control.request_stop();
    tokio::time::timeout(Duration::from_secs(5), f.control.join_shutdown())
        .await
        .unwrap();
    assert_eq!(
        f.session.view().unwrap().last_end,
        Some((1, TurnEndReason::Cancelled))
    );
    assert!(f.session.view().unwrap().delegations()[0].outcome.is_none());
    f.effect.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !owner.snapshot().children[0].joined {
            owner.maintain().await;
        }
    })
    .await
    .unwrap();
    let child = owner.snapshot().children[0].receipt.record.child;
    let receipt = owner.take(child).unwrap().unwrap();
    assert!(
        matches!(receipt.outcome, Some(DelegationOutcome::Joined { receipt: Some(ref receipt), error: None, answer: Some(_) }) if receipt.end == Some(TurnEndReason::Completed))
    );
    owner.join_shutdown().await.unwrap();
    let root = f.root.clone();
    drop((f, owner));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn background_waiters_keep_completion_when_the_last_waiter_is_cancelled() {
    for cancel_last in [false, true] {
        let owner = Arc::new(BackgroundOwner::new(1).unwrap());
        let mut f = Fixture::with_owner(
            Wait::Join,
            true,
            false,
            DelegationContext::Fresh,
            Some(owner.clone()),
        );
        f.start();
        drain(&mut f.control).await;
        let mut started = f.effect.started.subscribe();
        started.wait_for(|started| *started).await.unwrap();
        let child = f.session.view().unwrap().delegations()[0].record.child;
        let (finished, mut completions) = watch::channel(0);
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let owner = owner.clone();
            let finished = finished.clone();
            let (send, receive) = tokio::sync::oneshot::channel();
            tasks.push(tokio::spawn(async move {
                let wait = owner.wait(child);
                tokio::pin!(wait);
                let mut send = Some(send);
                let joined = std::future::poll_fn(|cx| {
                    let result = std::future::Future::poll(wait.as_mut(), cx);
                    if result.is_pending()
                        && let Some(send) = send.take()
                    {
                        let _ = send.send(());
                    }
                    result
                })
                .await
                .unwrap();
                assert!(joined.joined);
                assert!(matches!(
                    joined.receipt.outcome,
                    Some(DelegationOutcome::Joined { answer: Some(ref answer), .. })
                        if answer == "two"
                ));
                finished.send_modify(|count| *count += 1);
            }));
            // Establish polling order without sleeps, before releasing the child.
            receive.await.unwrap();
        }
        if cancel_last {
            let last = tasks.pop().unwrap();
            last.abort();
            assert!(last.await.unwrap_err().is_cancelled());
        }
        let expected = tasks.len();
        f.effect.release.notify_one();
        let joined = tokio::time::timeout(
            Duration::from_secs(5),
            completions.wait_for(|count| *count == expected),
        )
        .await
        .is_ok();
        // Recover custody even on regression failure before removing the fixture.
        owner.join_shutdown().await.unwrap();
        for task in tasks {
            task.await.unwrap();
        }
        let receipt = owner.take(child).unwrap().unwrap();
        assert!(matches!(
            receipt.outcome,
            Some(DelegationOutcome::Joined { answer: Some(ref answer), .. })
                if answer == "two"
        ));
        let root = f.root.clone();
        drop((f, owner));
        std::fs::remove_dir_all(root).unwrap();
        assert!(
            joined,
            "all surviving waiters must observe the original join; cancel_last={cancel_last}"
        );
    }
}
