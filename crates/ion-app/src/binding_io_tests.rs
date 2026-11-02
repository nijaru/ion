use super::*;
use crate::{ControlUpdate, Delivery, UserInput};
use ion_ai::{
    Content, Message, ModelResponse, ModelStreamEvent, ResponseTermination, Script,
    ScriptedModelService, Usage,
};
use ion_core::ToolSet;
use std::{
    cell::Cell,
    sync::{Condvar, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

const DEADLINE: Duration = Duration::from_secs(2);

#[test]
fn switch_rejects_followups_before_native_idle_reservation_and_until_result_taken() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let fixture = Fixture::new();
    let target_path = fixture.root.0.join("switch-target.sqlite");
    let target = Session::create(&target_path, None).unwrap();
    let target_id = target.id();
    drop(target);
    let mut control = fixture.setup().into_control::<Cell<u8>>();
    runtime.block_on(async {
        let gate = Gate::new();
        let release = Release(gate.clone());
        let blocked = tokio::task::spawn_blocking(move || gate.wait().unwrap());
        tokio::time::timeout(DEADLINE, release.0.entered.notified())
            .await
            .unwrap();
        let operation = control
            .start_binding(BindingChange::SwitchSession(target_path))
            .unwrap();
        // The only native thread is blocked. Core's idle guard is still free,
        // which previously allowed this input to be retargeted after switch.
        fixture.session.ensure_idle().unwrap();
        let original = Message::user_input("old binding instructions 雪\n".into(), []);
        let reject = |control: &mut crate::SessionControl<Cell<u8>>| {
            let rejected = control
                .submit(
                    Delivery::FollowUp,
                    UserInput::Message(original.clone()),
                    Cell::new(37),
                )
                .unwrap_err();
            assert_eq!(rejected.metadata.get(), 37);
            assert!(matches!(rejected.input, UserInput::Message(message) if message == original));
            assert!(rejected.reason.to_string().contains("binding result"));
        };
        reject(&mut control);
        release.0.release();
        blocked.await.unwrap();
        tokio::time::timeout(DEADLINE, async {
            while control.binding.as_ref().unwrap().is_running() {
                control.maintain().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(control.session().id(), target_id);
        reject(&mut control); // Finished but unread is still a binding fence.
        control
            .join_binding(operation)
            .await
            .unwrap()
            .result
            .unwrap();
        assert!(!control.is_busy());
        let view = control.session().reader().unwrap().view().await.unwrap();
        assert!(
            view.turns().is_empty(),
            "rejected old input must never reach the target journal"
        );
        control.shutdown().await;
    });
}

struct Gate {
    entered: Notify,
    released: Mutex<bool>,
    changed: Condvar,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Notify::new(),
            released: Mutex::new(false),
            changed: Condvar::new(),
        })
    }
    fn wait(&self) -> Result<()> {
        self.entered.notify_one();
        let (released, _) = self
            .changed
            .wait_timeout_while(
                self.released.lock().unwrap(),
                Duration::from_secs(5),
                |released| !*released,
            )
            .unwrap();
        ensure!(*released, "test resource gate was not released");
        Ok(())
    }
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.changed.notify_all();
    }
}
// A failed assertion must not leave a native worker stranded at a test gate.
struct Release(Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}

