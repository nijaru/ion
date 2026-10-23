use super::*;
use ion_host::{BackgroundOwner, ChildActivity};
use std::time::Duration;

#[tokio::test]
async fn live_observations_invalidate_without_readers_and_do_not_own_execution() {
    let owner = Arc::new(BackgroundOwner::new(1).unwrap());
    let mut f = Fixture::with_owner(Wait::Join, true, false, None, Some(owner.clone()), None);
    let mut started = f.effect.started.subscribe();
    let mut stopped = f.effect.stopped.subscribe();
    f.start();
    tokio::time::timeout(Duration::from_secs(5), started.wait_for(|started| *started))
        .await
        .unwrap()
        .unwrap();
    background::drain(&mut f.control).await;
    let record = f.session.view().unwrap().delegations()[0].record.clone();

    // A real owner invalidation, not a polling loop or borrowed execution waiter.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let revision = owner.revision();
            let live = owner.run(record.run).unwrap();
            if live
                .progress
                .as_ref()
                .is_some_and(|progress| progress.status.activity == Some(ChildActivity::Tool))
            {
                assert!(!live.joined && live.receipt.outcome.is_none());
                break;
            }
            owner.changed(revision).await;
        }
    })
    .await
    .unwrap();
    let reader = f.delegate.progress.lock().unwrap().clone().unwrap();
    let live = reader.snapshot().unwrap();
    assert!(live.provisional_output.is_none());
    let live = live.status;
    assert_eq!(live.child_session, record.child);
    assert_eq!(live.request, record.request_key());
    assert_eq!(live.current_tool.as_deref(), Some("effect"));
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(2)).await;
    let silent = owner.run(record.run).unwrap().progress.unwrap().status;
    assert_eq!(silent.last_activity_at_ms, live.last_activity_at_ms);
    assert!(silent.elapsed_ms >= live.elapsed_ms + 2000);
    tokio::time::resume();

    owner.request_stop(record.run).unwrap();
    tokio::time::timeout(Duration::from_secs(5), stopped.wait_for(|stopped| *stopped))
        .await
        .unwrap()
        .unwrap();
    let stopping = reader.activity().unwrap();
    assert!(stopping.stopping && stopping.activity.is_none());
    assert!(!owner.run(record.run).unwrap().joined);
    assert!(owner.take(record.run).is_none());
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    assert_eq!(reader.activity().unwrap().elapsed_ms, stopping.elapsed_ms);
    tokio::time::resume();

    f.effect.release.notify_one();
    let joined = owner
        .wait(&[record.run], Duration::from_secs(5))
        .await
        .unwrap();
    assert!(joined.runs[0].joined);
    let DelegationOutcome::Joined { elapsed_ms, .. } =
        joined.runs[0].receipt.outcome.as_ref().unwrap()
    else {
        panic!("joined")
    };
    assert_eq!(*elapsed_ms, Some(stopping.elapsed_ms));
    let acknowledged = owner.take(record.run).unwrap().unwrap();
    assert_eq!(
        acknowledged.progress.unwrap().status.elapsed_ms,
        stopping.elapsed_ms
    );
    assert_eq!(reader.activity().unwrap().elapsed_ms, stopping.elapsed_ms);
    // Even an actual retained observation grant cannot pin the child writer.
    let reopened = CodingSession::open(&record.path).unwrap();
    assert_eq!(reopened.id(), record.child);
    drop(reopened);
    owner.join_shutdown().await.unwrap();
    f.settle().await;
}
