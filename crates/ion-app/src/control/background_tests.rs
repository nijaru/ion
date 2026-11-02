use super::*;
use ion_ai::{
    Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    Agent, AuthorityScope, DelegationCompletion, DelegationFailure, DelegationSpec,
    DelegationTarget, DelegationTask, ToolDefinition, ToolExecutor, ToolInvocation, ToolOutput,
    ToolRegistration, ToolSet, ToolSource,
};
use serde_json::json;
use std::{future::Future, sync::atomic::AtomicUsize};

struct WakeProbe {
    shared: Weak<Shared>,
    wakes: AtomicUsize,
    locked: AtomicBool,
}
impl Wake for WakeProbe {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        if let Some(shared) = self.shared.upgrade() {
            self.locked
                .fetch_or(shared.state.try_lock().is_err(), Ordering::SeqCst);
        }
    }
}
impl WakeProbe {
    fn new(owner: &BackgroundOwner) -> Arc<Self> {
        Arc::new(Self {
            shared: Arc::downgrade(&owner.shared),
            wakes: AtomicUsize::new(0),
            locked: AtomicBool::new(false),
        })
    }
    fn assert_woken_unlocked(&self) {
        assert!(
            self.wakes.load(Ordering::SeqCst) > 0,
            "exercise a real registered wake"
        );
        assert!(
            !self.locked.load(Ordering::SeqCst),
            "observer woke under background lifecycle lock"
        );
    }
}
fn armed<'a, T: 'a>(
    future: impl Future<Output = T> + 'a,
    probe: &Arc<WakeProbe>,
) -> Pin<Box<dyn Future<Output = T> + 'a>> {
    let mut future: Pin<Box<dyn Future<Output = T> + 'a>> = Box::pin(future);
    let waker = Waker::from(probe.clone());
    assert!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    future
}

struct DropProbe {
    shared: Weak<Shared>,
    dropped: Arc<AtomicBool>,
    locked: Arc<AtomicBool>,
}
impl Wake for DropProbe {
    fn wake(self: Arc<Self>) {
        if let Some(shared) = self.shared.upgrade() {
            self.locked
                .fetch_or(shared.state.try_lock().is_err(), Ordering::SeqCst);
        }
    }
}
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
        if let Some(shared) = self.shared.upgrade() {
            self.locked
                .fetch_or(shared.state.try_lock().is_err(), Ordering::SeqCst);
        }
    }
}

