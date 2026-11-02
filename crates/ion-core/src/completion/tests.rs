use super::*;
use std::{path::PathBuf, sync::mpsc, time::Duration};

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ion-completion-{}", Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn session(&self) -> Session {
        Session::create(self.0.join("session.sqlite"), Some(&self.0)).unwrap()
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn notice() -> CompletionNotice {
    CompletionNotice {
        source_session: Uuid::now_v7(),
        run: Uuid::now_v7(),
        child_session: Uuid::now_v7(),
    }
}
fn model() -> ion_ai::ModelRef {
    ion_ai::ModelRef {
        provider: "test".into(),
        model: "test".into(),
    }
}
fn queued(inbox: &Arc<CompletionInbox>) -> CompletionPermit {
    let permit = inbox.reserve(0, None, CompletionDelivery::Wake).unwrap();
    permit.offer(notice());
    permit
}
fn store(session: &Session, batch: &CompletionBatch) -> Result<u64, SessionError> {
    let notices = batch.notices();
    let admission =
        session.admit_turn_input(notice_message(&notices), model(), None, None, notices)?;
    let crate::session::TurnAdmission::New { turn, .. } = admission else {
        panic!("anonymous completion admission must be new");
    };
    Ok(turn)
}

#[test]
fn another_session_cannot_consume_or_destroy_receiving_custody() {
    let root = TestDir::new();
    let other = root.session();
    let inbox = CompletionInbox::new(Uuid::now_v7());
    // Invalid on the other Session's branch: mismatch must win over pruning.
    let permit = inbox.reserve(99, None, CompletionDelivery::Wake).unwrap();
    permit.offer(notice());
    let revision = inbox.revision();
    assert!(matches!(
        inbox.batch(&other, None, false),
        Err(SessionError::CompletionTargetMismatch)
    ));
    assert_eq!(inbox.revision(), revision);
    assert!(inbox.has_wake());
    assert_eq!(permit.receipt().snapshot(), CompletionStatus::Queued);
}

#[test]
fn reservations_are_bounded_revocable_and_do_not_pin_receiver() {
    let inbox = CompletionInbox::new(Uuid::now_v7());
    let permits = (0..64)
        .map(|_| inbox.reserve(0, None, CompletionDelivery::Wake).unwrap())
        .collect::<Vec<_>>();
    assert!(inbox.reserve(0, None, CompletionDelivery::Wake).is_err());
    let receipt = permits[0].receipt();
    inbox.revoke(CompletionRevocation::Stopped);
    permits[0].offer(notice());
    assert!(!inbox.has_wake());
    assert_eq!(
        receipt.snapshot(),
        CompletionStatus::Revoked {
            reason: CompletionRevocation::Stopped
        }
    );
    let fresh = inbox.reserve(0, None, CompletionDelivery::Wake).unwrap();
    let fresh_receipt = fresh.receipt();
    let weak = Arc::downgrade(&inbox);
    drop(inbox);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        fresh_receipt.snapshot(),
        CompletionStatus::Revoked {
            reason: CompletionRevocation::Closed
        }
    );
}

#[test]
fn delivery_modes_gate_unsolicited_batches_but_share_natural_requests() {
    let root = TestDir::new();
    let session = root.session();
    for (delivery, turn, natural, eligible) in [
        (CompletionDelivery::Inbox, Some(7), false, false),
        (CompletionDelivery::Continue, Some(7), false, true),
        (CompletionDelivery::Continue, Some(8), false, false),
        (CompletionDelivery::Continue, None, false, false),
        (CompletionDelivery::Wake, None, false, true),
        (CompletionDelivery::Inbox, None, true, true),
        (CompletionDelivery::Continue, None, true, true),
    ] {
        let inbox = CompletionInbox::new(session.id());
        let permit = inbox.reserve(0, Some(7), delivery).unwrap();
        let data = notice();
        permit.offer(data.clone());
        let batch = inbox.batch(&session, turn, natural).unwrap();
        assert_eq!(batch.notices(), if eligible { vec![data] } else { vec![] });
        assert_eq!(permit.receipt().snapshot(), CompletionStatus::Queued);
    }

    let inbox = CompletionInbox::new(session.id());
    let inbox_only = inbox.reserve(0, None, CompletionDelivery::Inbox).unwrap();
    inbox_only.offer(notice());
    let wake = queued(&inbox);
    let batch = inbox.batch(&session, None, false).unwrap();
    assert_eq!(batch.notices().len(), 2, "Wake carries other ready notices");
    assert!(!inbox.has_wake());
    assert!(
        inbox
            .batch(&session, None, true)
            .unwrap()
            .notices()
            .is_empty()
    );
    drop(batch);
    assert!(inbox.has_wake());
    drop(wake);
}