struct TestApplication {
    resource_gate: Mutex<Option<Arc<Gate>>>,
    instructions: Mutex<String>,
    service: Arc<ScriptedModelService>,
}
impl TestApplication {
    fn new(scripts: impl IntoIterator<Item = Script>) -> Arc<Self> {
        Arc::new(Self {
            resource_gate: Mutex::new(None),
            instructions: Mutex::new("original instructions".into()),
            service: Arc::new(ScriptedModelService::new(scripts)),
        })
    }
    fn gate_resources(&self) -> Release {
        let gate = Gate::new();
        *self.resource_gate.lock().unwrap() = Some(gate.clone());
        Release(gate)
    }
}
fn model(name: &str) -> ModelRef {
    ModelRef {
        provider: "fixture".into(),
        model: name.into(),
    }
}
impl Application for TestApplication {
    fn resources(&self, _cwd: Option<&Path>) -> Result<Resources> {
        let gate = self.resource_gate.lock().unwrap().take();
        if let Some(gate) = gate {
            gate.wait()?;
        }
        Ok(Resources::new(self.instructions.lock().unwrap().clone()))
    }
    fn runtime(
        &self,
        _cwd: Option<&Path>,
        selected: Option<&ModelRef>,
        _questions: Option<Arc<QuestionCustody>>,
    ) -> Result<ApplicationRuntime> {
        Ok(ApplicationRuntime::new(Arc::new(Agent::with_tool_set(
            self.service.clone(),
            Arc::new(ToolSet::new([])),
            selected.cloned().unwrap_or_else(|| model("first")),
        ))))
    }
}

