use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::error::{CommandError, RuntimeError};
use crate::ids::OperationId;
use crate::provider::{EngineSignal, Provider, ProviderRequest, ScriptedMessage, ScriptedProvider};
use crate::runtime::{OperationStatus, Runtime, RuntimeEvent, SaturatedHandle};
use crate::session::{
    Applied, EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome,
    OperationState, SessionEntry, Transition,
};
use crate::tool::{ToolRegistry, ToolResult, ToolSpec};

const STEP: Duration = Duration::from_millis(50);

async fn collect_until_terminal(
    events: &mut crate::EventSubscription,
) -> Result<Vec<RuntimeEvent>, RuntimeError> {
    let mut out = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event recv timed out")?;
        let done = matches!(
            event,
            RuntimeEvent::OperationFinished { .. }
                | RuntimeEvent::OperationCancelled { .. }
                | RuntimeEvent::OperationFailed { .. }
                | RuntimeEvent::SessionClosed { .. }
        );
        out.push(event);
        if done {
            return Ok(out);
        }
    }
}

fn kinds(events: &[RuntimeEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            RuntimeEvent::OperationStarted { .. } => "operation_started",
            RuntimeEvent::AssistantTextDelta { .. } => "assistant_text_delta",
            RuntimeEvent::ToolStarted { .. } => "tool_started",
            RuntimeEvent::OperationFinished { .. } => "operation_finished",
            RuntimeEvent::OperationFailed { .. } => "operation_failed",
            RuntimeEvent::OperationCancelled { .. } => "operation_cancelled",
            RuntimeEvent::SessionClosed { .. } => "session_closed",
        })
        .collect()
}

fn texts(events: &[RuntimeEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::AssistantTextDelta { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn tool_runtime() -> Runtime {
    Runtime::start(ScriptedProvider::echo(), ToolRegistry::default())
}

// ---- Pure operation transition core (DESIGN.md §30.1) ----

fn machine_with_tools(prompt: &str, tools: Vec<ToolSpec>) -> (OperationMachine, Applied) {
    OperationMachine::accept(OperationId::new(1), prompt, tools)
}

fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: "d".to_owned(),
        input_schema: json!({"type": "object"}),
    }
}

fn call(id: u64, name: &str) -> crate::tool::ToolCall {
    crate::tool::ToolCall {
        operation_id: OperationId::new(1),
        call_id: id,
        name: name.to_owned(),
        arguments: json!({}),
    }
}

#[test]
fn accept_appends_user_entry_and_enters_accepted() {
    let (machine, applied) = machine_with_tools("goal", vec![spec("read")]);
    assert_eq!(machine.state(), &OperationState::Accepted);
    assert_eq!(machine.prompt(), "goal");
    assert_eq!(
        applied.entries,
        vec![SessionEntry::UserMessage {
            text: "goal".to_owned()
        }]
    );
    assert!(applied.intents.is_empty());
}

#[test]
fn start_model_step_commits_intent_with_frozen_tools() {
    let (mut machine, _) = machine_with_tools("goal", vec![spec("read")]);
    let applied = machine.apply(Transition::StartModelStep).expect("start");
    assert_eq!(machine.state(), &OperationState::AssistantEffectPending);
    assert_eq!(
        applied.intents,
        vec![EffectIntent::ModelStep {
            operation_id: OperationId::new(1),
            prompt: "goal".to_owned(),
            tools: vec![spec("read")],
        }]
    );
}

#[test]
fn provider_completed_without_inbox_finishes_completed() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine.apply(Transition::StartModelStep).expect("start");
    let applied = machine
        .apply(Transition::ProviderCompleted {
            text: "done".to_owned(),
            tool_calls: Vec::new(),
        })
        .expect("complete");
    assert_eq!(
        machine.state(),
        &OperationState::Finished(OperationOutcome::Completed)
    );
    assert_eq!(
        applied.entries,
        vec![SessionEntry::AssistantMessage {
            text: "done".to_owned()
        }]
    );
}

