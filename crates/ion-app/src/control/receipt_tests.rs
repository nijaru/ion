use super::*;

#[tokio::test]
async fn uncertain_input_commit_hands_off_exact_custody_without_claiming_rejection() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    // A real COMMIT error, not a statement rejection. This does not simulate fsync.
    rusqlite::Connection::open(fixture.control.session().path()).unwrap().execute_batch(
        "CREATE TABLE commit_parent (id INTEGER PRIMARY KEY);
         CREATE TABLE commit_child (parent INTEGER REFERENCES commit_parent(id) DEFERRABLE INITIALLY DEFERRED);
         CREATE TRIGGER uncertain_commit AFTER INSERT ON entries
         BEGIN INSERT INTO commit_child(parent) VALUES (999); END;"
    ).unwrap();
    let original = "  exact original 🦀\n";
    let session = fixture.control.session().id();
    let id = fixture
        .control
        .submit(Delivery::Start, prompt(original), meta("exact metadata"))
        .unwrap();
    let (unresolved, settlement) = tokio::time::timeout(Duration::from_secs(5), async {
        let mut unresolved = None;
        loop {
            match fixture.control.next().await {
                ControlUpdate::Unresolved(input) => unresolved = Some(input),
                ControlUpdate::Returned(_) => {
                    panic!("uncertain commit cannot become retryable input")
                }
                ControlUpdate::Accepted { .. } => {
                    panic!("uncertain commit is not a positive receipt")
                }
                ControlUpdate::Settled(settlement) => break (unresolved.unwrap(), settlement),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(unresolved.id, id);
    assert_eq!(unresolved.session, session);
    assert_eq!(unresolved.operation, settlement.operation);
    assert!(
        matches!(unresolved.input, UserInput::Prompt { text, images, files }
        if text == original && images.is_empty() && files.is_empty())
    );
    assert_eq!(unresolved.metadata.literal, "exact metadata");
    assert_eq!(unresolved.metadata.marker.get(), 7);
    assert_eq!(unresolved.receipt.status(), InputStatus::Unresolved);
    assert!(matches!(
        settlement.outcome,
        OperationOutcome::Turn(Err(AgentError::Session(
            SessionError::CommitUncertain { .. }
        )))
    ));
    assert!(settlement.accepted.is_empty());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert!(matches!(
        fixture.control.session().view(),
        Err(SessionError::StoreFaulted)
    ));
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn returning_ready_steering_notifies_watchers_outside_custody_lock() {
    struct Probe {
        custody: std::sync::Weak<Mutex<Custody<Metadata>>>,
        blocked: std::sync::atomic::AtomicBool,
        wakes: AtomicUsize,
    }
    impl std::task::Wake for Probe {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.wakes.fetch_add(1, Ordering::SeqCst);
            if self.custody.upgrade().unwrap().try_lock().is_err() {
                self.blocked.store(true, Ordering::SeqCst);
            }
        }
    }
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture
        .control
        .submit(Delivery::Start, prompt("root"), meta("root"))
        .unwrap();
    model.wait_calls(1).await;
    let probe = Arc::new(Probe {
        custody: Arc::downgrade(&fixture.control.custody),
        blocked: std::sync::atomic::AtomicBool::new(false),
        wakes: AtomicUsize::new(0),
    });
    let waker = std::task::Waker::from(probe.clone());
    let mut receipt = fixture
        .control
        .steering()
        .unwrap()
        .submit_observed(
            Message::user_input("withdraw me".into(), []),
            meta("original"),
            Some(waker.clone()),
            None,
        )
        .unwrap();
    probe.wakes.store(0, Ordering::SeqCst);
    let mut changed = Box::pin(receipt.changed());
    assert!(
        std::future::Future::poll(
            changed.as_mut(),
            &mut std::task::Context::from_waker(&waker)
        )
        .is_pending()
    );
    fixture.control.clear_queue();
    assert!(probe.wakes.load(Ordering::SeqCst) > 0);
    assert!(
        !probe.blocked.load(Ordering::SeqCst),
        "a watcher was woken under the custody mutex"
    );
    drop(changed);
    assert!(matches!(
        receipt.status(),
        SteeringStatus::Returned {
            reason: ReturnSnapshot::Cleared,
            ..
        }
    ));
    // Closing the last Host monitor at settlement is also a notification. A
    // committed receipt must not hide that second reentrancy boundary.
    let mut committed = fixture
        .control
        .steering()
        .unwrap()
        .submit_observed(
            Message::user_input("commit me".into(), []),
            meta("committed"),
            Some(waker.clone()),
            None,
        )
        .unwrap();
    drop(fixture.control.maintain().await);
    model.release();
    model.wait_calls(2).await;
    fixture.control.state();
    for _ in 0..8 {
        let mut changed = Box::pin(committed.changed());
        if std::future::Future::poll(
            changed.as_mut(),
            &mut std::task::Context::from_waker(&waker),
        )
        .is_pending()
        {
            break;
        }
    }
    let mut changed = Box::pin(committed.changed());
    assert!(
        std::future::Future::poll(
            changed.as_mut(),
            &mut std::task::Context::from_waker(&waker)
        )
        .is_pending()
    );
    probe.blocked.store(false, Ordering::SeqCst);
    model.release();
    wait_committed_end(&fixture.control).await;
    settle(&mut fixture.control).await;
    assert!(
        !probe.blocked.load(Ordering::SeqCst),
        "committed monitor closed under custody mutex"
    );
    drop(changed);
    fixture.control.shutdown().await;
}