struct TestRoot(PathBuf);
impl Drop for TestRoot {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

struct Fixture {
    catalog: SessionCatalog,
    session: Arc<Session>,
    application: Arc<TestApplication>,
    root: TestRoot,
}
impl Fixture {
    fn new() -> Self {
        Self::with_scripts([])
    }
    fn with_scripts(scripts: impl IntoIterator<Item = Script>) -> Self {
        let root = std::env::temp_dir().join(format!("ion-binding-io-{}", uuid::Uuid::now_v7()));
        let catalog = SessionCatalog::new(root.clone(), None);
        let session = Arc::new(Session::create(catalog.new_path().unwrap(), None).unwrap());
        Self {
            root: TestRoot(root),
            catalog,
            session,
            application: TestApplication::new(scripts),
        }
    }
    fn setup(&self) -> SessionSetup {
        // Deliberate blocking fixture construction, not the live async API.
        SessionSetup::new(
            self.session.clone(),
            self.catalog.clone(),
            self.application.clone(),
            None,
            None,
            None,
        )
        .unwrap()
    }
}
fn prepare_setup(
    session: Arc<Session>,
    catalog: SessionCatalog,
    application: Arc<TestApplication>,
) -> SetupPreparation {
    SetupPreparation::start(CancellationToken::new(), move |_| {
        SessionSetup::new(session, catalog, application, None, None, None)
    })
}

fn assert_send<T: Send>(_: T) {}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_selection_wait_adopts_native_commit_even_after_shutdown() {
    let fixture = Fixture::new();
    // Cell is Send, not Sync. No immutable Controller borrow may cross await.
    let mut control = fixture.setup().into_control::<Cell<u8>>();
    let blocker = rusqlite::Connection::open(fixture.session.path()).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE").unwrap();
    let operation = control
        .start_binding(BindingChange::SelectModel(model("second")))
        .unwrap();
    assert_send(control.join_binding(operation));
    tokio::time::timeout(DEADLINE, async {
        while fixture.session.ensure_idle().is_ok() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("native selection owns the engine gate while SQLite is deferred");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), control.join_binding(operation))
            .await
            .is_err()
    );
    // A real current-thread heartbeat runs while the actual SQLite write waits.
    tokio::time::timeout(DEADLINE, tokio::time::sleep(Duration::from_millis(10)))
        .await
        .unwrap();
    assert_eq!(control.agent().model(), &model("first"));
    assert!(control.start_binding(BindingChange::NewSession).is_err());
    control.request_shutdown();
    blocker.execute_batch("COMMIT").unwrap();
    drop(blocker);
    tokio::time::timeout(DEADLINE, control.join_shutdown())
        .await
        .unwrap();
    // Finished-but-unread custody must not make shutdown or next spin forever.
    assert!(!control.is_busy());
    assert_eq!(control.agent().model(), &model("second"));
    assert_eq!(
        fixture.session.model_preference().unwrap(),
        Some(model("second"))
    );
    assert!(control.start_binding(BindingChange::NewSession).is_err());
    let finished = control.join_binding(operation).await.unwrap();
    assert!(finished.stop_requested);
    assert!(matches!(
        finished.result.unwrap(),
        BindingResult::ModelSelected
    ));
    assert!(control.take_binding_result().is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn abandoned_next_retains_resource_preflight_and_one_finished_update() {
    let fixture = Fixture::new();
    let mut control = fixture.setup().into_control::<()>();
    let release = fixture.application.gate_resources();
    let operation = control
        .start_binding(BindingChange::ReloadResources)
        .unwrap();
    tokio::time::timeout(DEADLINE, release.0.entered.notified())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), control.next())
            .await
            .is_err()
    );
    assert_eq!(control.instructions(), "original instructions");
    *fixture.application.instructions.lock().unwrap() = "reloaded instructions".into();
    assert!(
        control
            .start_binding(BindingChange::ReloadResources)
            .is_err()
    );
    release.0.release();
    match tokio::time::timeout(DEADLINE, control.next())
        .await
        .unwrap()
    {
        ControlUpdate::BindingFinished(finished) => {
            assert_eq!(finished.operation, operation);
            assert!(matches!(
                finished.result.unwrap(),
                BindingResult::ResourcesReloaded
            ));
        }
        _ => panic!("the retained binding result must be delivered"),
    }
    assert_eq!(control.instructions(), "reloaded instructions");
    assert!(control.take_binding_result().is_none());
    assert!(matches!(control.next().await, ControlUpdate::Idle));
    control.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn reload_accepts_active_and_queued_inputs_but_gates_the_next_root() {
    let response = || {
        Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
            message: Message {
                role: ion_ai::Role::Assistant,
                content: vec![Content::Text("done".into())],
                provider_replay: None,
            },
            usage: Usage::unknown(),
            termination: ResponseTermination::Completed,
            returned_model: None,
        })])
    };
    let fixture = Fixture::with_scripts([response(), response(), response()]);
    let mut control = fixture.setup().into_control::<()>();
    let input = |text: &str| UserInput::Message(Message::user_input(text.into(), []));
    control
        .submit_batch(vec![(input("first"), ()), (input("queued"), ())])
        .unwrap();
    let release = fixture.application.gate_resources();
    let operation = control
        .start_binding(BindingChange::ReloadResources)
        .unwrap();
    tokio::time::timeout(DEADLINE, release.0.entered.notified())
        .await
        .unwrap();
    *fixture.application.instructions.lock().unwrap() = "reloaded instructions".into();
    // Reload is valid while a root is active and a follow-up is queued. Joining
    // the active root must not dispatch the next one before binding adoption.
    tokio::time::timeout(DEADLINE, async {
        loop {
            if let ControlUpdate::Settled(settlement) = control.next().await {
                assert!(matches!(
                    settlement.outcome,
                    crate::OperationOutcome::Turn(Ok(_))
                ));
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), async {
            loop {
                match control.next().await {
                    ControlUpdate::Settled(_) | ControlUpdate::Idle => {
                        panic!("queued root must wait for binding preparation")
                    }
                    _ => {}
                }
            }
        })
        .await
        .is_err()
    );
    assert_eq!(fixture.application.service.requests().len(), 1);
    release.0.release();
    let mut binding_finished = false;
    tokio::time::timeout(DEADLINE, async {
        loop {
            match control.next().await {
                ControlUpdate::BindingFinished(finished) => {
                    assert_eq!(finished.operation, operation);
                    finished.result.unwrap();
                    binding_finished = true;
                }
                ControlUpdate::Settled(settlement) => {
                    assert!(matches!(
                        settlement.outcome,
                        crate::OperationOutcome::Turn(Ok(_))
                    ));
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(binding_finished);
    assert_eq!(control.instructions(), "reloaded instructions");
    let requests = fixture.application.service.requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(
            request.instructions.as_deref(),
            Some("original instructions")
        );
    }
    control
        .submit(Delivery::Start, input("new admission"), ())
        .unwrap();
    tokio::time::timeout(DEADLINE, async {
        loop {
            if let ControlUpdate::Settled(settlement) = control.next().await {
                assert!(matches!(
                    settlement.outcome,
                    crate::OperationOutcome::Turn(Ok(_))
                ));
                break;
            }
        }
    })
    .await
    .unwrap();
    let requests = fixture.application.service.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2].instructions.as_deref(),
        Some("reloaded instructions")
    );
    control.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_new_session_preflight_holds_lease_without_blocking_runtime() {
    let fixture = Fixture::new();
    let mut control = fixture.setup().into_control::<()>();
    let original = control.session_id();
    let release = fixture.application.gate_resources();
    let operation = control.start_binding(BindingChange::NewSession).unwrap();
    tokio::time::timeout(DEADLINE, release.0.entered.notified())
        .await
        .unwrap();
    assert!(matches!(
        fixture.session.ensure_idle(),
        Err(ion_core::SessionError::OperationActive)
    ));
    let external = fixture.session.run_user_shell(
        "must not dispatch".into(),
        false,
        CancellationToken::new(),
        |_, _, _| async { panic!("external engine work must be excluded by the native lease") },
    );
    assert!(matches!(
        external.await,
        Err(ion_core::SessionError::OperationActive)
    ));
    assert_eq!(control.session_id(), original);
    control.request_stop();
    release.0.release();
    let finished = tokio::time::timeout(DEADLINE, control.join_binding(operation))
        .await
        .unwrap()
        .unwrap();
    assert!(finished.stop_requested);
    assert!(finished.result.is_err());
    assert_eq!(control.session_id(), original);
    fixture.session.ensure_idle().unwrap();
    control.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn owned_setup_retains_actual_worker_and_writer_lease_across_abandoned_wait() {
    let fixture = Fixture::new();
    let release = fixture.application.gate_resources();
    let mut preparation = prepare_setup(
        fixture.session.clone(),
        fixture.catalog.clone(),
        fixture.application.clone(),
    );
    tokio::time::timeout(DEADLINE, release.0.entered.notified())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(20), preparation.join())
            .await
            .is_err()
    );
    assert!(matches!(
        Session::open(fixture.session.path()),
        Err(ion_core::SessionError::AlreadyOpen)
    ));
    release.0.release();
    let setup = tokio::time::timeout(DEADLINE, preparation.join())
        .await
        .unwrap()
        .unwrap();
    assert!(Arc::ptr_eq(setup.session(), &fixture.session));
    assert_eq!(setup.instructions(), "original instructions");
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_setup_releases_writer_after_actual_leaf_without_registry_cycle() {
    let Fixture {
        root,
        catalog,
        session,
        application,
    } = Fixture::new();
    let path = session.path().to_path_buf();
    let release = application.gate_resources();
    let preparation = prepare_setup(session.clone(), catalog, application);
    tokio::time::timeout(DEADLINE, release.0.entered.notified())
        .await
        .unwrap();
    drop(session);
    drop(preparation);
    assert!(matches!(
        Session::open(&path),
        Err(ion_core::SessionError::AlreadyOpen)
    ));
    release.0.release();
    let reopened = tokio::time::timeout(DEADLINE, async {
        loop {
            let path = path.clone();
            match tokio::task::spawn_blocking(move || Session::open(path))
                .await
                .unwrap()
            {
                Ok(session) => break session,
                Err(ion_core::SessionError::AlreadyOpen) => {
                    tokio::time::sleep(Duration::from_millis(1)).await
                }
                Err(error) => panic!("unexpected reopen failure: {error}"),
            }
        }
    })
    .await
    .expect("abandoned setup must not retain its own Session through a registry cycle");
    drop(reopened);
    drop(root);
}