#[test]
fn branch_changes_revoke_only_unclaimed_off_branch_custody() {
    let root = TestDir::new();
    let session = root.session();
    session.select_model(model()).unwrap();
    let anchor = session.context_position().unwrap().head_entry;
    assert_ne!(anchor, 0);
    let inbox = CompletionInbox::new(session.id());
    let claimed = inbox
        .reserve(anchor, None, CompletionDelivery::Wake)
        .unwrap();
    claimed.offer(notice());
    let mut batch = inbox.batch(&session, None, false).unwrap();
    batch.begin();
    let queued = inbox
        .reserve(anchor, None, CompletionDelivery::Wake)
        .unwrap();
    queued.offer(notice());
    let waiting = inbox
        .reserve(anchor, None, CompletionDelivery::Inbox)
        .unwrap();
    let root_notice = inbox.reserve(0, None, CompletionDelivery::Inbox).unwrap();
    root_notice.offer(notice());
    session
        .prepare_navigation(session.context_position().unwrap(), 0)
        .unwrap()
        .commit()
        .unwrap();
    let revision = inbox.revision();
    assert!(
        inbox
            .batch(&session, None, false)
            .unwrap()
            .notices()
            .is_empty()
    );
    assert_ne!(
        inbox.revision(),
        revision,
        "branch pruning invalidates readers"
    );
    for permit in [&queued, &waiting] {
        assert_eq!(
            permit.receipt().snapshot(),
            CompletionStatus::Revoked {
                reason: CompletionRevocation::BranchChanged
            }
        );
    }
    assert_eq!(claimed.receipt().snapshot(), CompletionStatus::Queued);
    // A known rejection releases the old claim. Selection rechecks its branch.
    batch.reject();
    let next = inbox.batch(&session, None, true).unwrap();
    assert_eq!(next.notices().len(), 1);
    assert_eq!(
        claimed.receipt().snapshot(),
        CompletionStatus::Revoked {
            reason: CompletionRevocation::BranchChanged
        }
    );
    assert_eq!(root_notice.receipt().snapshot(), CompletionStatus::Queued);
}

#[test]
fn in_flight_withdrawal_is_nonblocking_and_durable_success_wins() {
    for action in ["drop", "acknowledge", "revoke", "close"] {
        let root = TestDir::new();
        let session = root.session();
        let inbox = CompletionInbox::new(session.id());
        let permit = queued(&inbox);
        let receipt = permit.receipt();
        let mut batch = inbox.batch(&session, None, false).unwrap();
        let notices = batch.notices();
        batch.begin();
        let (resume, wait) = mpsc::channel();
        // Moving the batch into the storage worker also verifies Send + 'static.
        let worker = std::thread::spawn(move || {
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
            let turn = store(&session, &batch).unwrap();
            batch.commit(turn);
            (turn, session)
        });
        let target = inbox.clone();
        let (done, completed) = mpsc::channel();
        let withdrawer = std::thread::spawn(move || {
            match action {
                "drop" => drop(permit),
                "acknowledge" => permit.acknowledge(),
                "revoke" => target.revoke(CompletionRevocation::Stopped),
                "close" => target.close(CompletionRevocation::Closed),
                _ => unreachable!(),
            }
            done.send(()).unwrap();
        });
        let nonblocking = completed.recv_timeout(Duration::from_secs(2));
        // Always release the worker, even if the nonblocking assertion fails.
        resume.send(()).unwrap();
        let (turn, session) = worker.join().unwrap();
        withdrawer.join().unwrap();
        nonblocking.expect("withdrawal waited for the storage claim");
        assert_eq!(receipt.snapshot(), CompletionStatus::Offered { turn });
        assert!(!inbox.has_wake());
        assert!(session.view().unwrap().entries.iter().any(|entry| matches!(
            entry, crate::SessionEntry::TurnStarted { completion, .. } if completion == &notices
        )));
    }
}

