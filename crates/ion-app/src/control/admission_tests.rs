//! Retained Host custody over real Core admission, with no live providers.
use super::*;
use std::sync::{Condvar, atomic::AtomicBool};

struct AdmissionModel {
    model: Arc<Model>,
    executor: std::thread::ThreadId,
    entered: Notify,
    released: Mutex<bool>,
    changed: Condvar,
    first: AtomicBool,
    after_claim: std::sync::OnceLock<Arc<ion_core::CompletionInbox>>,
    panic: bool,
}
impl AdmissionModel {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
impl ModelService for AdmissionModel {
    fn validate_controls(
        &self,
        _model: &ion_ai::ModelRef,
        controls: &ion_ai::GenerationControls,
    ) -> Result<(), ProviderError> {
        if self.after_claim.get().is_none_or(|inbox| !inbox.has_wake())
            && self.first.swap(false, Ordering::SeqCst)
        {
            assert_ne!(
                std::thread::current().id(),
                self.executor,
                "admission blocked the executor"
            );
            self.entered.notify_one();
            let (_released, timeout) = self
                .changed
                .wait_timeout_while(
                    self.released.lock().unwrap(),
                    Duration::from_secs(5),
                    |released| !*released,
                )
                .unwrap();
            assert!(!timeout.timed_out(), "test did not release admission");
            assert!(!self.panic, "injected actual admission task panic");
        }
        controls.validate()
    }
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        self.model.stream(request)
    }
}
fn gated_fixture(panic: bool) -> (Fixture, Arc<AdmissionModel>) {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    let service = Arc::new(AdmissionModel {
        model,
        executor: std::thread::current().id(),
        entered: Notify::new(),
        released: Mutex::new(false),
        changed: Condvar::new(),
        first: AtomicBool::new(true),
        after_claim: std::sync::OnceLock::new(),
        panic,
    });
    let agent = fixture.control.agent();
    fixture.control.setup.runtime.agent = Arc::new(
        Agent::with_tool_set(
            service.clone(),
            Arc::new(ToolSet::new([])),
            agent.model().clone(),
        )
        .with_limits(agent.limits())
        .with_recovery_scope(ion_core::AuthorityScope::new("host-admission-fixture").unwrap()),
    );
    (fixture, service)
}
fn key() -> ion_core::RequestKey {
    ion_core::RequestKey::new("host-fixture".into(), "exact-delivery".into()).unwrap()
}
fn message() -> Message {
    Message::user_input("  exact original 雪\n".into(), [])
}
async fn join_admission(control: &mut SessionControl<Metadata>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while control.admitting.is_some() {
            control.maintain().await;
        }
    })
    .await
    .unwrap();
}

