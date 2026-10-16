use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use ion_ai::{
    BoxFuture, Content, Message, ModelRef, ModelResponse, ModelStreamEvent, ResponseTermination,
    Role, Script, ScriptedModelService, ToolCall, ToolSpec, Usage,
};
use ion_core::{
    AuthorityScope, CodeLimits, CodingAgent, CodingAgentError, CodingSession, CodingToolOutput,
    CodingToolSource, DelegationOutcome, ToolDefinition, ToolExecutor, ToolInvocation,
    ToolRegistration, ToolSet, TurnEndReason,
};
use ion_host::{
    Application, ApplicationRuntime, ControlUpdate, OperationOutcome, Resources, SessionCatalog,
    SessionControl, SessionDelegation, SessionSetup,
};
use serde_json::json;
use tokio::sync::{Notify, watch};

#[path = "foreground_delegation/background.rs"]
mod background;

struct App {
    agent: Arc<CodingAgent>,
    instructions: &'static str,
}
impl Application for App {
    fn resources(&self, _: Option<&std::path::Path>) -> anyhow::Result<Resources> {
        Ok(Resources::new(self.instructions.into()))
    }
    fn runtime(
        &self,
        _: Option<&std::path::Path>,
        _: Option<&ModelRef>,
    ) -> anyhow::Result<ApplicationRuntime> {
        Ok(ApplicationRuntime::new(self.agent.clone()))
    }
}
struct Effect {
    parent: Arc<CodingSession>,
    calls: AtomicUsize,
    started: watch::Sender<bool>,
    stopped: watch::Sender<bool>,
    release: Notify,
    block: bool,
}
impl CodingToolSource for Effect {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(definition("effect"), self)]
    }
}
struct DeferredChildTools(Arc<Effect>);
impl CodingToolSource for DeferredChildTools {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(
            definition("effect").deferred(),
            self.0.clone(),
        )]
    }
}
impl ToolExecutor for Effect {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, CodingToolOutput> {
        let origin = invocation.origin().unwrap();
        // Count forbidden follow-on parent execution too. A child-only assertion
        // here could panic before recording that effect and mask a gateway bug.
        self.calls.fetch_add(1, Ordering::SeqCst);
        if origin.session() != self.parent.id() {
            let links = self.parent.view().unwrap().delegations();
            let record = &links.last().unwrap().record;
            assert_eq!(record.child, origin.session());
            assert_eq!(
                CodingSession::inspect_id(&record.path).unwrap(),
                record.child
            );
            assert!(links.last().unwrap().outcome.is_none());
        }
        self.started.send_replace(true);
        let stop = invocation.stop().clone();
        Box::pin(async move {
            if self.block {
                tokio::select! {
                    () = self.release.notified() => {},
                    () = stop.cancelled() => {
                        self.stopped.send_replace(true);
                        self.release.notified().await;
                    }
                }
            }
            output(json!({"count": "é🦀".chars().count()}))
        })
    }
}
#[derive(Clone, Copy)]
enum Wait {
    Join,
    Abandon,
    Repeat,
    Panic,
    HoldAfterTransfer,
    StopBeforeTransfer,
    CloseBeforeTransfer,
}
struct Delegate {
    child: SessionDelegation,
    owner: Option<Arc<ion_host::BackgroundOwner>>,
    effect: Arc<Effect>,
    wait: Wait,
}
impl CodingToolSource for Delegate {
    fn registrations(self: Arc<Self>) -> Vec<ToolRegistration> {
        vec![ToolRegistration::new(definition("delegate"), self)]
    }
}
impl ToolExecutor for Delegate {
    fn execute<'a>(&'a self, invocation: ToolInvocation<'a>) -> BoxFuture<'a, CodingToolOutput> {
        Box::pin(async move {
            let task = || {
                let input = Message::user_input("count two characters".into(), []);
                let task = if matches!(self.wait, Wait::Panic) {
                    ion_core::DelegationTask::new(
                        ion_core::DelegationSpec {
                            context: self.child.context,
                            directory: self.child.directory.clone(),
                            cwd: None,
                            model: model("child"),
                            reasoning: self.child.reasoning,
                            authority: self.child.authority.clone(),
                            input,
                        },
                        |_, _, _| Box::pin(async { panic!("injected child supervisor panic") }),
                    )
                    .unwrap()
                } else {
                    self.child.prepare(input).unwrap()
                };
                match &self.owner {
                    Some(owner) => {
                        let permit = owner.destination();
                        match self.wait {
                            Wait::StopBeforeTransfer => {
                                task.in_background(background::BeforeTransfer {
                                    permit,
                                    stop: Some(invocation.stop().clone()),
                                    close: None,
                                })
                            }
                            Wait::CloseBeforeTransfer => {
                                task.in_background(background::BeforeTransfer {
                                    permit,
                                    stop: None,
                                    close: Some(owner.clone()),
                                })
                            }
                            _ => task.in_background(permit),
                        }
                    }
                    None => task,
                }
            };
            let result = match self.wait {
                Wait::Abandon => {
                    let mut started = self.effect.started.subscribe();
                    tokio::select! {
                        result = invocation.delegate(task()) => result,
                        _ = started.wait_for(|started| *started) => return output(json!("wait abandoned")),
                    }
                }
                Wait::Join
                | Wait::Repeat
                | Wait::Panic
                | Wait::HoldAfterTransfer
                | Wait::StopBeforeTransfer
                | Wait::CloseBeforeTransfer => invocation.delegate(task()).await,
            };
            if let Ok(receipt) = result {
                if matches!(self.wait, Wait::HoldAfterTransfer) {
                    invocation.stop().cancelled().await;
                }
                if matches!(self.wait, Wait::Repeat) {
                    assert_eq!(invocation.delegate(task()).await.unwrap(), receipt);
                    let different = self
                        .child
                        .prepare(Message::user_input("different input".into(), []))
                        .unwrap();
                    assert!(invocation.delegate(different).await.is_err());
                    if self.owner.is_some() {
                        let other = ion_host::BackgroundOwner::new(1).unwrap();
                        let changed_owner = self
                            .child
                            .prepare(Message::user_input("count two characters".into(), []))
                            .unwrap()
                            .in_background(other.destination());
                        assert!(invocation.delegate(changed_owner).await.is_err());
                    }
                }
                output(json!({"child":receipt.record.child, "outcome":receipt.outcome}))
            } else {
                // Deliberately catch infrastructure errors. Core must still
                // refuse consumption/continuation, including through Code Mode.
                output(json!("caught tool error"))
            }
        })
    }
}
fn definition(name: &str) -> ToolDefinition {
    ToolDefinition::external(ToolSpec {
        name: name.into(),
        description: name.into(),
        input_schema: json!({"type":"object"}),
    })
}
fn output(value: serde_json::Value) -> CodingToolOutput {
    CodingToolOutput {
        value,
        images: Vec::new(),
        is_error: false,
    }
}
fn response(content: Vec<Content>) -> Script {
    Script::Stream(vec![ModelStreamEvent::Completed(ModelResponse {
        message: Message {
            role: Role::Assistant,
            content,
            provider_replay: None,
        },
        usage: Usage::unknown(),
        termination: ResponseTermination::Completed,
        returned_model: None,
    })])
}
fn call(name: &str, arguments: serde_json::Value) -> Content {
    Content::ToolCall(ToolCall {
        id: "same".into(),
        name: name.into(),
        arguments,
        raw_arguments: None,
    })
}
fn model(name: &str) -> ModelRef {
    ModelRef {
        provider: "synthetic".into(),
        model: name.into(),
    }
}
struct Fixture {
    root: PathBuf,
    session: Arc<CodingSession>,
    parent: Arc<ScriptedModelService>,
    child: Arc<ScriptedModelService>,
    effect: Arc<Effect>,
    control: SessionControl,
}
impl Fixture {
    fn new(wait: Wait, block: bool, composed: bool) -> Self {
        Self::with_context(wait, block, composed, ion_core::DelegationContext::Fresh)
    }
    fn with_context(
        wait: Wait,
        block: bool,
        composed: bool,
        context: ion_core::DelegationContext,
    ) -> Self {
        Self::with_owner(wait, block, composed, context, None)
    }
    fn with_owner(
        wait: Wait,
        block: bool,
        composed: bool,
        context: ion_core::DelegationContext,
        owner: Option<Arc<ion_host::BackgroundOwner>>,
    ) -> Self {
        let root = std::env::temp_dir().join(format!("ion-foreground-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let session = Arc::new(CodingSession::create(root.join("parent.sqlite"), None).unwrap());
        let effect = Arc::new(Effect {
            parent: session.clone(),
            calls: AtomicUsize::new(0),
            started: watch::channel(false).0,
            stopped: watch::channel(false).0,
            release: Notify::new(),
            block,
        });
        let child = Arc::new(ScriptedModelService::new([
            response(vec![call("effect", json!({}))]),
            response(vec![Content::Text("two".into())]),
        ]));
        let child_agent = Arc::new(CodingAgent::with_tool_set(
            child.clone(),
            Arc::new(ToolSet::new([
                if matches!(context, ion_core::DelegationContext::Fork { .. }) {
                    Arc::new(DeferredChildTools(effect.clone())) as Arc<dyn CodingToolSource>
                } else {
                    effect.clone() as Arc<dyn CodingToolSource>
                },
            ])),
            model("child"),
        ));
        let background = owner.is_some();
        let delegate = Arc::new(Delegate {
            child: SessionDelegation {
                context,
                directory: root.join("children"),
                cwd: None,
                application: Arc::new(App {
                    agent: child_agent,
                    instructions: "child instructions",
                }),
                model: None,
                reasoning: ion_ai::Reasoning::ProviderDefault,
                authority: AuthorityScope::new("fixture-child/v1").unwrap(),
            },
            owner,
            effect: effect.clone(),
            wait,
        });
        let first = if composed {
            call(
                "code_mode",
                json!({"code":"try { await tools.delegate({}); } catch {} return await tools.effect({});"}),
            )
        } else {
            call("delegate", json!({"parent":"forged"}))
        };
        let mut scripts = Vec::new();
        if matches!(context, ion_core::DelegationContext::Fork { .. }) {
            scripts.push(response(vec![Content::Text("historical answer".into())]));
        }
        scripts.extend([
            response(vec![first]),
            response(vec![Content::Text("parent done".into())]),
        ]);
        if background {
            scripts.push(response(vec![Content::Text("later parent work".into())]));
        }
        let parent = Arc::new(ScriptedModelService::new(scripts));
        let tools = ToolSet::new([
            delegate as Arc<dyn CodingToolSource>,
            effect.clone() as Arc<dyn CodingToolSource>,
        ]);
        let tools = if composed {
            tools.with_code_mode(
                Arc::new(ion_host::code_mode::QuickJs),
                CodeLimits::default(),
            )
        } else {
            tools
        };
        let agent = Arc::new(CodingAgent::with_tool_set(
            parent.clone(),
            Arc::new(tools),
            model("parent"),
        ));
        let control = SessionSetup::new(
            session.clone(),
            SessionCatalog::new(root.clone(), None),
            Arc::new(App {
                agent,
                instructions: "parent instructions",
            }),
            None,
            None,
        )
        .unwrap()
        .into_control();
        Self {
            root,
            session,
            parent,
            child,
            effect,
            control,
        }
    }
    fn start(&mut self) {
        self.control
            .submit_request(
                &ion_core::RequestKey::new("fixture".into(), "parent".into()).unwrap(),
                Message::user_input("delegate".into(), []),
                (),
            )
            .unwrap();
    }
    async fn settle(&mut self) -> ion_host::ShutdownReport<()> {
        let joined = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while self.control.is_busy() {
                self.control.maintain().await;
            }
        })
        .await;
        self.effect.release.notify_one();
        let report = self.control.shutdown().await;
        joined.unwrap();
        report
    }
    fn trigger(&self, kind: &str) {
        rusqlite::Connection::open(self.session.path()).unwrap().execute_batch(&format!(
            "CREATE TRIGGER fail_delegation BEFORE INSERT ON entries WHEN json_extract(NEW.body,'$.kind') = '{kind}' BEGIN SELECT RAISE(FAIL, 'injected delegation fault'); END;"
        )).unwrap();
    }
}