#[test]
fn rejection_and_unbegun_drop_apply_first_withdrawal_or_requeue() {
    let root = TestDir::new();
    let session = root.session();
    for begun in [false, true] {
        for withdrawn in [false, true] {
            let inbox = CompletionInbox::new(session.id());
            let permit = queued(&inbox);
            let mut batch = inbox.batch(&session, None, false).unwrap();
            if begun {
                batch.begin();
            }
            if withdrawn {
                permit.acknowledge();
                inbox.revoke(CompletionRevocation::Stopped);
                permit.withdraw();
            }
            assert_eq!(permit.receipt().snapshot(), CompletionStatus::Queued);
            if begun {
                batch.reject();
            } else {
                drop(batch);
            }
            if withdrawn {
                assert_eq!(
                    permit.receipt().snapshot(),
                    CompletionStatus::Revoked {
                        reason: CompletionRevocation::Acknowledged
                    }
                );
                assert!(!inbox.has_wake());
            } else {
                assert_eq!(permit.receipt().snapshot(), CompletionStatus::Queued);
                assert!(inbox.has_wake());
                assert_eq!(
                    inbox.batch(&session, None, false).unwrap().notices().len(),
                    1
                );
            }
        }
    }
}

#[test]
fn storage_rejection_can_retry_without_a_false_offer() {
    let root = TestDir::new();
    let session = root.session();
    let connection = rusqlite::Connection::open(session.path()).unwrap();
    connection.execute_batch(
        "CREATE TRIGGER reject_completion BEFORE INSERT ON entries BEGIN SELECT RAISE(ABORT, 'storage unavailable'); END;"
    ).unwrap();
    let inbox = CompletionInbox::new(session.id());
    let permit = queued(&inbox);
    let mut batch = inbox.batch(&session, None, false).unwrap();
    batch.begin();
    assert!(matches!(
        store(&session, &batch),
        Err(SessionError::Sqlite(_))
    ));
    batch.reject();
    assert!(session.view().unwrap().entries.is_empty());
    assert_eq!(permit.receipt().snapshot(), CompletionStatus::Queued);
    assert!(inbox.has_wake());
    connection
        .execute_batch("DROP TRIGGER reject_completion;")
        .unwrap();
    let mut batch = inbox.batch(&session, None, false).unwrap();
    batch.begin();
    let turn = store(&session, &batch).unwrap();
    assert_eq!(permit.receipt().snapshot(), CompletionStatus::Queued);
    batch.commit(turn);
    assert_eq!(
        permit.receipt().snapshot(),
        CompletionStatus::Offered { turn }
    );
}

#[test]
fn uncertain_drop_never_requeues_even_if_withdrawn_or_actually_committed() {
    for committed in [false, true] {
        let root = TestDir::new();
        let session = root.session();
        let inbox = CompletionInbox::new(session.id());
        let permit = queued(&inbox);
        let mut batch = inbox.batch(&session, None, false).unwrap();
        batch.begin();
        if committed {
            store(&session, &batch).unwrap();
        }
        inbox.close(CompletionRevocation::Closed);
        drop(batch);
        assert_eq!(permit.receipt().snapshot(), CompletionStatus::Unresolved);
        permit.offer(notice());
        permit.withdraw();
        inbox.revoke(CompletionRevocation::Stopped);
        assert_eq!(permit.receipt().snapshot(), CompletionStatus::Unresolved);
        assert!(!inbox.has_wake());
        assert!(
            inbox
                .batch(&session, None, true)
                .unwrap()
                .notices()
                .is_empty()
        );
        assert_eq!(session.view().unwrap().entries.is_empty(), !committed);
    }
}