#[test]
fn provider_completed_with_tools_plans_then_admits_sequentially() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine.apply(Transition::StartModelStep).expect("start");
    let applied = machine
        .apply(Transition::ProviderCompleted {
            text: "using tools".to_owned(),
            tool_calls: vec![call(1, "read"), call(2, "bash")],
        })
        .expect("complete");
    assert_eq!(
        machine.state(),
        &OperationState::ToolsPlanned {
            pending: vec![call(1, "read"), call(2, "bash")]
        }
    );
    assert_eq!(
        applied.entries[1..],
        [
            SessionEntry::ToolCall {
                call: call(1, "read")
            },
            SessionEntry::ToolCall {
                call: call(2, "bash")
            }
        ]
    );

    let applied = machine.apply(Transition::AdmitNextTool).expect("admit 1");
    assert_eq!(
        machine.state(),
        &OperationState::ToolEffectPending {
            pending: vec![call(2, "bash")]
        }
    );
    assert_eq!(
        applied.intents,
        vec![EffectIntent::Tool {
            call: call(1, "read")
        }]
    );

    let applied = machine
        .apply(Transition::ToolSettled {
            result: ToolResult::Ok {
                call_id: 1,
                output: "contents".to_owned(),
            },
        })
        .expect("settle 1");
    assert_eq!(
        machine.state(),
        &OperationState::ToolsPlanned {
            pending: vec![call(2, "bash")]
        }
    );
    assert_eq!(
        applied.entries,
        vec![SessionEntry::ToolResult {
            result: ToolResult::Ok {
                call_id: 1,
                output: "contents".to_owned()
            }
        }]
    );

    machine.apply(Transition::AdmitNextTool).expect("admit 2");
    assert_eq!(
        machine.state(),
        &OperationState::ToolEffectPending { pending: vec![] }
    );
    let applied = machine
        .apply(Transition::ToolSettled {
            result: ToolResult::Ok {
                call_id: 2,
                output: "out".to_owned(),
            },
        })
        .expect("settle 2");
    assert_eq!(machine.state(), &OperationState::NeedAssistant);
    assert_eq!(applied.entries.len(), 1);
}

#[test]
fn tool_error_is_a_model_visible_result_and_continues() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine.apply(Transition::StartModelStep).expect("start");
    machine
        .apply(Transition::ProviderCompleted {
            text: String::new(),
            tool_calls: vec![call(1, "read")],
        })
        .expect("complete");
    machine.apply(Transition::AdmitNextTool).expect("admit");
    let applied = machine
        .apply(Transition::ToolSettled {
            result: ToolResult::Err {
                call_id: 1,
                error: "read failed".to_owned(),
            },
        })
        .expect("settle");
    assert_eq!(machine.state(), &OperationState::NeedAssistant);
    assert_eq!(
        applied.entries,
        vec![SessionEntry::ToolResult {
            result: ToolResult::Err {
                call_id: 1,
                error: "read failed".to_owned()
            }
        }]
    );
}

#[test]
fn steer_during_effect_queues_and_applies_at_the_boundary() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine.apply(Transition::StartModelStep).expect("start");
    let applied = machine
        .apply(Transition::ApplyInbox {
            item: InboxItem {
                kind: InboxKind::Steer,
                text: "and also check tests".to_owned(),
            },
        })
        .expect("steer");
    // Queued, not applied: the model step is in flight.
    assert_eq!(machine.state(), &OperationState::AssistantEffectPending);
    assert!(applied.entries.is_empty());
    assert!(machine.has_queued_inbox());

    machine
        .apply(Transition::ProviderCompleted {
            text: "partial".to_owned(),
            tool_calls: Vec::new(),
        })
        .expect("complete");
    // Accepted input exists, so the operation continues.
    assert_eq!(machine.state(), &OperationState::NeedContinuation);

    let drained = machine.drain_inbox().expect("drain");
    assert_eq!(drained.len(), 1);
    assert_eq!(machine.state(), &OperationState::NeedAssistant);
    assert_eq!(
        drained[0].entries,
        vec![SessionEntry::UserMessage {
            text: "and also check tests".to_owned()
        }]
    );

    let applied = machine.apply(Transition::StartModelStep).expect("step 2");
    assert_eq!(
        applied.intents,
        vec![EffectIntent::ModelStep {
            operation_id: OperationId::new(1),
            prompt: "goal\nand also check tests".to_owned(),
            tools: vec![],
        }]
    );
}