#[tokio::test]
async fn fresh_foreground_child_reconciles_without_inheriting_parent_configuration() {
    let mut f = Fixture::new(Wait::Repeat, false, false);
    f.start();
    let report = f.settle().await;
    assert!(report.cleanup_errors.is_empty());
    let view = f.session.view().unwrap();
    let links = view.delegations();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].record.site.session, f.session.id());
    assert_eq!(links[0].record.site.turn, 1);
    assert_eq!(links[0].record.site.composed_child, None);
    assert_eq!(f.effect.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.parent.requests().len(), 2);
    assert_eq!(f.child.requests().len(), 2);
    assert_eq!(view.last_end, Some((1, TurnEndReason::Completed)));
    assert!(
        matches!(&links[0].outcome, Some(DelegationOutcome::Joined { receipt: Some(receipt), error: None })
        if receipt.session == links[0].record.child && receipt.end == Some(TurnEndReason::Completed))
    );
    let child = CodingSession::open(&links[0].record.path).unwrap();
    assert_eq!(
        child.view().unwrap().parent_delegation(),
        Some(&links[0].record)
    );
    assert_eq!(child.cwd(), None);
    let child_request = &f.child.requests()[0];
    assert!(
        !serde_json::to_string(child_request)
            .unwrap()
            .contains("parent instructions")
    );
    assert!(
        serde_json::to_string(child_request)
            .unwrap()
            .contains("child instructions")
    );
    assert_eq!(
        child_request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["effect"]
    );
    let copy = f.session.clone_to(f.root.join("copy.sqlite")).unwrap();
    assert!(copy.view().unwrap().delegations().is_empty());
    let child_copy = child.clone_to(f.root.join("child-copy.sqlite")).unwrap();
    assert!(child_copy.view().unwrap().parent_delegation().is_none());
    drop((child, copy, child_copy));
    let root = f.root.clone();
    drop(f);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn forked_child_shares_only_the_chosen_history_and_current_child_capabilities() {
    for point in [
        ion_core::ForkPoint::AfterTurn(1),
        ion_core::ForkPoint::BeforeTurn(2),
    ] {
        let mut f = Fixture::with_context(
            Wait::Repeat,
            false,
            false,
            ion_core::DelegationContext::Fork { point },
        );
        let source_key = ion_core::RequestKey::new("source".into(), "history".into()).unwrap();
        f.control
            .submit_request(
                &source_key,
                Message::user_input("historical input marker".into(), []),
                (),
            )
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while f.control.is_busy() {
                f.control.maintain().await;
            }
        })
        .await
        .unwrap();
        let mut settled = false;
        while let Some(update) = f.control.take_update() {
            if let ControlUpdate::Settled(settlement) = update {
                assert!(matches!(settlement.outcome, OperationOutcome::Turn(Ok(_))));
                settled = true;
            }
        }
        assert!(
            settled,
            "transfer the first Turn's reliable result before new work"
        );
        f.start();
        let report = f.settle().await;
        assert!(report.cleanup_errors.is_empty());
        assert_eq!(f.effect.calls.load(Ordering::SeqCst), 1);
        let links = f.session.view().unwrap().delegations();
        assert_eq!(links.len(), 1);
        assert_eq!(
            links[0].record.spec.context,
            ion_core::DelegationContext::Fork { point }
        );
        assert_eq!(links[0].record.site.turn, 2);
        let child = CodingSession::open(&links[0].record.path).unwrap();
        assert_eq!(
            child.view().unwrap().parent_delegation(),
            Some(&links[0].record)
        );
        assert!(child.request_receipt(&source_key).unwrap().is_none());
        assert_eq!(
            child.view().unwrap().last_end,
            Some((2, TurnEndReason::Completed))
        );
        let requests = f.child.requests();
        assert_eq!(requests.len(), 2);
        let request = &requests[0];
        assert_eq!(request.instructions.as_deref(), Some("child instructions"));
        assert_eq!(
            request
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["effect"],
            "only definition-matching tools in the explicit child application may regain exposure"
        );
        let messages = serde_json::to_string(&request.messages).unwrap();
        assert!(
            messages.contains("historical input marker") && messages.contains("historical answer")
        );
        assert!(!messages.contains("forged") && !messages.contains("parent done"));
        assert_eq!(f.parent.requests().len(), 3);
        drop(child);
        let root = f.root.clone();
        drop(f);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[tokio::test]
