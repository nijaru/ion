use super::*;
use futures_util::FutureExt;
use ion_core::{BackgroundSlot, ForkPoint, RequestKey};
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
    for context in [None, Some(ForkPoint::AfterTurn(1))] {
        let owner = Arc::new(BackgroundOwner::new(1).unwrap());
        let mut f = Fixture::with_owner(
            Wait::Repeat,
            true,
            false,
            context,
            Some(owner.clone()),
            None,
        );
        if context.is_some() {
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
        let child = links[0].record.run;
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
            while !owner.snapshot().runs[0].joined {
                owner.maintain().await;
            }
        })
        .await
        .unwrap();
        assert!(
            reserve(&owner).is_err(),
            "unread joined result still consumes capacity"
        );
        let receipt = owner.take(child).unwrap().unwrap().receipt;
        assert!(
            matches!(receipt.outcome, Some(DelegationOutcome::Joined { execution_timed_out: false, receipt: Some(ref receipt), error: None, answer: Some(_) }) if receipt.end == Some(TurnEndReason::Completed))
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
    let mut f = Fixture::with_owner(Wait::Join, true, false, None, Some(owner.clone()), None);
    f.start();
    drain(&mut f.control).await;
    let mut started = f.effect.started.subscribe();
    started.wait_for(|started| *started).await.unwrap();
    let child = f.session.view().unwrap().delegations()[0].record.run;
    let mut stopped = f.effect.stopped.subscribe();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), owner.join_shutdown())
            .await
            .is_err()
    );
    stopped.wait_for(|stopped| *stopped).await.unwrap();
    assert!(reserve(&owner).is_err());
    assert!(!owner.snapshot().runs[0].joined);
    assert!(owner.take(child).is_none());
    assert!(f.session.view().unwrap().delegations()[0].outcome.is_none());
    f.effect.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), owner.join_shutdown())
        .await
        .unwrap()
        .unwrap();
    let receipt = owner.take(child).unwrap().unwrap().receipt;
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
    let mut f = Fixture::with_owner(Wait::Join, false, false, None, Some(owner.clone()), None);
    f.trigger("delegation_transferred");
    f.start();
    let report = f.settle().await;
    assert_eq!(f.effect.calls.load(Ordering::SeqCst), 0);
    assert!(!f.root.join("children").exists());
    assert!(owner.snapshot().runs.is_empty());
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
    let mut f = Fixture::with_owner(Wait::Join, true, false, None, Some(owner.clone()), None);
    f.start();
    drain(&mut f.control).await;
    let mut started = f.effect.started.subscribe();
    started.wait_for(|started| *started).await.unwrap();
    f.trigger("delegation_settled");
    let child = f.session.view().unwrap().delegations()[0].record.run;
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
    let mut sibling = Fixture::with_owner(Wait::Join, true, false, None, Some(owner.clone()), None);
    sibling.start();
    drain(&mut sibling.control).await;
    let mut started = sibling.effect.started.subscribe();
    started.wait_for(|started| *started).await.unwrap();
    let mut stopped = sibling.effect.stopped.subscribe();
    let mut panics =
        Fixture::with_owner(Wait::Panic, false, false, None, Some(owner.clone()), None);
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
    assert!(snapshot.runs.iter().all(|child| child.joined));
    assert_eq!(
        snapshot
            .runs
            .iter()
            .filter(|child| child.error.is_some())
            .count(),
        1
    );
    for child in snapshot.runs {
        let _result = owner.take(child.receipt.record.run).unwrap();
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
        let mut f = Fixture::with_owner(wait, false, false, None, Some(owner.clone()), None);
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
        assert!(owner.snapshot().runs.is_empty());
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
        None,
        Some(owner.clone()),
        None,
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
        while !owner.snapshot().runs[0].joined {
            owner.maintain().await;
        }
    })
    .await
    .unwrap();
    let child = owner.snapshot().runs[0].receipt.record.run;
    let receipt = owner.take(child).unwrap().unwrap().receipt;
    assert!(
        matches!(receipt.outcome, Some(DelegationOutcome::Joined { execution_timed_out: false, receipt: Some(ref receipt), error: None, answer: Some(_) }) if receipt.end == Some(TurnEndReason::Completed))
    );
    owner.join_shutdown().await.unwrap();
    let root = f.root.clone();
    drop((f, owner));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn wait_any_expires_without_stop_and_returns_a_joined_cancellation_before_running_work() {
    let owner = Arc::new(BackgroundOwner::new(3).unwrap());
    let mut fixtures: [Fixture; 2] = std::array::from_fn(|_| {
        Fixture::with_owner(Wait::Join, true, false, None, Some(owner.clone()), None)
    });
    for fixture in &mut fixtures {
        fixture.start();
        drain(&mut fixture.control).await;
        let mut started = fixture.effect.started.subscribe();
        started.wait_for(|started| *started).await.unwrap();
    }
    let selected = fixtures
        .each_ref()
        .map(|fixture| fixture.session.view().unwrap().delegations()[0].record.run);
    tokio::time::pause();
    let mut wait = Box::pin(owner.wait(&selected, Duration::from_millis(100)));
    assert!(wait.as_mut().now_or_never().is_none());
    tokio::time::sleep(Duration::from_millis(40)).await;
    let permit = reserve(&owner).unwrap();
    assert!(wait.as_mut().now_or_never().is_none());
    tokio::time::sleep(Duration::from_millis(40)).await;
    drop(permit);
    assert!(wait.as_mut().now_or_never().is_none());
    let report = tokio::time::timeout(Duration::from_millis(30), wait.as_mut()).await;
    tokio::time::resume();
    let report = report
        .expect("unrelated custody changes must not reset the budget")
        .unwrap();
    assert!(report.wait_expired);
    assert!(report.runs.iter().all(|child| !child.joined));
    assert!(
        fixtures
            .iter()
            .all(|fixture| !*fixture.effect.stopped.borrow())
    );

    let mut stopped = fixtures[1].effect.stopped.subscribe();
    owner.request_stop(selected[1]).unwrap();
    tokio::time::timeout(Duration::from_secs(5), stopped.wait_for(|stopped| *stopped))
        .await
        .unwrap()
        .unwrap();
    fixtures[1].effect.release.notify_one();
    let report = owner.wait(&selected, Duration::from_secs(5)).await.unwrap();
    assert!(!report.wait_expired);
    assert_eq!(report.runs[0].receipt.record.run, selected[0]);
    assert!(!report.runs[0].joined);
    let cancelled = &report.runs[1];
    assert_eq!(cancelled.receipt.record.run, selected[1]);
    assert!(cancelled.joined);
    assert!(matches!(
        &cancelled.receipt.outcome,
        Some(DelegationOutcome::Joined { receipt: Some(receipt), answer: None, .. })
            if receipt.end == Some(TurnEndReason::Cancelled)
    ));
    assert!(!*fixtures[0].effect.stopped.borrow());
    for invalid in [
        vec![],
        vec![selected[1], selected[1]],
        vec![selected[1], uuid::Uuid::nil()],
    ] {
        assert!(owner.wait(&invalid, Duration::ZERO).await.is_err());
    }
    let report = owner
        .wait(&[selected[1], selected[0]], Duration::ZERO)
        .await
        .unwrap();
    assert!(!report.wait_expired);
    assert_eq!(report.runs[0].receipt, cancelled.receipt);
    assert_eq!(
        owner.take(selected[1]).unwrap().unwrap().receipt,
        cancelled.receipt
    );
    assert!(owner.wait(&selected, Duration::ZERO).await.is_err());

    fixtures[0].effect.release.notify_one();
    let report = owner
        .wait(&[selected[0]], Duration::from_secs(5))
        .await
        .unwrap();
    assert!(!report.wait_expired);
    assert!(matches!(
        &report.runs[0].receipt.outcome,
        Some(DelegationOutcome::Joined { answer: Some(answer), .. }) if answer == "two"
    ));
    owner.take(selected[0]).unwrap().unwrap();
    owner.join_shutdown().await.unwrap();
    for mut fixture in fixtures {
        fixture.control.shutdown().await;
        let root = fixture.root.clone();
        drop(fixture);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn background_waiters_keep_completion_when_the_last_waiter_is_cancelled() {
    for cancel_last in [false, true] {
        let owner = Arc::new(BackgroundOwner::new(1).unwrap());
        let mut f = Fixture::with_owner(Wait::Join, true, false, None, Some(owner.clone()), None);
        f.start();
        drain(&mut f.control).await;
        let mut started = f.effect.started.subscribe();
        started.wait_for(|started| *started).await.unwrap();
        let child = f.session.view().unwrap().delegations()[0].record.run;
        let (finished, mut completions) = watch::channel(0);
        let mut tasks = Vec::new();
        for _ in 0..2 {
            let owner = owner.clone();
            let finished = finished.clone();
            let (send, receive) = tokio::sync::oneshot::channel();
            tasks.push(tokio::spawn(async move {
                let selected = [child];
                let wait = owner.wait(&selected, Duration::from_secs(5));
                tokio::pin!(wait);
                let mut send = Some(send);
                let mut report = std::future::poll_fn(|cx| {
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
                assert!(!report.wait_expired);
                let joined = report.runs.pop().unwrap();
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
        let receipt = owner.take(child).unwrap().unwrap().receipt;
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