struct Transfer {
    owner: Arc<BackgroundOwner>,
    target: crate::CompletionTarget,
    directory: std::path::PathBuf,
    release: Arc<Notify>,
}
impl ToolSource for Transfer {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            ToolDefinition::external(ToolSpec {
                name: "transfer".into(),
                description: "fixture".into(),
                input_schema: json!({"type":"object"}),
            }),
            self,
        )]
    }
}
impl ToolExecutor for Transfer {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> ion_ai::BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let release = self.release.clone();
            let task = DelegationTask::new(
                DelegationSpec {
                    target: DelegationTarget::Fresh {
                        directory: self.directory.clone(),
                    },
                    cwd: None,
                    model: ModelRef {
                        provider: "fixture".into(),
                        model: "child".into(),
                    },
                    reasoning: Default::default(),
                    execution_timeout_ms: None,
                    authority: AuthorityScope::new("wake-probe").unwrap(),
                    input: Message::user_input("fixture".into(), []),
                },
                move |_, _, _| {
                    Box::pin(async move {
                        release.notified().await;
                        DelegationCompletion {
                            result: Err(DelegationFailure::Execution(
                                "ordinary joined child failure".into(),
                            )),
                            execution_timed_out: false,
                            elapsed_ms: None,
                            partial_output: None,
                        }
                    })
                },
            )
            .unwrap()
            .in_background(
                self.owner
                    .destination()
                    .with_completion(self.target.clone()),
            );
            invocation.delegate(task).await.unwrap();
            ToolOutput {
                value: json!("transferred"),
                images: Vec::new(),
                artifacts: Vec::new(),
                is_error: false,
            }
        })
    }
}
fn response(content: Content) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![content],
            provider_replay: None,
        },
        usage: Usage::unknown(),
        termination: ResponseTermination::Completed,
        returned_model: None,
    })])
}
struct Root(std::path::PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test]
async fn background_reserve_finish_stop_shutdown_and_ack_wake_only_after_unlock() {
    for shutdown in [false, true] {
        let root = Root(
            std::env::temp_dir().join(format!("ion-background-wake-{}", uuid::Uuid::now_v7())),
        );
        std::fs::create_dir_all(&root.0).unwrap();
        let session = Arc::new(Session::create(root.0.join("parent.sqlite"), None).unwrap());
        let owner = Arc::new(BackgroundOwner::new(1).unwrap());
        let receiver = crate::completion::CompletionReceiver::new(session.id());
        let target = receiver
            .target(0, None, ion_core::CompletionDelivery::Inbox)
            .unwrap();
        let release = Arc::new(Notify::new());
        let transfer = Arc::new(Transfer {
            owner: owner.clone(),
            target: target.clone(),
            directory: root.0.clone(),
            release: release.clone(),
        });
        let model = Arc::new(ScriptedModelService::new([
            response(Content::ToolCall(ToolCall {
                id: "transfer".into(),
                name: "transfer".into(),
                arguments: json!({}),
                raw_arguments: None,
            })),
            response(Content::Text("done".into())),
        ]));
        let agent = Agent::with_tool_set(
            model,
            Arc::new(ToolSet::new([transfer as Arc<dyn ToolSource>])),
            ModelRef {
                provider: "fixture".into(),
                model: "parent".into(),
            },
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            agent.submit(
                session.clone(),
                "start".into(),
                String::new(),
                CancellationToken::new(),
                |_| {},
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let id = owner.snapshot().runs[0].receipt.record.run;
        let dropped = Arc::new(AtomicBool::new(false));
        let locked = Arc::new(AtomicBool::new(false));
        let old = Waker::from(Arc::new(DropProbe {
            shared: Arc::downgrade(&owner.shared),
            dropped: dropped.clone(),
            locked: locked.clone(),
        }));
        let mut maintaining = Box::pin(owner.maintain());
        assert!(
            maintaining
                .as_mut()
                .poll(&mut Context::from_waker(&old))
                .is_pending()
        );
        drop(old);
        assert!(
            maintaining
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "replace a real registered observer"
        );
        assert!(
            !locked.load(Ordering::SeqCst),
            "observer destructor ran under background lifecycle lock"
        );
        drop(maintaining);
        let stop = owner.shared.lock().runs[&id].stop.clone();
        let stop_probe = WakeProbe::new(&owner);
        let _stop_wait = armed(stop.cancelled_owned(), &stop_probe);
        let signal = receiver.inbox().signal();
        let probe = WakeProbe::new(&owner);
        let _changed = armed(signal.changed(signal.revision()), &probe);
        if shutdown {
            owner.request_shutdown(); // token cancellation and permit withdrawal
            stop_probe.assert_woken_unlocked();
            probe.assert_woken_unlocked();
        } else {
            owner.request_stop(id).unwrap();
            stop_probe.assert_woken_unlocked();
            // A transient receiving reservation refused by full source custody
            // must withdraw outside the source lock too.
            let mut refused = owner.destination().with_completion(target);
            assert!(refused.reserve().is_err());
            probe.assert_woken_unlocked();
        }
        let finish_probe = WakeProbe::new(&owner);
        let _finished = armed(owner.shared.changed.notified(), &finish_probe);
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !owner.run(id).unwrap().joined {
                owner.maintain().await;
            }
        })
        .await
        .unwrap();
        finish_probe.assert_woken_unlocked();
        let ack_probe = WakeProbe::new(&owner);
        let _acked = armed(signal.changed(signal.revision()), &ack_probe);
        assert!(owner.take(id).unwrap().is_ok());
        // Shutdown already withdrew the permit; acknowledgement may still
        // invalidate the receiver, but only a live queued permit must wake.
        if !shutdown {
            ack_probe.assert_woken_unlocked();
        }
        owner.join_shutdown().await.unwrap();
        session.shutdown().await.unwrap();
        drop((_stop_wait, _changed, _finished, _acked));
        drop(agent);
        drop(session);
        drop(receiver);
        drop(owner);
    }
}

#[tokio::test]
async fn poisoned_background_state_releases_its_guard_before_closing_and_notifying() {
    let owner = BackgroundOwner::new(1).unwrap();
    let probe = WakeProbe::new(&owner);
    let waiting = armed(owner.shared.stop.clone().cancelled_owned(), &probe);
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = owner.shared.state.lock().unwrap();
            panic!("injected lifecycle poison");
        }))
        .is_err()
    );
    drop(owner.shared.lock());
    probe.assert_woken_unlocked();
    assert!(owner.shared.closed.load(Ordering::SeqCst));
    assert!(owner.check().is_err());
    drop(waiting);
}

#[tokio::test]
async fn repeated_destination_reservation_returns_the_same_cancellation_scope() {
    let owner = BackgroundOwner::new(1).unwrap();
    let mut destination = owner.destination();
    let first = destination.reserve().unwrap();
    let second = destination.reserve().unwrap();
    assert_eq!(owner.snapshot().reserved, 1);
    first.cancel();
    assert!(
        second.is_cancelled(),
        "repeat reservation must not mint a sibling scope"
    );
    drop(destination);
    assert_eq!(owner.snapshot().reserved, 0);
}