async fn abandoned_tool_wait_still_joins_child_and_parent_stop_reaches_cleanup() {
    let mut f = Fixture::new(Wait::Abandon, true, false);
    f.start();
    let mut started = f.effect.started.subscribe();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        started.wait_for(|started| *started),
    )
    .await
    .unwrap()
    .unwrap();
    f.control.request_stop();
    let mut stopped = f.effect.stopped.subscribe();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stopped.wait_for(|stopped| *stopped),
    )
    .await
    .unwrap()
    .unwrap();
    let wait = tokio::time::timeout(std::time::Duration::from_millis(20), async {
        while f.control.is_busy() {
            f.control.maintain().await;
        }
    })
    .await;
    let before = f.session.view().unwrap();
    f.effect.release.notify_one();
    let report = f.settle().await;
    assert!(wait.is_err(), "parent settled before child cleanup joined");
    assert_eq!(before.last_end, None);
    assert!(before.delegations()[0].outcome.is_none());
    assert_eq!(f.parent.requests().len(), 1);
    assert_eq!(
        f.session.view().unwrap().last_end,
        Some((1, TurnEndReason::Cancelled))
    );
    assert!(
        report
            .updates
            .iter()
            .any(|update| matches!(update, ControlUpdate::Settled(settlement)
        if matches!(settlement.outcome, OperationOutcome::Turn(Err(CodingAgentError::Cancelled)))))
    );
    let root = f.root.clone();
    drop(f);
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn relationship_faults_cannot_be_caught_to_continue_direct_or_composed_work() {
    for (kind, composed, effects) in [
        ("delegation_admitted", false, 0),
        ("delegation_settled", false, 1),
        ("delegation_settled", true, 1),
    ] {
        let mut f = Fixture::new(Wait::Join, false, composed);
        f.trigger(kind);
        f.start();
        let report = f.settle().await;
        assert!(report.cleanup_errors.is_empty());
        assert_eq!(f.parent.requests().len(), 1);
        assert_eq!(f.effect.calls.load(Ordering::SeqCst), effects);
        assert_eq!(f.session.view().unwrap().last_end, None);
        assert!(report.updates.iter().any(|update| matches!(update, ControlUpdate::Settled(settlement)
            if matches!(settlement.outcome, OperationOutcome::Turn(Err(CodingAgentError::Session(ion_core::CodingSessionError::Sqlite(_))))))));
        let links = f.session.view().unwrap().delegations();
        if effects == 0 {
            assert!(links.is_empty());
            assert!(!f.root.join("children").exists());
        } else {
            assert_eq!(links.len(), 1);
            assert!(links[0].outcome.is_none());
            let child = CodingSession::open(&links[0].record.path).unwrap();
            assert_eq!(
                child.view().unwrap().last_end,
                Some((1, TurnEndReason::Completed))
            );
        }
        let root = f.root.clone();
        drop(f);
        std::fs::remove_dir_all(root).unwrap();
    }
}