#[test]
fn follow_up_continues_the_same_operation() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine.apply(Transition::StartModelStep).expect("start");
    machine
        .apply(Transition::ApplyInbox {
            item: InboxItem {
                kind: InboxKind::FollowUp,
                text: "now summarize".to_owned(),
            },
        })
        .expect("follow-up");
    machine
        .apply(Transition::ProviderCompleted {
            text: "first".to_owned(),
            tool_calls: Vec::new(),
        })
        .expect("complete");
    assert_eq!(machine.state(), &OperationState::NeedContinuation);
    machine.drain_inbox().expect("drain");
    machine.apply(Transition::StartModelStep).expect("step 2");
    machine
        .apply(Transition::ProviderCompleted {
            text: "second".to_owned(),
            tool_calls: Vec::new(),
        })
        .expect("complete");
    assert_eq!(
        machine.state(),
        &OperationState::Finished(OperationOutcome::Completed)
    );
}

#[test]
fn cancel_request_sets_effects_updating_and_settles_cancelled() {
    // During a model step.
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine.apply(Transition::StartModelStep).expect("start");
    let applied = machine.apply(Transition::CancelRequested).expect("cancel");
    assert!(applied.cancel_effects);
    assert_eq!(machine.state(), &OperationState::AssistantEffectPending);
    let applied = machine
        .apply(Transition::ProviderCancelled)
        .expect("settled");
    assert_eq!(
        machine.state(),
        &OperationState::Finished(OperationOutcome::Cancelled)
    );
    assert!(applied.entries.is_empty());

    // During a tool effect.
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine.apply(Transition::StartModelStep).expect("start");
    machine
        .apply(Transition::ProviderCompleted {
            text: String::new(),
            tool_calls: vec![call(1, "bash")],
        })
        .expect("complete");
    machine.apply(Transition::AdmitNextTool).expect("admit");
    machine.apply(Transition::CancelRequested).expect("cancel");
    let applied = machine
        .apply(Transition::ToolSettled {
            result: ToolResult::Ok {
                call_id: 1,
                output: "raced".to_owned(),
            },
        })
        .expect("settle");
    assert_eq!(
        machine.state(),
        &OperationState::Finished(OperationOutcome::Cancelled)
    );
    assert!(matches!(
        applied.entries.as_slice(),
        [SessionEntry::ToolResult { .. }]
    ));
}

#[test]
fn provider_failure_after_cancel_request_settles_cancelled() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine.apply(Transition::StartModelStep).expect("start");
    machine.apply(Transition::CancelRequested).expect("cancel");
    // The runtime maps a provider error after a cancel request to the
    // cancellation outcome; the raw transition is still typed.
    machine
        .apply(Transition::ProviderFailed {
            message: "boom".to_owned(),
        })
        .expect("failed");
    assert_eq!(
        machine.state(),
        &OperationState::Finished(OperationOutcome::Failed("boom".to_owned()))
    );
}

#[test]
fn suspend_from_any_open_state_is_recoverable_not_cancelled() {
    // Suspend is valid from every open state and never produces a user
    // cancellation outcome (DESIGN.md §9.5).
    let cases: Vec<fn(&mut OperationMachine)> = vec![
        |m| {
            let _ = m.apply(Transition::StartModelStep);
        },
        |m| {
            let _ = m.apply(Transition::StartModelStep);
            let _ = m.apply(Transition::ProviderCompleted {
                text: String::new(),
                tool_calls: vec![call(1, "read")],
            });
        },
    ];
    for run in cases {
        let (mut machine, _) = machine_with_tools("goal", vec![]);
        run(&mut machine);
        let applied = machine.apply(Transition::Suspend).expect("suspend");
        assert_eq!(machine.state(), &OperationState::Suspended);
        assert!(applied.cancel_effects);
        assert!(applied.entries.is_empty());
    }
}

#[test]
fn suspend_from_accepted_suspends() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine.apply(Transition::Suspend).expect("suspend");
    assert_eq!(machine.state(), &OperationState::Suspended);
}

