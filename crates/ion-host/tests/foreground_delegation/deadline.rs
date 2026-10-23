use super::*;
use ion_host::BackgroundOwner;
use std::time::Duration;

#[tokio::test]
async fn deadline_stops_but_joins_and_retains_provenance_after_infrastructure_failure() {
    for (background, fault) in [(false, false), (true, false), (true, true)] {
        let owner = Arc::new(BackgroundOwner::new(1).unwrap());
        let mut f = Fixture::with_owner(
            if background {
                Wait::Join
            } else {
                Wait::Abandon
            },
            true,
            false,
            ion_core::DelegationContext::Fresh,
            background.then(|| owner.clone()),
            Some(60_000),
        );
        let mut started = f.effect.started.subscribe();
        let mut stopped = f.effect.stopped.subscribe();
        f.start();
        tokio::time::timeout(Duration::from_secs(5), started.wait_for(|started| *started))
            .await
            .unwrap()
            .unwrap();
        if background {
            background::drain(&mut f.control).await;
        }
        let record = f.session.view().unwrap().delegations()[0].record.clone();
        assert_eq!(record.spec.execution_timeout_ms, Some(60_000));
        if fault {
            rusqlite::Connection::open(&record.path).unwrap().execute_batch(
                "CREATE TRIGGER fail_end BEFORE INSERT ON entries WHEN json_extract(NEW.body,'$.kind') = 'turn_ended' BEGIN SELECT RAISE(FAIL, 'injected child settlement failure'); END;"
            ).unwrap();
        }

        // No borrowed result waiter drives this deadline. Expiry must retain
        // custody while the real effect deliberately holds its stopped cleanup.
        tokio::time::pause();
        tokio::time::sleep(Duration::from_secs(120)).await;
        tokio::time::timeout(Duration::from_secs(1), stopped.wait_for(|stopped| *stopped))
            .await
            .unwrap()
            .unwrap();
        assert!(f.session.view().unwrap().delegations()[0].outcome.is_none());
        if background {
            assert!(!owner.child(record.child).unwrap().joined);
            assert!(owner.take(record.child).is_none());
        } else {
            assert!(f.control.is_busy());
        }
        tokio::time::resume();

        f.effect.release.notify_one();
        if background {
            let joined = owner.wait(&[record.child], Duration::from_secs(5)).await;
            assert_eq!(joined.is_err(), fault);
            assert_eq!(owner.check().is_err(), fault);
        } else {
            assert!(f.settle().await.cleanup_errors.is_empty());
        }
        // Infrastructure invalidation may precede operation joining. Even an
        // error from wait is not permission to abandon the original owner.
        assert_eq!(owner.join_shutdown().await.is_err(), fault);
        if background {
            assert!(owner.child(record.child).unwrap().joined);
        }
        let outcome = f.session.view().unwrap().delegations()[0].outcome.clone();
        let Some(DelegationOutcome::Joined {
            execution_timed_out: true,
            receipt,
            error: Some(_),
            answer: None,
        }) = &outcome
        else {
            panic!("timeout provenance must survive full joining and cleanup errors: {outcome:?}");
        };
        if !fault {
            assert_eq!(
                receipt.as_ref().unwrap().end,
                Some(TurnEndReason::Cancelled)
            );
        }
        assert_eq!(
            CodingSession::inspect(f.session.path())
                .unwrap()
                .delegations()[0]
                .outcome,
            outcome,
        );
        assert_eq!(f.effect.calls.load(Ordering::SeqCst), 1);
        assert_eq!(f.child.requests().len(), 1);
        f.control.shutdown().await;
        let root = f.root.clone();
        drop((f, owner));
        std::fs::remove_dir_all(root).unwrap();
    }
}

struct ProgressModel(watch::Sender<usize>);
impl ion_ai::ModelService for ProgressModel {
    fn stream<'a>(
        &'a self,
        _: ion_ai::ModelRequest,
    ) -> BoxFuture<'a, Result<ion_ai::ModelStream, ion_ai::ProviderError>> {
        let progress = self.0.clone();
        Box::pin(async move {
            let stream: ion_ai::ModelStream = Box::pin(async_stream::stream! {
                for count in 1.. {
                    yield Ok(ModelStreamEvent::TextDelta("still working".into()));
                    progress.send_replace(count);
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            });
            Ok(stream)
        })
    }
}

#[tokio::test]
async fn model_progress_does_not_refresh_the_execution_deadline() {
    let (progress, mut observed) = watch::channel(0);
    let mut f = Fixture::with_child_service(
        Wait::Join,
        false,
        false,
        ion_core::DelegationContext::Fresh,
        None,
        Some(60_000),
        Some(Arc::new(ProgressModel(progress))),
    );
    tokio::time::pause();
    f.start();
    // Five real provider deltas reach the child's ordinary control path. A
    // budget restarted on progress would never expire while this stream runs.
    observed.wait_for(|count| *count >= 5).await.unwrap();
    let settled = tokio::time::timeout(Duration::from_secs(25), async {
        while f.control.is_busy() {
            f.control.maintain().await;
        }
    })
    .await;
    tokio::time::resume();
    settled.expect("progress must not extend the original execution budget");
    assert!(matches!(
        &f.session.view().unwrap().delegations()[0].outcome,
        Some(DelegationOutcome::Joined {
            execution_timed_out: true, answer: None, receipt: Some(receipt), ..
        }) if receipt.end == Some(TurnEndReason::Cancelled)
    ));
    f.control.shutdown().await;
    let root = f.root.clone();
    drop(f);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn explicit_stop_wins_over_unobserved_expiry_and_cleanup_has_no_new_deadline() {
    let owner = Arc::new(BackgroundOwner::new(1).unwrap());
    let mut f = Fixture::with_owner(
        Wait::Join,
        true,
        false,
        ion_core::DelegationContext::Fresh,
        Some(owner.clone()),
        Some(60_000),
    );
    let mut started = f.effect.started.subscribe();
    let mut stopped = f.effect.stopped.subscribe();
    f.start();
    background::drain(&mut f.control).await;
    started.wait_for(|started| *started).await.unwrap();
    let id = f.session.view().unwrap().delegations()[0].record.child;
    owner.request_stop(id).unwrap();
    tokio::time::pause();
    tokio::time::sleep(Duration::from_secs(120)).await;
    stopped.wait_for(|stopped| *stopped).await.unwrap();
    assert!(!owner.child(id).unwrap().joined);
    tokio::time::resume();
    f.effect.release.notify_one();
    let report = owner.wait(&[id], Duration::from_secs(5)).await.unwrap();
    assert!(matches!(
        &report.children[0].receipt.outcome,
        Some(DelegationOutcome::Joined {
            execution_timed_out: false, receipt: Some(receipt), answer: None, ..
        }) if receipt.end == Some(TurnEndReason::Cancelled)
    ));
    owner.join_shutdown().await.unwrap();
    f.control.shutdown().await;
    let root = f.root.clone();
    drop((f, owner));
    std::fs::remove_dir_all(root).unwrap();
}
