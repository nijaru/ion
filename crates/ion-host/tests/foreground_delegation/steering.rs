use super::*;
use ion_core::RequestKey;
use ion_host::SteeringStatus;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn key(id: &str) -> RequestKey {
    RequestKey::new("steering-fixture".into(), id.into()).unwrap()
}

struct HeldContinuation {
    model: ScriptedModelService,
    continuation: watch::Sender<bool>,
    release: Notify,
}
impl ion_ai::ModelService for HeldContinuation {
    fn stream<'a>(
        &'a self,
        request: ion_ai::ModelRequest,
    ) -> BoxFuture<'a, Result<ion_ai::ModelStream, ion_ai::ProviderError>> {
        Box::pin(async move {
            if !self.model.requests().is_empty() {
                self.continuation.send_replace(true);
                self.release.notified().await;
            }
            self.model.stream(request).await
        })
    }
}

#[tokio::test]
async fn steering_commit_invalidates_owner_before_child_join_without_retaining_a_waiter() {
    let owner = Arc::new(ion_host::BackgroundOwner::new(1).unwrap());
    let model = Arc::new(HeldContinuation {
        model: ScriptedModelService::new([
            response(vec![call("effect", json!({}))]),
            response(vec![Content::Text("guided answer".into())]),
        ]),
        continuation: watch::channel(false).0,
        release: Notify::new(),
    });
    let mut f = Fixture::with_child_service(
        Wait::Join,
        true,
        false,
        None,
        Some(owner.clone()),
        None,
        Some(model.clone()),
    );
    f.start();
    f.effect
        .started
        .subscribe()
        .wait_for(|started| *started)
        .await
        .unwrap();
    let child = f.session.view().unwrap().delegations()[0].record.run;
    let message = Message::user_input("live guidance".into(), []);
    let report = owner
        .steer(child, &key("owned"), &message, &CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(report.status, SteeringStatus::Queued));
    let revision = owner.revision();
    assert!(
        tokio::time::timeout(Duration::from_millis(5), owner.changed(revision))
            .await
            .is_err()
    );
    f.effect.release.notify_one();
    // The original borrowed waiter is gone. Commit alone must invalidate the
    // owner; the held continuation prevents completion from masking a lost wake.
    tokio::time::timeout(Duration::from_secs(5), owner.changed(revision))
        .await
        .unwrap();
    model
        .continuation
        .subscribe()
        .wait_for(|entered| *entered)
        .await
        .unwrap();
    let snapshot = owner.run(child).unwrap();
    assert!(!snapshot.joined);
    assert!(matches!(
        snapshot.steering[0].status,
        SteeringStatus::Committed { .. }
    ));
    model.release.notify_one();
    assert!(
        owner
            .wait(&[child], Duration::from_secs(5))
            .await
            .unwrap()
            .runs[0]
            .joined
    );
    f.settle().await;
    owner.join_shutdown().await.unwrap();
    assert!(owner.take(child).unwrap().is_ok());
    assert!(
        owner
            .steer(child, &key("owned"), &message, &CancellationToken::new())
            .await
            .is_err()
    );
    f.session.shutdown().await.unwrap();
    let root = f.root.clone();
    drop(f);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn cold_readiness_wait_cancellation_neither_admits_nor_stops_prepared_work() {
    let mut f = Fixture::new(Wait::Join, false, false);
    let prepared = f
        .delegate
        .child
        .prepare(Message::user_input("cold".into(), []))
        .unwrap();
    let message = Message::user_input("guide".into(), []);
    let stop = CancellationToken::new();
    let request = key("cold");
    {
        let steering = prepared.steering.steer(&request, &message, &stop);
        tokio::pin!(steering);
        tokio::select! {
            biased;
            result = &mut steering => panic!("cold task unexpectedly handled guidance: {:?}", result.err()),
            () = tokio::time::sleep(Duration::from_millis(5)) => {}
        }
        stop.cancel();
        assert!(steering.await.is_err());
    }
    assert!(prepared.steering.snapshot().is_empty());
    assert!(f.child.requests().is_empty());
    assert!(!f.root.join("children").exists());
    drop(prepared.task);
    assert!(
        prepared
            .steering
            .steer(&request, &message, &CancellationToken::new())
            .await
            .is_err()
    );
    assert!(prepared.steering.snapshot().is_empty());
    f.control.shutdown().await;
    f.session.shutdown().await.unwrap();
    let root = f.root.clone();
    drop(f);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn foreground_sdk_grant_reconciles_inputs_and_never_reopens_a_settled_child() {
    let service = Arc::new(ScriptedModelService::new([
        response(vec![call("effect", json!({}))]),
        response(vec![Content::Text("guided answer".into())]),
    ]));
    let mut f = Fixture::with_child_service(
        Wait::Join,
        true,
        false,
        None,
        None,
        None,
        Some(service.clone()),
    );
    f.start();
    f.effect
        .started
        .subscribe()
        .wait_for(|started| *started)
        .await
        .unwrap();
    let steering = f.delegate.steering.lock().unwrap().clone().unwrap();
    let request = key("same");
    let message = Message::user_input("/literal 🦀".into(), []);
    let stop = CancellationToken::new();
    let first = steering.steer(&request, &message, &stop).await.unwrap();
    assert!(matches!(first.status, SteeringStatus::Queued));
    assert_eq!(
        steering.steer(&request, &message, &stop).await.unwrap().id,
        first.id
    );
    assert!(
        steering
            .steer(&request, &Message::user_input("conflict".into(), []), &stop)
            .await
            .is_err()
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        steering
            .steer(&key("cancelled"), &message, &cancelled)
            .await
            .is_err()
    );
    assert_eq!(steering.snapshot().len(), 1);
    f.effect.release.notify_one();
    let report = f.settle().await;
    assert!(report.cleanup_errors.is_empty());
    assert!(matches!(
        steering.snapshot()[0].status,
        SteeringStatus::Committed { .. }
    ));
    assert_eq!(
        steering.steer(&request, &message, &stop).await.unwrap().id,
        first.id
    );
    assert!(
        steering
            .steer(&key("after"), &message, &stop)
            .await
            .is_err()
    );
    let requests = service.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|input| input == &message));
    assert_eq!(requests[0].instructions, requests[1].instructions);
    let link = f.session.view().unwrap().delegations().pop().unwrap();
    let Some(DelegationOutcome::Joined { answer, .. }) = link.outcome else {
        panic!("joined child")
    };
    assert_eq!(answer.as_deref(), Some("guided answer"));
    let child = CodingSession::inspect(&link.record.path).unwrap();
    assert_eq!(child.last_end, Some((1, TurnEndReason::Completed)));
    assert_eq!(
        child
            .entries
            .iter()
            .filter(|entry| matches!(entry, ion_core::SessionEntry::Steering { .. }))
            .count(),
        1
    );
    f.session.shutdown().await.unwrap();
    let root = f.root.clone();
    drop(f);
    std::fs::remove_dir_all(root).unwrap();
}