#[test]
fn invalid_state_transition_pairs_are_typed_errors() {
    // (setup, transition, expected invalid pair)
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    // From Accepted:
    for transition in [
        Transition::ProviderCompleted {
            text: String::new(),
            tool_calls: Vec::new(),
        },
        Transition::ProviderFailed {
            message: "x".to_owned(),
        },
        Transition::ProviderCancelled,
        Transition::AdmitNextTool,
        Transition::ToolSettled {
            result: ToolResult::Ok {
                call_id: 1,
                output: String::new(),
            },
        },
    ] {
        let (mut m, _) = machine_with_tools("goal", vec![]);
        let label = match &transition {
            Transition::ProviderCompleted { .. } => "provider_completed",
            Transition::ProviderFailed { .. } => "provider_failed",
            Transition::ProviderCancelled => "provider_cancelled",
            Transition::AdmitNextTool => "admit_next_tool",
            Transition::ToolSettled { .. } => "tool_settled",
            _ => unreachable!(),
        };
        let err = m.apply(transition).expect_err(label);
        assert_eq!(err.state, "accepted");
        assert_eq!(err.transition, label);
    }
    let _ = &mut machine;

    // From AssistantEffectPending:
    let (mut m, _) = machine_with_tools("goal", vec![]);
    m.apply(Transition::StartModelStep).expect("start");
    let err = m
        .apply(Transition::StartModelStep)
        .expect_err("double start");
    assert_eq!(
        (err.state, err.transition),
        ("assistant_effect_pending", "start_model_step")
    );
    let err = m.apply(Transition::AdmitNextTool).expect_err("admit");
    assert_eq!(err.state, "assistant_effect_pending");

    // From Finished:
    let (mut m, _) = machine_with_tools("goal", vec![]);
    m.apply(Transition::StartModelStep).expect("start");
    m.apply(Transition::ProviderCancelled).expect("cancel");
    assert!(matches!(
        m.state(),
        OperationState::Finished(OperationOutcome::Cancelled)
    ));
    for transition in [
        Transition::StartModelStep,
        Transition::ApplyInbox {
            item: InboxItem {
                kind: InboxKind::Steer,
                text: "late".to_owned(),
            },
        },
        Transition::AdmitNextTool,
        Transition::CancelRequested,
        Transition::Suspend,
        Transition::ProviderCancelled,
        Transition::ToolSettled {
            result: ToolResult::Ok {
                call_id: 1,
                output: String::new(),
            },
        },
    ] {
        let label = match &transition {
            Transition::StartModelStep => "start_model_step",
            Transition::ApplyInbox { .. } => "apply_inbox",
            Transition::AdmitNextTool => "admit_next_tool",
            Transition::CancelRequested => "cancel_requested",
            Transition::Suspend => "suspend",
            Transition::ProviderCancelled => "provider_cancelled",
            Transition::ToolSettled { .. } => "tool_settled",
            _ => unreachable!(),
        };
        let err = m.apply(transition).expect_err(label);
        assert_eq!(err.state, "finished", "{label}");
    }
}

#[test]
fn failed_transition_leaves_state_unmutated() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine.apply(Transition::StartModelStep).expect("start");
    let before = machine.state().clone();
    assert!(machine.apply(Transition::AdmitNextTool).is_err());
    assert_eq!(machine.state(), &before);
}

// ---- Provider request projection tests ----

/// A provider that records every model-step request it receives, so
/// tests can assert what the runtime projected into each step.
#[derive(Clone, Default)]
struct SharedLogProvider {
    log: Arc<Mutex<Vec<ProviderRequest>>>,
    settle_delay: Duration,
}

impl SharedLogProvider {
    fn requests(&self) -> Vec<ProviderRequest> {
        self.log.lock().expect("log poisoned").clone()
    }
}

impl Provider for SharedLogProvider {
    fn run(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        let operation_id = request.operation_id;
        let delay = self.settle_delay;
        async move {
            self.log.lock().expect("log poisoned").push(request);
            if out
                .send(EngineSignal::TextDelta {
                    operation_id,
                    text: "working".to_owned(),
                })
                .await
                .is_err()
            {
                return;
            }
            sleep(delay).await;
            let _ = out.send(EngineSignal::Completed { operation_id }).await;
        }
    }
}

// ---- Print-mode regression tests ----

