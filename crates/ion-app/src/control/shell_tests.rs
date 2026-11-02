use super::*;
use std::sync::{Condvar, Mutex};

struct NativeGate {
    entered: Notify,
    released: Mutex<bool>,
    changed: Condvar,
}
impl NativeGate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            released: Mutex::new(false),
            changed: Condvar::new(),
        })
    }
    fn wait(&self) {
        self.entered.notify_one();
        let (released, _) = self
            .changed
            .wait_timeout_while(
                self.released.lock().unwrap(),
                Duration::from_secs(5),
                |released| !*released,
            )
            .unwrap();
        assert!(*released, "native test gate must be released");
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
struct Release(Arc<NativeGate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}
struct NativeShell {
    gate: Arc<NativeGate>,
    session: Arc<Session>,
    marker: PathBuf,
    panic: bool,
}
impl crate::UserShell for NativeShell {
    fn prepare(&self, _: &str) -> Result<crate::PreparedShell> {
        // Actual filesystem preflight on the retained native leaf.
        assert_eq!(fs::read_to_string(&self.marker)?.as_str(), "not executed");
        self.gate.wait();
        assert!(!self.panic, "injected cold native preflight panic");
        let session = self.session.clone();
        let marker = self.marker.clone();
        Ok(Box::new(move |_, _| {
            Box::pin(async move {
                let view = session.reader().unwrap().view().await.unwrap();
                assert!(view.entries.iter().any(|entry| matches!(entry,
                ion_core::SessionEntry::UserShellAdmitted { command, .. } if command == "literal command"
            )), "Core intent must precede the cold executor");
                tokio::fs::write(marker, "executed").await.unwrap();
                ToolOutput {
                    value: serde_json::json!({"executed": true}),
                    images: Vec::new(),
                    artifacts: Vec::new(),
                    is_error: false,
                }
            })
        }))
    }
}
fn native(fixture: &mut Fixture, panic: bool) -> Release {
    let gate = NativeGate::new();
    let marker = fixture.root.join("native-marker");
    fs::write(&marker, "not executed").unwrap();
    fixture.control.setup.runtime.shell = Some(Arc::new(NativeShell {
        gate: gate.clone(),
        session: fixture.control.session().clone(),
        marker,
        panic,
    }));
    Release(gate)
}

#[tokio::test(flavor = "current_thread")]
async fn native_preflight_is_retained_across_abandoned_next_and_executes_only_after_intent() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    let release = native(&mut fixture, false);
    let before = fixture
        .control
        .session()
        .reader()
        .unwrap()
        .view()
        .await
        .unwrap()
        .entries;
    let metadata = meta("exact Send !Sync metadata");
    metadata.marker.set(29);
    let bytes = serde_json::to_vec(&(Message::user_input("literal command".into(), []), &metadata))
        .unwrap()
        .len();
    fixture.control.budget = InputBudget::new(bytes);
    let operation = fixture
        .control
        .shell("literal command".into(), false, metadata)
        .map_err(|(_, error)| error)
        .unwrap();
    assert_eq!(
        fixture.control.state().operation.unwrap().kind,
        OperationKind::Shell
    );
    assert!(
        fixture
            .control
            .admitting
            .as_ref()
            .unwrap()
            .turn_stop
            .is_none()
    );
    tokio::time::timeout(Duration::from_secs(2), release.0.entered.notified())
        .await
        .unwrap();
    // A current-thread heartbeat and abandoned wait must not block on native I/O.
    assert!(
        tokio::time::timeout(Duration::from_millis(20), fixture.control.next())
            .await
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("native-marker")).unwrap(),
        "not executed"
    );
    assert_eq!(
        fixture
            .control
            .session()
            .reader()
            .unwrap()
            .view()
            .await
            .unwrap()
            .entries,
        before
    );
    release.0.release();
    let (_, settled) = settle(&mut fixture.control).await;
    assert_eq!(settled.operation, operation);
    assert!(matches!(settled.outcome, OperationOutcome::Shell(Ok(_))));
    let metadata = settled.metadata.unwrap();
    assert_eq!(metadata.literal, "exact Send !Sync metadata");
    assert_eq!(metadata.marker.get(), 29);
    assert_eq!(
        fs::read_to_string(fixture.root.join("native-marker")).unwrap(),
        "executed"
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    fixture.control.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn stop_and_abandoned_shutdown_join_the_native_leaf_without_dispatch_or_core_write() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    let release = native(&mut fixture, false);
    let before = fixture
        .control
        .session()
        .reader()
        .unwrap()
        .view()
        .await
        .unwrap()
        .entries;
    fixture
        .control
        .shell("literal command".into(), false, meta("stop exact"))
        .map_err(|(_, error)| error)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), release.0.entered.notified())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), fixture.control.join_shutdown())
            .await
            .is_err()
    );
    assert!(
        fixture.control.admitting.is_some(),
        "stop is not native completion"
    );
    release.0.release();
    fixture.control.join_shutdown().await;
    let (_, settled) = settle(&mut fixture.control).await;
    assert!(settled.stop_requested);
    assert!(matches!(
        settled.outcome,
        OperationOutcome::NotDispatched(_)
    ));
    assert_eq!(settled.metadata.unwrap().literal, "stop exact");
    assert_eq!(
        fs::read_to_string(fixture.root.join("native-marker")).unwrap(),
        "not executed"
    );
    assert_eq!(
        fixture
            .control
            .session()
            .reader()
            .unwrap()
            .view()
            .await
            .unwrap()
            .entries,
        before
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn native_preflight_panic_is_a_reliable_negative_and_retains_result_capacity() {
    let mut fixture = Fixture::new(Model::new(1), false);
    let release = native(&mut fixture, true);
    let before = fixture
        .control
        .session()
        .reader()
        .unwrap()
        .view()
        .await
        .unwrap()
        .entries;
    fixture
        .control
        .shell("literal command".into(), false, meta("panic exact"))
        .map_err(|(_, error)| error)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), release.0.entered.notified())
        .await
        .unwrap();
    release.0.release();
    fixture.control.maintain().await;
    assert!(fixture.control.admitting.is_none());
    assert_eq!(lock(&fixture.control.custody).occupied, 1);
    let snapshot = fixture
        .control
        .snapshot(|metadata| metadata.literal.clone())
        .await
        .unwrap();
    assert!(snapshot.updates.iter().any(|update| matches!(
        update,
        UpdateSnapshot::Settled {
            outcome: OutcomeSnapshot::NotDispatched(_),
            ..
        }
    )));
    let (metadata, _) = fixture
        .control
        .shell("second".into(), false, meta("not retargeted"))
        .unwrap_err();
    assert_eq!(metadata.literal, "not retargeted");
    let (_, settled) = settle(&mut fixture.control).await;
    assert!(
        matches!(settled.outcome, OperationOutcome::NotDispatched(error)
        if error.chain().any(|source| source.is::<tokio::task::JoinError>()))
    );
    assert_eq!(settled.metadata.unwrap().literal, "panic exact");
    assert_eq!(lock(&fixture.control.custody).occupied, 0);
    assert_eq!(
        fixture
            .control
            .session()
            .reader()
            .unwrap()
            .view()
            .await
            .unwrap()
            .entries,
        before
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn cheap_shell_budget_rejection_returns_metadata_without_starting_native_preflight() {
    let mut fixture = Fixture::new(Model::new(1), false);
    let release = native(&mut fixture, true);
    fixture.control.budget = InputBudget::new(1);
    let (metadata, _) = fixture
        .control
        .shell("literal command".into(), false, meta("cheap exact"))
        .unwrap_err();
    assert_eq!(metadata.literal, "cheap exact");
    assert!(fixture.control.admitting.is_none());
    assert_eq!(lock(&fixture.control.custody).occupied, 0);
    drop(release);
    fixture.control.shutdown().await;
}
