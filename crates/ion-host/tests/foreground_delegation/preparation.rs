use super::*;
use ion_host::{DelegationPreparationError, PreparedDelegation};
use std::{path::Path, sync::Mutex, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum Stage {
    Resources,
    Runtime,
}
#[derive(Clone, Copy)]
enum Finish {
    Ready,
    Error,
    Panic,
}
struct HeldApplication {
    inner: Arc<dyn Application>,
    stage: Stage,
    finish: Finish,
    entered: std::sync::mpsc::Sender<()>,
    notify: Arc<Notify>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
    dropped: Arc<Notify>,
    runtime_calls: Arc<AtomicUsize>,
}
impl HeldApplication {
    fn hold(&self) -> anyhow::Result<()> {
        self.entered.send(()).unwrap();
        self.notify.notify_one();
        self.release.lock().unwrap().recv().unwrap();
        match self.finish {
            Finish::Ready => Ok(()),
            Finish::Error => anyhow::bail!("late preparation failure"),
            Finish::Panic => panic!("injected application preflight panic"),
        }
    }
}
impl Drop for HeldApplication {
    fn drop(&mut self) {
        self.dropped.notify_one();
    }
}
impl Application for HeldApplication {
    fn resources(&self, cwd: Option<&Path>) -> anyhow::Result<Resources> {
        if matches!(self.stage, Stage::Resources) {
            self.hold()?;
        }
        self.inner.resources(cwd)
    }
    fn runtime(
        &self,
        cwd: Option<&Path>,
        model: Option<&ModelRef>,
        questions: Option<Arc<ion_host::QuestionCustody>>,
    ) -> anyhow::Result<ApplicationRuntime> {
        self.runtime_calls.fetch_add(1, Ordering::SeqCst);
        if matches!(self.stage, Stage::Runtime) {
            self.hold()?;
        }
        self.inner.runtime(cwd, model, questions)
    }
}
// Release even if a test assertion unwinds: Tokio must never be left waiting
// for a blocking application hook that only this test can release.
struct Release(Option<std::sync::mpsc::Sender<()>>);
impl Drop for Release {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            let _ = release.send(());
        }
    }
}
struct Gate {
    entering: std::sync::mpsc::Receiver<()>,
    notify: Arc<Notify>,
    release: Release,
    dropped: Arc<Notify>,
    runtime_calls: Arc<AtomicUsize>,
}
fn held(fixture: &Fixture, stage: Stage, finish: Finish) -> (SessionDelegation, Gate) {
    let (entered, entering) = std::sync::mpsc::channel();
    let (release, releasing) = std::sync::mpsc::channel();
    let notify = Arc::new(Notify::new());
    let dropped = Arc::new(Notify::new());
    let runtime_calls = Arc::new(AtomicUsize::new(0));
    let recipe = SessionDelegation {
        application: Arc::new(HeldApplication {
            inner: fixture.delegate.child.application.clone(),
            stage,
            finish,
            entered,
            notify: notify.clone(),
            release: Mutex::new(releasing),
            dropped: dropped.clone(),
            runtime_calls: runtime_calls.clone(),
        }),
        ..fixture.delegate.child.clone()
    };
    (
        recipe,
        Gate {
            entering,
            notify,
            release: Release(Some(release)),
            dropped,
            runtime_calls,
        },
    )
}
async fn cleanup(mut fixture: Fixture) {
    assert!(
        !fixture.root.join("children").exists(),
        "preflight allocated a child"
    );
    assert!(fixture.child.requests().is_empty());
    assert!(fixture.parent.requests().is_empty());
    let shutdown = fixture.control.shutdown().await;
    assert!(shutdown.cleanup_errors.is_empty());
    fixture.session.shutdown().await.unwrap();
    let root = fixture.root.clone();
    drop(fixture);
    std::fs::remove_dir_all(root).unwrap();
}
fn error(result: anyhow::Result<PreparedDelegation>) -> anyhow::Error {
    match result {
        Ok(_) => panic!("stopped preflight published a cold task"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn child_resource_preflight_does_not_block_the_control_executor() {
    let fixture = Fixture::new(Wait::Join, false, false);
    let (recipe, gate) = held(&fixture, Stage::Resources, Finish::Ready);
    let (pulse, pulsing) = std::sync::mpsc::channel();
    let heartbeat = tokio::spawn(async move {
        gate.notify.notified().await;
        // A failing implementation may exhaust the monitor's deadlock guard.
        let _ = pulse.send(());
    });
    let monitor = std::thread::spawn(move || {
        gate.entering.recv_timeout(Duration::from_secs(5)).unwrap();
        let responsive = pulsing.recv_timeout(Duration::from_secs(5)).is_ok();
        drop(gate.release);
        responsive
    });
    let prepared = recipe
        .prepare(
            Message::user_input("prepare only".into(), []),
            &CancellationToken::new(),
        )
        .unwrap()
        .join()
        .await
        .unwrap();
    drop(prepared);
    heartbeat.await.unwrap();
    let responsive = monitor.join().unwrap();
    cleanup(fixture).await;
    assert!(
        responsive,
        "child preflight blocked the control executor while loading resources"
    );
}

#[tokio::test]
async fn startup_stop_and_expiry_join_held_hooks_and_preserve_late_failures() {
    // Local stop and parent cancellation stop before runtime; expiry also covers
    // a runtime hook and retains its late failure, not an execution outcome.
    for case in 0..3 {
        let fixture = Fixture::new(Wait::Join, false, false);
        let stage = if case == 2 {
            Stage::Runtime
        } else {
            Stage::Resources
        };
        let finish = if case == 2 {
            Finish::Error
        } else {
            Finish::Ready
        };
        let (mut recipe, gate) = held(&fixture, stage, finish);
        recipe.startup_timeout_ms = (case == 2).then_some(10);
        let parent = CancellationToken::new();
        let mut preparation = recipe
            .prepare(Message::user_input("prepare only".into(), []), &parent)
            .unwrap();
        gate.notify.notified().await;
        match case {
            0 => preparation.request_stop(),
            1 => parent.cancel(),
            _ => {}
        }
        // Cancels the borrowed wait, not its retained worker. Even expiry must
        // not complete until this test's blocking hook has actually returned.
        assert!(
            tokio::time::timeout(Duration::from_millis(30), preparation.join())
                .await
                .is_err()
        );
        drop(gate.release);
        let failure = error(preparation.join().await);
        if case == 2 {
            assert!(
                matches!(failure.downcast_ref::<DelegationPreparationError>(), Some(DelegationPreparationError::TimedOut { timeout_ms: 10, elapsed_ms }) if *elapsed_ms >= 10)
            );
            assert!(
                failure
                    .chain()
                    .any(|cause| cause.to_string() == "late preparation failure")
            );
        } else {
            assert!(matches!(
                failure.downcast_ref::<DelegationPreparationError>(),
                Some(DelegationPreparationError::Cancelled)
            ));
        }
        assert_eq!(
            parent.is_cancelled(),
            case == 1,
            "startup stop cancelled its parent"
        );
        assert_eq!(
            gate.runtime_calls.load(Ordering::SeqCst),
            usize::from(case == 2)
        );
        gate.dropped.notified().await;
        cleanup(fixture).await;
    }
}

#[tokio::test]
async fn dropping_startup_owner_stops_but_does_not_abandon_started_preflight() {
    let fixture = Fixture::new(Wait::Join, false, false);
    let (recipe, gate) = held(&fixture, Stage::Resources, Finish::Ready);
    let parent = CancellationToken::new();
    let preparation = recipe
        .prepare(Message::user_input("prepare only".into(), []), &parent)
        .unwrap();
    gate.notify.notified().await;
    drop(preparation);
    drop(gate.release);
    gate.dropped.notified().await;
    assert!(!parent.is_cancelled());
    assert_eq!(gate.runtime_calls.load(Ordering::SeqCst), 0);
    cleanup(fixture).await;
}

#[tokio::test]
async fn trusted_application_preflight_panic_is_not_a_catchable_startup_refusal() {
    let fixture = Fixture::new(Wait::Join, false, false);
    let (recipe, gate) = held(&fixture, Stage::Resources, Finish::Panic);
    let mut preparation = recipe
        .prepare(
            Message::user_input("prepare only".into(), []),
            &CancellationToken::new(),
        )
        .unwrap();
    let join = tokio::spawn(async move { preparation.join().await });
    gate.notify.notified().await;
    drop(gate.release);
    let Err(failure) = join.await else {
        panic!("trusted application panic was downgraded")
    };
    assert!(failure.is_panic());
    gate.dropped.notified().await;
    cleanup(fixture).await;
}

#[tokio::test]
async fn invalid_budget_and_preexisting_stop_refuse_hooks_before_start() {
    for invalid_budget in [true, false] {
        let fixture = Fixture::new(Wait::Join, false, false);
        let (mut recipe, gate) = held(&fixture, Stage::Resources, Finish::Ready);
        let parent = CancellationToken::new();
        if invalid_budget {
            recipe.startup_timeout_ms = Some(0);
        } else {
            parent.cancel();
        }
        let failure = match recipe.prepare(Message::user_input("prepare only".into(), []), &parent)
        {
            Err(error) => error,
            Ok(mut preparation) => error(
                tokio::time::timeout(Duration::from_secs(5), preparation.join())
                    .await
                    .expect("pre-cancelled startup entered a blocking hook"),
            ),
        };
        if invalid_budget {
            assert!(failure.to_string().contains("must be positive"));
        } else {
            assert!(matches!(
                failure.downcast_ref::<DelegationPreparationError>(),
                Some(DelegationPreparationError::Cancelled)
            ));
        }
        gate.dropped.notified().await;
        assert!(matches!(
            gate.entering.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
        assert_eq!(gate.runtime_calls.load(Ordering::SeqCst), 0);
        cleanup(fixture).await;
    }
}

struct Joined(Notify);
impl std::task::Wake for Joined {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }
}

#[tokio::test]
async fn completed_preflight_respects_local_stop_before_publication() {
    let fixture = Fixture::new(Wait::Join, false, false);
    let (recipe, gate) = held(&fixture, Stage::Resources, Finish::Ready);
    let mut preparation = recipe
        .prepare(
            Message::user_input("prepare only".into(), []),
            &CancellationToken::new(),
        )
        .unwrap();
    gate.notify.notified().await;
    let joined = Arc::new(Joined(Notify::new()));
    {
        // Observe completion without consuming its result: cancelling this
        // borrowed wait leaves its waker registered with the retained task.
        let waker = std::task::Waker::from(joined.clone());
        let mut context = std::task::Context::from_waker(&waker);
        let mut wait = Box::pin(preparation.join());
        assert!(std::future::Future::poll(wait.as_mut(), &mut context).is_pending());
    }
    drop(gate.release);
    joined.0.notified().await;
    preparation.request_stop();
    let failure = preparation.join().await.err();
    let cancelled = matches!(
        failure
            .as_ref()
            .and_then(|error| error.downcast_ref::<DelegationPreparationError>()),
        Some(DelegationPreparationError::Cancelled)
    );
    cleanup(fixture).await;
    assert!(
        cancelled,
        "local stop published an already prepared runnable task"
    );
}

#[tokio::test]
async fn completed_overdue_preflight_is_not_published_after_executor_delay() {
    let fixture = Fixture::new(Wait::Join, false, false);
    let (mut recipe, gate) = held(&fixture, Stage::Resources, Finish::Ready);
    recipe.startup_timeout_ms = Some(10);
    let mut preparation = recipe
        .prepare(
            Message::user_input("prepare only".into(), []),
            &CancellationToken::new(),
        )
        .unwrap();
    gate.notify.notified().await;
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        drop(gate.release);
    });
    // Deliberately starve this current-thread executor across the deadline while
    // blocking preflight finishes late, making completion and expiry both ready.
    std::thread::sleep(Duration::from_millis(50));
    release.join().unwrap();
    let failure = preparation.join().await.err();
    let expired = matches!(
        failure
            .as_ref()
            .and_then(|error| error.downcast_ref::<DelegationPreparationError>()),
        Some(DelegationPreparationError::TimedOut { timeout_ms: 10, .. })
    );
    cleanup(fixture).await;
    assert!(expired, "late preflight escaped its startup deadline");
}