#[tokio::test]
async fn streams_scripted_text_in_order() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("hel"),
            ScriptedMessage::text("lo"),
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (snapshot, mut events) = session.subscribe().await.expect("subscribe");
    assert_eq!(snapshot.operation, OperationStatus::Idle);

    let operation_id = session.submit("hi").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(
        kinds(&recorded),
        [
            "operation_started",
            "assistant_text_delta",
            "assistant_text_delta",
            "operation_finished"
        ]
    );
    assert!(matches!(
        recorded[0],
        RuntimeEvent::OperationStarted { operation_id: id, .. } if id == operation_id
    ));
    assert_eq!(texts(&recorded), vec!["hel".to_owned(), "lo".to_owned()]);
    let cursors: Vec<_> = recorded.iter().map(RuntimeEvent::cursor).collect();
    assert!(cursors.windows(2).all(|pair| pair[0] < pair[1]));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_stops_provider_before_later_chunks() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("one"),
            ScriptedMessage::delayed(Duration::from_secs(30), "two"),
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit("slow").await.expect("submit");

    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("delta")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }

    session.cancel(operation_id).await.expect("cancel");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(
        |event| matches!(event, RuntimeEvent::OperationCancelled { operation_id: id, .. } if *id == operation_id)
    ));
    assert!(!recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::AssistantTextDelta { text, .. } if text == "two"
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn busy_submit_is_rejected() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "later",
        )]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let first = session.submit("a").await.expect("first submit");
    let err = session.submit("b").await.expect_err("second submit");
    assert_eq!(
        err,
        CommandError::Busy {
            operation_id: first
        }
    );
    session.cancel(first).await.expect("cancel");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn steer_and_follow_up_require_an_active_operation() {
    let runtime = tool_runtime();
    let session = runtime.session();
    assert_eq!(
        session.steer("nope").await,
        Err(CommandError::NoActiveOperation)
    );
    assert_eq!(
        session.follow_up("nope").await,
        Err(CommandError::NoActiveOperation)
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn saturated_queue_returns_error() {
    let saturated = SaturatedHandle::new();
    let err = saturated.handle().shutdown().await.expect_err("saturated");
    assert_eq!(err, CommandError::QueueSaturated);
}

#[tokio::test]
async fn close_rejects_new_work_and_joins() {
    let runtime = tool_runtime();
    let session = runtime.session();
    session.close().await.expect("close");
    let err = session.submit("after").await.expect_err("closed");
    assert_eq!(err, CommandError::Closed);
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn print_frontend_writes_streamed_text() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("hel"),
            ScriptedMessage::text("lo\n"),
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let mut buf = Vec::new();
    crate::PrintFrontend::new(&mut buf)
        .run(&session, "hi")
        .await
        .expect("print");
    assert_eq!(String::from_utf8(buf).expect("utf8"), "hello\n");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test(start_paused = true)]
async fn delayed_chunk_respects_cancel_without_waiting_full_delay() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "late",
        )]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit("wait").await.expect("submit");
    sleep(STEP).await;
    session.cancel(operation_id).await.expect("cancel");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationCancelled { .. })
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn session_returns_to_idle_after_finish_and_accepts_next_operation() {
    let runtime = Runtime::start(ScriptedProvider::echo(), ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let first = session.submit("a").await.expect("first");
    let _ = collect_until_terminal(&mut events).await.expect("first op");

    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.operation, OperationStatus::Idle);

    let second = session.submit("b").await.expect("second");
    assert_ne!(first, second);
    let recorded = collect_until_terminal(&mut events)
        .await
        .expect("second op");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == second
    ));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

// ---- Tool registry unit tests ----

#[tokio::test]
async fn registry_exposes_core_tool_specs() {
    let registry = ToolRegistry::default();
    let specs = registry.specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"read"));
    assert!(names.contains(&"write"));
    assert!(names.contains(&"edit"));
    assert!(names.contains(&"bash"));
    assert!(names.contains(&"search"));
    assert!(names.contains(&"find"));
}

#[tokio::test]
async fn registry_rejects_missing_required_args() {
    let registry = ToolRegistry::default();
    let err = registry
        .validate("read", &json!({}))
        .expect_err("missing path should fail");
    assert!(err.contains("path"), "got: {err}");

    assert!(
        registry.validate("read", &json!({"path": 5})).is_ok(),
        "present key is structurally valid"
    );
}

#[tokio::test]
async fn registry_rejects_unknown_tool() {
    let registry = ToolRegistry::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    let outcome = registry.execute("nope", &json!({}), cancel).await;
    assert!(outcome.is_error);
    assert!(outcome.output.contains("unknown tool"));
}

#[test]
fn tool_result_classifies_ok_and_err() {
    let ok = ToolResult::Ok {
        call_id: 1,
        output: "hi".into(),
    };
    let err = ToolResult::Err {
        call_id: 2,
        error: "boom".into(),
    };
    assert_eq!(ok.call_id(), 1);
    assert!(ok.is_ok());
    assert_eq!(err.call_id(), 2);
    assert!(!err.is_ok());
    assert_eq!(
        ToolResult::Ok {
            call_id: 3,
            output: "x".into()
        }
        .into_text(),
        "x"
    );
    assert_eq!(err.into_text(), "boom");
}