// Drive the SDK far enough to start the actual worker, then abandon only its
// wait. The timer and worker's thread assertion independently protect executor
// responsiveness; metadata must already be in the controller at this boundary.
async fn abandon_request(
    control: &mut SessionControl<Metadata>,
    service: &AdmissionModel,
) -> InputId {
    let key = key();
    let mut wait = Box::pin(control.submit_request(&key, message(), meta("  exact M 🦀\n")));
    tokio::select! {
        () = service.entered.notified() => {},
        result = &mut wait => panic!("admission completed before release: {}", result.is_ok()),
        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("admission did not enter its worker"),
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut wait)
            .await
            .is_err()
    );
    drop(wait);
    let AdmissionPayload::Request {
        id,
        metadata,
        original,
        ..
    } = &control.admitting.as_ref().unwrap().payload
    else {
        panic!("request custody was not retained")
    };
    metadata.with(|metadata| {
        assert_eq!(metadata.literal, "  exact M 🦀\n");
        assert_eq!(metadata.marker.get(), 7);
    });
    assert!(matches!(original, UserInput::Message(input) if input == &message()));
    assert_eq!(lock(&control.custody).occupied, 1);
    assert!(control.active.is_none());
    assert!(
        !control.stop_requested,
        "an explicit pending Start admits ordered intent"
    );
    assert!(
        control.steering().is_none(),
        "preparation is not an acceptance grant"
    );
    *id
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_keyed_wait_shutdown_delivers_positive_receipt_and_exact_metadata() {
    let (mut fixture, service) = gated_fixture(false);
    let mut observer = fixture.control.observe();
    assert!(matches!(
        observer.next().await,
        Some(Observation::Gap { skipped: 0 })
    ));
    let id = abandon_request(&mut fixture.control, &service).await;
    service.release();
    // Core can already execute before Host has polled/adopted its admission.
    service.model.wait_calls(1).await;
    assert!(fixture.control.admitting.is_some() && fixture.control.active.is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), observer.next())
            .await
            .is_err(),
        "passive progress preceded positive Host admission"
    );
    let RequestAdmission::Existing { metadata, .. } = fixture
        .control
        .submit_request(&key(), message(), meta("busy retry M"))
        .await
        .unwrap()
    else {
        panic!("busy reconciliation started new work")
    };
    assert_eq!(metadata.literal, "busy retry M");
    assert_eq!(metadata.marker.get(), 7);
    assert!(
        fixture.control.admitting.is_some(),
        "reconciliation must not adopt or drive the original admission"
    );
    service.model.release();
    fixture.control.request_shutdown();
    let RequestAdmission::Existing { metadata, .. } = fixture
        .control
        .submit_request(&key(), message(), meta("closed retry M"))
        .await
        .unwrap()
    else {
        panic!("closed reconciliation started new work")
    };
    assert_eq!(metadata.literal, "closed retry M");
    assert_eq!(metadata.marker.get(), 7);
    let report = fixture.control.shutdown().await;
    let mut positive = None;
    let mut settled = None;
    for update in report.updates {
        match update {
            ControlUpdate::RequestAdmission(decision) => {
                assert!(positive.is_none() && settled.is_none());
                assert_eq!(decision.id, id);
                let RequestAdmission::Started {
                    id: accepted,
                    receipt,
                } = decision.result.unwrap()
                else {
                    panic!("committed admission was not published positively")
                };
                assert_eq!(accepted, id);
                assert_eq!(receipt.key, key());
                assert_eq!(receipt.session, fixture.control.session().id());
                positive = Some(receipt);
            }
            ControlUpdate::Accepted {
                id: accepted, turn, ..
            } => {
                assert_eq!(accepted, id);
                assert_eq!(Some(turn), positive.as_ref().map(|receipt| receipt.turn));
            }
            ControlUpdate::Activity(_) => assert!(positive.is_some()),
            ControlUpdate::Settled(result) => settled = Some(result),
            ControlUpdate::Returned(_) | ControlUpdate::Unresolved(_) => {
                panic!("positive receipt cannot return its input")
            }
            _ => {}
        }
    }
    let result = settled.unwrap();
    assert_eq!(result.accepted.len(), 1);
    assert_eq!(result.accepted[0].id, id);
    assert_eq!(result.accepted[0].metadata.literal, "  exact M 🦀\n");
    assert_eq!(result.accepted[0].metadata.marker.get(), 7);
    let receipt = fixture
        .control
        .session()
        .reader()
        .unwrap()
        .request_receipt(&key())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(receipt.turn, positive.unwrap().turn);
    assert_eq!(lock(&fixture.control.custody).occupied, 0);
    assert_eq!(service.model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_keyed_rejection_uncertainty_and_task_panic_keep_distinct_custody() {
    for fault in ["reject", "uncertain", "panic"] {
        let (mut fixture, service) = gated_fixture(fault == "panic");
        if fault == "reject" {
            fixture.fault("1");
        } else if fault == "uncertain" {
            // A real COMMIT failure, as opposed to a confirmed statement rollback.
            rusqlite::Connection::open(fixture.control.session().path()).unwrap().execute_batch(
                "CREATE TABLE commit_parent (id INTEGER PRIMARY KEY);
                 CREATE TABLE commit_child (parent INTEGER REFERENCES commit_parent(id) DEFERRABLE INITIALLY DEFERRED);
                 CREATE TRIGGER uncertain_commit AFTER INSERT ON entries
                 BEGIN INSERT INTO commit_child(parent) VALUES (999); END;"
            ).unwrap();
        }
        fixture.control.request_stop();
        let id = abandon_request(&mut fixture.control, &service).await;
        service.release();
        // Join/adopt, but do not consume its reliable decision or reservation.
        join_admission(&mut fixture.control).await;
        assert!(fixture.control.admitting.is_none());
        assert!(
            fixture.control.stop_requested,
            "failed Start must retain prior stop intent"
        );
        assert_eq!(lock(&fixture.control.custody).occupied, 1);
        let report = fixture.control.shutdown().await;
        let mut decision = None;
        for update in report.updates {
            match update {
                ControlUpdate::RequestAdmission(result) => {
                    assert!(decision.is_none());
                    decision = Some(result);
                }
                ControlUpdate::Accepted { .. }
                | ControlUpdate::Dispatched { .. }
                | ControlUpdate::Settled(_) => panic!("no admission was positively acknowledged"),
                _ => {}
            }
        }
        let result = decision.unwrap();
        assert_eq!(result.id, id);
        let (input, metadata) = if fault == "reject" {
            let rejected = result
                .result
                .err()
                .expect("confirmed rejection returns retryable originals");
            assert!(
                rejected
                    .reason
                    .to_string()
                    .contains("injected storage fault")
            );
            (rejected.input, rejected.metadata)
        } else {
            let RequestAdmission::Unresolved(input) = result.result.unwrap() else {
                panic!("unknown admission must not become a retryable rejection")
            };
            assert_eq!(input.id, id);
            assert_eq!(input.key, key());
            assert_eq!(input.session, fixture.control.session().id());
            (input.input, input.metadata)
        };
        assert!(matches!(input, UserInput::Message(input) if input == message()));
        assert_eq!(metadata.literal, "  exact M 🦀\n");
        assert_eq!(metadata.marker.get(), 7);
        assert_eq!(lock(&fixture.control.custody).occupied, 0);
        assert_eq!(service.model.calls.load(Ordering::SeqCst), 0);
        if fault == "uncertain" {
            let rejected = fixture
                .control
                .submit_request(
                    &ion_core::RequestKey::new("host-fixture".into(), "new-refusal".into())
                        .unwrap(),
                    message(),
                    meta("new refusal M"),
                )
                .await
                .err()
                .expect("faulted Store refuses a new attempt");
            assert_eq!(rejected.metadata.literal, "new refusal M");
            assert!(matches!(
                rejected.reason.downcast_ref::<SessionError>(),
                Some(SessionError::StoreFaulted)
            ));
        }
    }
}

#[tokio::test]
async fn stale_new_plan_reconciles_racing_existing_key_with_exact_metadata_and_capacity() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    let plan = fixture
        .control
        .prepare_request(key(), message())
        .await
        .unwrap();
    assert!(plan.existing().is_none());
    let submitted = fixture
        .control
        .agent()
        .submit_request::<_, InputId>(
            fixture.control.session().clone(),
            ion_core::SubmissionRequest {
                key: key(),
                input: message(),
            },
            fixture.control.instructions().to_owned(),
            CancellationToken::new(),
            None,
            |_| {},
        )
        .await
        .unwrap();
    model.release();
    submitted.operation.unwrap().await.unwrap();
    fixture.control.request_stop();
    let RequestLaunch::Pending(id) = fixture
        .control
        .launch_request(plan, meta("racing M"))
        .unwrap()
    else {
        panic!("the read-only plan was originally new")
    };
    join_admission(&mut fixture.control).await;
    assert_eq!(lock(&fixture.control.custody).occupied, 1);
    let ControlUpdate::RequestAdmission(decision) = fixture.control.take_update().unwrap() else {
        panic!("race must produce a reliable admission decision")
    };
    assert_eq!(decision.id, id);
    let RequestAdmission::Existing { receipt, metadata } = decision.result.unwrap() else {
        panic!("racing durable key must not execute again")
    };
    assert_eq!(receipt.turn, submitted.receipt.turn);
    assert_eq!(metadata.literal, "racing M");
    assert_eq!(metadata.marker.get(), 7);
    assert!(
        fixture.control.stop_requested,
        "Existing must not clear prior stop intent"
    );
    assert_eq!(lock(&fixture.control.custody).occupied, 0);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    fixture.control.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_recovery_failure_retains_checkpoint_metadata_and_reservation() {
    let (mut fixture, service) = gated_fixture(false);
    let model = fixture.control.agent().model().clone();
    fixture
        .control
        .session()
        .io()
        .unwrap()
        .run(move |session| session.select_model(model))
        .await
        .unwrap();
    let point = ion_core::RecoveryPoint {
        session: fixture.control.session().id(),
        turn: 1,
        checkpoint: 1,
    };
    let mut wait = Box::pin(fixture.control.resume_turn(point, meta("exact recovery M")));
    tokio::select! {
        () = service.entered.notified() => {},
        result = &mut wait => panic!("recovery completed before release: {}", result.is_ok()),
        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("recovery did not enter its worker"),
    }
    drop(wait);
    service.release();
    join_admission(&mut fixture.control).await;
    assert_eq!(lock(&fixture.control.custody).occupied, 1);
    let report = fixture.control.shutdown().await;
    let failure = report
        .updates
        .into_iter()
        .find_map(|update| {
            if let ControlUpdate::RecoveryAdmission(decision) = update {
                Some(decision.result.unwrap_err())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(failure.point, point);
    assert_eq!(failure.kind, AdmissionFailureKind::NotAccepted);
    assert_eq!(failure.metadata.literal, "exact recovery M");
    assert_eq!(failure.metadata.marker.get(), 7);
    assert_eq!(lock(&fixture.control.custody).occupied, 0);
    assert_eq!(service.model.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_automated_admission_is_joined_on_stop_without_resubmitting() {
    for claimed in [false, true] {
        let (mut fixture, service) = gated_fixture(false);
        if claimed {
            assert!(
                service
                    .after_claim
                    .set(fixture.control.completions.inbox())
                    .is_ok()
            );
        }
        fixture
            .control
            .set_completion_delivery(ion_core::CompletionDelivery::Wake)
            .unwrap();
        let permit = fixture
            .control
            .completion_target(ion_core::CompletionDelivery::Wake)
            .await
            .unwrap()
            .reserve()
            .unwrap();
        permit.offer(ion_core::CompletionNotice {
            source_session: uuid::Uuid::now_v7(),
            run: uuid::Uuid::now_v7(),
            child_session: uuid::Uuid::now_v7(),
        });
        let mut wait = Box::pin(fixture.control.next());
        tokio::select! {
            () = service.entered.notified() => {},
            _ = &mut wait => panic!("automated admission published before its actual decision"),
            () = tokio::time::sleep(Duration::from_secs(5)) => panic!("automated admission did not enter its worker"),
        }
        drop(wait);
        assert!(matches!(
            fixture
                .control
                .admitting
                .as_ref()
                .map(|admission| &admission.payload),
            Some(AdmissionPayload::Completion)
        ));
        fixture.control.request_stop();
        service.release();
        let report = fixture.control.shutdown().await;
        assert!(report.cleanup_errors.is_empty());
        if !claimed {
            // Stop revoked readiness before Core selected it. No Turn was admitted;
            // inventing a cancelled settlement here would be a false positive.
            assert!(report.updates.is_empty());
            assert!(fixture.control.session().view().unwrap().entries.is_empty());
            assert!(matches!(
                permit.receipt().snapshot(),
                ion_core::CompletionStatus::Revoked {
                    reason: ion_core::CompletionRevocation::Stopped,
                }
            ));
            assert_eq!(service.model.calls.load(Ordering::SeqCst), 0);
            assert_eq!(lock(&fixture.control.custody).occupied, 0);
            assert!(!fixture.control.is_busy());
            continue;
        }
        let results = report
            .updates
            .into_iter()
            .filter_map(|update| {
                if let ControlUpdate::Settled(result) = update {
                    Some(result)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 1);
        assert!(
            results[0].automated && results[0].metadata.is_none() && results[0].accepted.is_empty()
        );
        assert!(results[0].stop_requested);
        assert!(matches!(
            results[0].outcome,
            OperationOutcome::Turn(Err(AgentError::Cancelled))
        ));
        assert_eq!(service.model.calls.load(Ordering::SeqCst), 0);
        assert_eq!(lock(&fixture.control.custody).occupied, 0);
        assert!(!fixture.control.is_busy());
        assert!(matches!(
            permit.receipt().snapshot(),
            ion_core::CompletionStatus::Offered { turn: 1 }
        ));
    }
}

#[tokio::test]
async fn completion_target_read_cannot_regrant_revoked_authority_after_rearm() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture
        .control
        .set_completion_delivery(ion_core::CompletionDelivery::Wake)
        .unwrap();
    let reading = fixture
        .control
        .prepare_completion_target(ion_core::CompletionDelivery::Wake);
    fixture.control.request_stop();
    fixture
        .control
        .submit(Delivery::Start, prompt("explicit rearm"), meta("human"))
        .unwrap();
    let plan = reading.await.unwrap();
    assert!(fixture.control.finish_completion_target(plan).is_err());
    model.wait_calls(1).await;
    model.release();
    settle(&mut fixture.control).await;
    fixture.control.shutdown().await;
}