#[test]
fn claims_remain_bounded_after_revoke_and_release_capacity_on_resolution() {
    let root = TestDir::new();
    let session = root.session();
    for outcome in ["commit", "reject", "uncertain"] {
        let inbox = CompletionInbox::new(session.id());
        let permits = (0..64).map(|_| queued(&inbox)).collect::<Vec<_>>();
        let mut batch = inbox.batch(&session, None, false).unwrap();
        batch.begin();
        // Repeated revocation cannot free claims for unbounded new reservations.
        for _ in 0..3 {
            inbox.revoke(CompletionRevocation::Stopped);
            assert!(inbox.reserve(0, None, CompletionDelivery::Wake).is_err());
        }
        let expected = match outcome {
            "commit" => {
                let turn = store(&session, &batch).unwrap();
                batch.commit(turn);
                CompletionStatus::Offered { turn }
            }
            "reject" => {
                batch.reject();
                CompletionStatus::Revoked {
                    reason: CompletionRevocation::Stopped,
                }
            }
            "uncertain" => {
                drop(batch);
                CompletionStatus::Unresolved
            }
            _ => unreachable!(),
        };
        for permit in permits {
            assert_eq!(permit.receipt().snapshot(), expected);
        }
        let fresh = (0..64).map(|_| queued(&inbox)).collect::<Vec<_>>();
        assert!(inbox.reserve(0, None, CompletionDelivery::Wake).is_err());
        drop(fresh);
    }
}

#[test]
fn batch_does_not_pin_session_writer() {
    let root = TestDir::new();
    let session = root.session();
    let path = session.path().to_path_buf();
    let inbox = CompletionInbox::new(session.id());
    let permit = queued(&inbox);
    let mut batch = inbox.batch(&session, None, false).unwrap();
    drop(session);
    let reopened = Session::open(path).expect("claim must not keep the writer open");
    batch.begin();
    let turn = store(&reopened, &batch).unwrap();
    batch.commit(turn);
    assert_eq!(
        permit.receipt().snapshot(),
        CompletionStatus::Offered { turn }
    );
}

#[test]
fn signal_wakers_can_read_inbox_without_locking_the_publisher() {
    use std::{
        future::Future,
        task::{Context, Poll, Wake, Waker},
    };

    struct ReadOnWake {
        signal: CompletionSignal,
        done: mpsc::Sender<u64>,
    }
    impl Wake for ReadOnWake {
        fn wake(self: Arc<Self>) {
            self.done.send(self.signal.revision()).unwrap();
        }
    }
    let inbox = CompletionInbox::new(Uuid::now_v7());
    let permit = inbox.reserve(0, None, CompletionDelivery::Wake).unwrap();
    let signal = inbox.signal();
    let previous = signal.revision();
    let mut changed = std::pin::pin!(signal.changed(previous));
    let (done, completed) = mpsc::channel();
    let waker = Waker::from(Arc::new(ReadOnWake {
        signal: signal.clone(),
        done,
    }));
    let mut context = Context::from_waker(&waker);
    assert!(changed.as_mut().poll(&mut context).is_pending());
    let publisher = std::thread::spawn(move || {
        permit.offer(notice());
        permit
    });
    let revision = completed
        .recv_timeout(Duration::from_secs(2))
        .expect("signal waker could not read inbox during publication");
    let _permit = publisher.join().unwrap();
    assert_ne!(revision, previous);
    assert_eq!(changed.as_mut().poll(&mut context), Poll::Ready(revision));
}

#[tokio::test]
async fn signal_tracks_claim_release_withdrawal_and_resolution() {
    tokio::time::timeout(Duration::from_secs(2), async {
        signal_claim_lifecycle().await;
    })
    .await
    .expect("completion signal missed an eligibility change");
}

async fn signal_claim_lifecycle() {
    let root = TestDir::new();
    let session = root.session();
    let inbox = CompletionInbox::new(session.id());
    let signal = inbox.signal();
    let mut previous = signal.revision();
    let permit = queued(&inbox);
    previous = signal.changed(previous).await;
    let batch = inbox.batch(&session, None, false).unwrap();
    previous = signal.changed(previous).await;
    assert!(!inbox.has_wake());
    batch.reject();
    previous = signal.changed(previous).await;
    assert!(inbox.has_wake());
    let mut batch = inbox.batch(&session, None, false).unwrap();
    previous = signal.changed(previous).await;
    batch.begin();
    permit.withdraw();
    previous = signal.changed(previous).await;
    assert_eq!(permit.receipt().snapshot(), CompletionStatus::Queued);
    drop(batch);
    assert_ne!(signal.changed(previous).await, previous);
    assert_eq!(permit.receipt().snapshot(), CompletionStatus::Unresolved);
}