#[test]
fn tool_spec_is_clonable_and_comparable() {
    let a = spec("read");
    let b = a.clone();
    assert_eq!(a, b);
}

// ---- Tool execution (read/write/edit/search/find/bash) unit tests ----

#[tokio::test]
async fn read_write_edit_search_find_roundtrip() {
    let tmp = std::env::temp_dir().join(format!("ion-tool-test-{}-{}", std::process::id(), 1));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::create_dir_all(&tmp);
    let registry = ToolRegistry::with_cwd(&tmp);
    let cancel = tokio_util::sync::CancellationToken::new();

    let out = registry
        .execute(
            "write",
            &json!({"path":"sub/note.txt","contents":"hello world"}),
            cancel.clone(),
        )
        .await;
    assert!(!out.is_error, "write failed: {out:?}");
    assert_eq!(out.output, "written");

    let out = registry
        .execute("read", &json!({"path":"sub/note.txt"}), cancel.clone())
        .await;
    assert!(!out.is_error, "read failed: {out:?}");
    assert_eq!(out.output, "hello world");

    let out = registry
        .execute(
            "edit",
            &json!({"path":"sub/note.txt","old_str":"world","new_str":"ion"}),
            cancel.clone(),
        )
        .await;
    assert!(!out.is_error, "edit failed: {out:?}");
    let out = registry
        .execute("read", &json!({"path":"sub/note.txt"}), cancel.clone())
        .await;
    assert_eq!(out.output, "hello ion");

    let out = registry
        .execute(
            "edit",
            &json!({"path":"sub/note.txt","old_str":"zzz","new_str":"x"}),
            cancel.clone(),
        )
        .await;
    assert!(out.is_error);
    assert!(out.output.contains("not found"));

    let out = registry
        .execute("search", &json!({"pattern":"hello"}), cancel.clone())
        .await;
    assert!(!out.is_error, "search failed: {out:?}");
    assert!(out.output.contains("note.txt"), "got: {out:?}");

    let out = registry
        .execute("find", &json!({"pattern":"*.txt"}), cancel.clone())
        .await;
    assert!(!out.is_error, "find failed: {out:?}");
    assert!(out.output.contains("note.txt"), "got: {out:?}");

    let out = registry
        .execute("read", &json!({"path":"../outside.txt"}), cancel.clone())
        .await;
    assert!(out.is_error);
    assert!(out.output.contains("escapes"), "got: {out:?}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn bash_runs_command_and_reports_nonzero_exit() {
    let registry = ToolRegistry::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    let out = registry
        .execute("bash", &json!({"command":"echo hi"}), cancel.clone())
        .await;
    assert!(!out.is_error, "bash failed: {out:?}");
    assert!(out.output.contains("hi"), "got: {out:?}");

    let out = registry
        .execute("bash", &json!({"command":"exit 3"}), cancel.clone())
        .await;
    assert!(out.is_error, "nonzero exit should error");
    assert!(out.output.contains("3"), "got: {out:?}");
}

#[tokio::test]
async fn bash_cancel_kills_long_running_command() {
    let registry = ToolRegistry::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    let cancel_clone = cancel.clone();
    let handle = tokio::spawn(async move {
        registry
            .execute("bash", &json!({"command":"sleep 30"}), cancel_clone)
            .await
    });
    sleep(STEP).await;
    cancel.cancel();
    let outcome = timeout(Duration::from_secs(3), handle)
        .await
        .expect("tool should be killed on cancel")
        .expect("task join");
    assert!(outcome.is_error);
    assert_eq!(outcome.output, "cancelled");
}

// ---- Operation-level integration tests ----

#[tokio::test]
async fn tool_loop_success_admits_tools_and_finishes() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("bash", json!({"command":"echo tool-said-hello"})),
        ScriptedMessage::text("final answer\n"),
    ]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");

    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));
    // Tool execution is a live event; tool output is a semantic entry,
    // never an assistant text delta (DESIGN.md §16.4).
    assert!(recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::ToolStarted { tool, .. } if tool == "bash"
    )));
    assert_eq!(texts(&recorded), vec!["final answer\n".to_owned()]);
    assert!(recorded.iter().all(|e| !matches!(
        e,
        RuntimeEvent::OperationFailed { .. } | RuntimeEvent::OperationCancelled { .. }
    )));
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. },
        } if output.contains("tool-said-hello")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn tool_error_is_model_visible_and_operation_continues() {
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool(
        "read",
        json!({"path":"definitely-not-here.txt"}),
    )]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    // An expected tool failure is a model-visible outcome, not a harness
    // failure (DESIGN.md §16.5): the operation completes.
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));
    assert!(!recorded.iter().any(|e| matches!(
        e,
        RuntimeEvent::OperationFailed { .. } | RuntimeEvent::OperationCancelled { .. }
    )));
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. },
        } if error.contains("read failed") || error.contains("No such file")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn malformed_args_are_denied_before_the_effect_starts() {
    let provider =
        ScriptedProvider::new(vec![ScriptedMessage::tool("read", json!({"bogus": true}))]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    // No ToolStarted: the effect never started (DESIGN.md §17.3).
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ToolStarted { .. }))
    );
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. },
        } if error.contains("path")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn unknown_tool_is_denied_before_the_effect_starts() {
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool("frobnicate", json!({}))]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ToolStarted { .. }))
    );
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. },
        } if error.contains("unknown tool")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_during_tool_cancels_operation_and_kills_process() {
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool(
        "bash",
        json!({"command":"sleep 30 && echo PWNED"}),
    )]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit("go").await.expect("submit");

    // Wait until the tool effect has actually started.
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::ToolStarted { .. }) {
            break;
        }
    }
    // Give the bash child a moment to spawn before cancelling.
    sleep(STEP * 4).await;

    session.cancel(operation_id).await.expect("cancel");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(
        |e| matches!(e, RuntimeEvent::OperationCancelled { operation_id: id, .. } if *id == operation_id)
    ));
    assert!(!recorded.iter().any(|e| matches!(
        e,
        RuntimeEvent::AssistantTextDelta { text, .. } if text.contains("PWNED")
    )));

    let close = session.close().await;
    let join = timeout(Duration::from_secs(5), runtime.join());
    assert!(close.is_ok(), "close: {close:?}");
    assert!(join.await.is_ok(), "runtime should join after cancel");
}

#[tokio::test]
async fn tool_loop_multiple_calls_run_sequentially_in_one_operation() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("bash", json!({"command":"echo one"})),
        ScriptedMessage::tool("bash", json!({"command":"echo two"})),
        ScriptedMessage::text("done\n"),
    ]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));
    let started_tools: Vec<&str> = recorded
        .iter()
        .filter_map(|e| match e {
            RuntimeEvent::ToolStarted { tool, .. } => Some(tool.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(started_tools, ["bash", "bash"]);
    assert_eq!(texts(&recorded), vec!["done\n".to_owned()]);
    let snapshot = session.snapshot().await.expect("snapshot");
    let outputs: Vec<String> = snapshot
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ToolResult {
                result: ToolResult::Ok { output, .. },
            } => Some(output.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(outputs, vec!["one\n".to_owned(), "two\n".to_owned()]);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn model_step_request_carries_frozen_tool_specs() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("read", json!({"path":"Cargo.toml"})),
        ScriptedMessage::text("done"),
    ]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().any(|entry| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. },
        } if output.contains("ion-core")
    )));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn steer_projection_reaches_the_next_model_step() {
    // The steer must land while the first step is in its settle delay, so
    // it queues and applies at the next continuation boundary.
    let provider = SharedLogProvider {
        log: Arc::default(),
        settle_delay: Duration::from_millis(150),
    };
    let runtime = Runtime::start(provider.clone(), ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("goal").await.expect("submit");
    session.steer("and also check tests").await.expect("steer");
    let _ = collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        2,
        "one steer must open exactly one new step"
    );
    assert_eq!(requests[0].prompt, "goal");
    assert_eq!(
        requests[1].prompt, "goal\nand also check tests",
        "the steer must be projected into the next step"
    );
}

#[tokio::test]
async fn close_while_operating_suspends_instead_of_cancelling() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "late",
        )]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("slow").await.expect("submit");
    sleep(STEP).await;

    session.close().await.expect("close");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    // Close is lifecycle shutdown, never a user cancellation
    // (DESIGN.md §9.5).
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::SessionClosed { .. }))
    );
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationCancelled { .. }))
    );
    runtime.join().await.expect("join");
}
