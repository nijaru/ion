use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::context::{ContextMessage, ContextPlan};
use crate::error::{CommandError, RuntimeError};
use crate::ids::{EffectId, InboxId, OperationId};
use crate::policy::{AllowlistPolicy, PolicyEngine};
use crate::provider::{EngineSignal, Provider, ProviderRequest, ScriptedMessage, ScriptedProvider};
use crate::runtime::{OperationStatus, Runtime, RuntimeEvent, SaturatedHandle, SessionHandle};
use crate::session::{
    Applied, EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome,
    OperationState, SessionEntry, Transition,
};
use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EntryRecord, InboxRecord, SessionRecord,
    SessionStore,
};
use crate::tool::{RecoveryClass, ToolCall, ToolRegistry, ToolResult, ToolSpec};

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
                | RuntimeEvent::OperationApprovalRequired { .. }
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
            RuntimeEvent::OperationApprovalRequired { .. } => "operation_approval_required",
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

/// Tests that exercise mechanics rather than policy run with every
/// core tool granted; the policy-gate tests construct their own.
fn permissive_policy() -> Arc<dyn PolicyEngine> {
    Arc::new(AllowlistPolicy::new([
        "read", "write", "edit", "bash", "search", "find",
    ]))
}

/// Runtime over an in-memory store; file-backed stores are exercised by
/// the dedicated store tests below.
fn start_runtime(provider: impl crate::Provider, tools: ToolRegistry) -> Runtime {
    let store = SessionStore::open_in_memory().expect("in-memory store");
    start_runtime_with_store(provider, tools, store)
}

fn start_runtime_with_store(
    provider: impl crate::Provider,
    tools: ToolRegistry,
    store: SessionStore,
) -> Runtime {
    Runtime::start_with_policy(provider, tools, store, permissive_policy())
}

fn tool_runtime() -> Runtime {
    start_runtime(ScriptedProvider::echo(), ToolRegistry::default())
}

// ---- Pure operation transition core (DESIGN.md §30.1) ----

fn machine_with_tools(prompt: &str, tools: Vec<ToolSpec>) -> (OperationMachine, Applied) {
    OperationMachine::accept(OperationId::from_uuid(uuid::Uuid::nil()), prompt, tools)
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
        operation_id: OperationId::from_uuid(uuid::Uuid::nil()),
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
    let applied = machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: vec![ContextMessage::User {
                    content: "user: goal".to_owned(),
                }],
            },
        })
        .expect("start");
    assert_eq!(machine.state(), &OperationState::AssistantEffectPending);
    assert_eq!(
        applied.intents,
        vec![EffectIntent::ModelStep {
            operation_id: OperationId::from_uuid(uuid::Uuid::nil()),
            plan: ContextPlan {
                system: String::new(),
                messages: vec![ContextMessage::User {
                    content: "user: goal".to_owned(),
                }],
            },
            tools: vec![spec("read")],
        }]
    );
}

#[test]
fn provider_completed_without_inbox_finishes_completed() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
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
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
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
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
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
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: vec![ContextMessage::User {
                    content: "user: goal".to_owned(),
                }],
            },
        })
        .expect("start");
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

    let drained = machine.drain_steers().expect("steer drain");
    assert_eq!(drained.len(), 1);
    assert_eq!(machine.state(), &OperationState::NeedAssistant);
    assert_eq!(
        drained[0].entries,
        vec![SessionEntry::UserMessage {
            text: "and also check tests".to_owned()
        }]
    );

    let applied = machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: vec![ContextMessage::User {
                    content: "user: goal\nassistant: partial\nuser: and also check tests"
                        .to_owned(),
                }],
            },
        })
        .expect("step 2");
    assert_eq!(
        applied.intents,
        vec![EffectIntent::ModelStep {
            operation_id: OperationId::from_uuid(uuid::Uuid::nil()),
            plan: ContextPlan {
                system: String::new(),
                messages: vec![ContextMessage::User {
                    content: "user: goal\nassistant: partial\nuser: and also check tests"
                        .to_owned(),
                }],
            },
            tools: vec![],
        }]
    );
}

#[test]
fn follow_up_continues_the_same_operation() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
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
    machine.drain_followups().expect("follow-up drain");
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: vec![ContextMessage::User {
                    content: "user: goal\nuser: now summarize".to_owned(),
                }],
            },
        })
        .expect("step 2");
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
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
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
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
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
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
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
            let _ = m.apply(Transition::StartModelStep {
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new(),
                },
            });
        },
        |m| {
            let _ = m.apply(Transition::StartModelStep {
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new(),
                },
            });
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
    m.apply(Transition::StartModelStep {
        plan: ContextPlan {
            system: String::new(),
            messages: Vec::new(),
        },
    })
    .expect("start");
    let err = m
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect_err("double start");
    assert_eq!(
        (err.state, err.transition),
        ("assistant_effect_pending", "start_model_step")
    );
    let err = m.apply(Transition::AdmitNextTool).expect_err("admit");
    assert_eq!(err.state, "assistant_effect_pending");

    // From Finished:
    let (mut m, _) = machine_with_tools("goal", vec![]);
    m.apply(Transition::StartModelStep {
        plan: ContextPlan {
            system: String::new(),
            messages: Vec::new(),
        },
    })
    .expect("start");
    m.apply(Transition::ProviderCancelled).expect("cancel");
    assert!(matches!(
        m.state(),
        OperationState::Finished(OperationOutcome::Cancelled)
    ));
    for transition in [
        Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        },
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
            Transition::StartModelStep { .. } => "start_model_step",
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
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
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
        let step = request.step;
        let delay = self.settle_delay;
        async move {
            self.log.lock().expect("log poisoned").push(request);
            if out
                .send(EngineSignal::TextDelta {
                    operation_id,
                    step,
                    text: "working".to_owned(),
                })
                .await
                .is_err()
            {
                return;
            }
            sleep(delay).await;
            let _ = out
                .send(EngineSignal::Completed { operation_id, step })
                .await;
        }
    }
}

// ---- Print-mode regression tests ----

#[tokio::test]
async fn streams_scripted_text_in_order() {
    let runtime = start_runtime(
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
    let runtime = start_runtime(
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
    let runtime = start_runtime(
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
async fn delayed_chunk_respects_cancel_without_waiting_full_delay() {
    let runtime = start_runtime(
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
    let runtime = start_runtime(ScriptedProvider::echo(), ToolRegistry::default());
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
    let runtime = start_runtime(provider, ToolRegistry::default());
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
    let runtime = start_runtime(provider, ToolRegistry::default());
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
    let runtime = start_runtime(provider, ToolRegistry::default());
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
    let runtime = start_runtime(provider, ToolRegistry::default());
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
    let runtime = start_runtime(provider, ToolRegistry::default());
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
    let runtime = start_runtime(provider, ToolRegistry::default());
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
    let runtime = start_runtime(provider, ToolRegistry::default());
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
    let runtime = start_runtime(provider.clone(), ToolRegistry::default());
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
    assert_eq!(
        requests[0].plan.messages,
        vec![ContextMessage::User {
            content: "goal".to_owned()
        }]
    );
    assert_eq!(
        requests[1].plan.messages,
        vec![
            ContextMessage::User {
                content: "goal".to_owned()
            },
            ContextMessage::Assistant {
                content: "working".to_owned(),
                tool_calls: Vec::new(),
            },
            ContextMessage::User {
                content: "and also check tests".to_owned()
            },
        ],
        "the steer must be projected into the next step's plan"
    );
}

#[tokio::test]
async fn close_while_operating_suspends_instead_of_cancelling() {
    let runtime = start_runtime(
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

// ---- Durable session store (DESIGN.md §32 Step 2) ----

fn temp_db(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ion-store-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("sessions.db")
}

fn entry_kinds(entries: &[(u64, crate::SessionEntry)]) -> Vec<&'static str> {
    entries
        .iter()
        .map(|(_, entry)| match entry {
            crate::SessionEntry::UserMessage { .. } => "user_message",
            crate::SessionEntry::AssistantMessage { .. } => "assistant_message",
            crate::SessionEntry::ToolCall { .. } => "tool_call",
            crate::SessionEntry::ToolResult { .. } => "tool_result",
            crate::SessionEntry::Compaction { .. } => "compaction",
        })
        .collect()
}

#[tokio::test]
async fn restart_reproduces_the_logical_transcript() {
    let db = temp_db("restart");
    let store = SessionStore::open(&db).expect("open store");
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("bash", json!({"command":"echo persisted"})),
        ScriptedMessage::text("final\n"),
    ]);
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store);
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("read it").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    // Reopen the same database: the logical transcript must reproduce.
    let store = SessionStore::open(&db).expect("reopen store");
    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(
        entry_kinds(&loaded.entries),
        [
            "user_message",
            "assistant_message",
            "tool_call",
            "tool_result",
            "assistant_message",
        ]
    );
    assert_eq!(loaded.operations.len(), 1);
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(
        checkpoint.state,
        OperationState::Finished(OperationOutcome::Completed)
    );
    assert!(!checkpoint.cancel_requested);
    assert!(loaded.pending_inbox.is_empty());
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn durable_admission_failure_is_visible_and_non_corrupting() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    // Wait until the session row is committed before injecting.
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    store.fail_next_write();

    let err = session.submit("lost").await.expect_err("submit must fail");
    assert!(matches!(err, CommandError::Persistence(_)));
    // No operation was installed: the session is still idle and usable.
    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.operation, OperationStatus::Idle);
    assert!(snapshot.entries.is_empty());

    let operation_id = session.submit("kept").await.expect("retry succeeds");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn mid_operation_persistence_failure_fails_the_operation_visibly() {
    let store = SessionStore::open_in_memory().expect("store");
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool(
        "bash",
        json!({"command":"sleep 1 && echo slow"}),
    )]);
    let runtime = start_runtime_with_store(provider, ToolRegistry::default(), store.clone());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    // Wait for the tool effect to start, then fail its settlement commit.
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::ToolStarted { .. }) {
            break;
        }
    }
    store.fail_next_write();

    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    let failed = recorded.iter().any(|event| {
        matches!(
            event,
            RuntimeEvent::OperationFailed { message, .. } if message.contains("persistence failed")
        )
    });
    assert!(failed, "persistence failure must be visible: {recorded:?}");
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. }))
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_request_is_durable() {
    let db = temp_db("cancel");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("start"),
            ScriptedMessage::delayed(Duration::from_secs(30), "late"),
        ]),
        ToolRegistry::default(),
        store,
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit("slow").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    session.cancel(operation_id).await.expect("cancel");
    collect_until_terminal(&mut events).await.expect("settle");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    let store = SessionStore::open(&db).expect("reopen");
    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(
        checkpoint.cancel_requested,
        "the cancellation request must be durable"
    );
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn steer_is_durable_as_pending_inbox() {
    let db = temp_db("steer");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("start"),
            ScriptedMessage::delayed(Duration::from_secs(30), "late"),
        ]),
        ToolRegistry::default(),
        store,
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("goal").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    session.steer("and also check tests").await.expect("steer");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    let store = SessionStore::open(&db).expect("reopen");
    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(loaded.pending_inbox.len(), 1);
    assert_eq!(loaded.pending_inbox[0].text, "and also check tests");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[test]
fn recovery_classes_match_the_design() {
    let registry = ToolRegistry::default();
    assert_eq!(registry.recovery_class("read"), RecoveryClass::ReplaySafe);
    assert_eq!(registry.recovery_class("search"), RecoveryClass::ReplaySafe);
    assert_eq!(registry.recovery_class("find"), RecoveryClass::ReplaySafe);
    assert_eq!(registry.recovery_class("write"), RecoveryClass::Reconcile);
    assert_eq!(registry.recovery_class("edit"), RecoveryClass::Reconcile);
    assert_eq!(registry.recovery_class("bash"), RecoveryClass::NeverReplay);
    assert_eq!(
        registry.recovery_class("unknown-tool"),
        RecoveryClass::NeverReplay
    );
}

#[test]
fn fail_operation_lands_from_any_open_state_and_never_from_finished() {
    for setup in [
        |m: &mut OperationMachine| {
            let _ = m.apply(Transition::StartModelStep {
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new(),
                },
            });
        },
        |m: &mut OperationMachine| {
            let _ = m.apply(Transition::StartModelStep {
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new(),
                },
            });
            let _ = m.apply(Transition::ProviderCompleted {
                text: String::new(),
                tool_calls: vec![call(1, "read")],
            });
        },
    ] {
        let (mut machine, _) = machine_with_tools("goal", vec![]);
        setup(&mut machine);
        let applied = machine
            .apply(Transition::FailOperation {
                message: "harness failure".to_owned(),
            })
            .expect("fail from an open state");
        assert_eq!(
            machine.state(),
            &OperationState::Finished(OperationOutcome::Failed("harness failure".to_owned()))
        );
        assert!(applied.cancel_effects);
    }
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
    machine.apply(Transition::ProviderCancelled).expect("done");
    let err = machine
        .apply(Transition::FailOperation {
            message: "late".to_owned(),
        })
        .expect_err("finished is terminal");
    assert_eq!(err.transition, "fail_operation");
}

// ---- Reopen and schema integrity (Codex review blockers) ----

#[tokio::test]
async fn reopen_rebuilds_an_open_operation_and_blocks_new_work() {
    let db = temp_db("reopen");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("start"),
            ScriptedMessage::delayed(Duration::from_secs(30), "late"),
        ]),
        ToolRegistry::default(),
        store,
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("goal").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    // The durable transcript at close time is whatever was committed;
    // a delta that never settled stays an ephemeral draft (§10.3).
    let before = session.snapshot().await.expect("snapshot");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    // Reopen the same session: the suspended operation must be rebuilt.
    let store = SessionStore::open(&db).expect("reopen store");
    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store,
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let snapshot = session.snapshot().await.expect("snapshot");
    let OperationStatus::Active {
        operation_id: reopened_id,
        prompt,
        state,
    } = snapshot.operation
    else {
        panic!("expected the open operation to be rebuilt, got idle");
    };
    assert_eq!(prompt, "goal");
    assert_eq!(state, OperationState::Suspended);
    // Recovery decisions are Step 3 work: an open operation blocks new
    // submits rather than being silently resumed or cancelled.
    let err = session.submit("new").await.expect_err("busy");
    assert!(matches!(err, CommandError::Busy { operation_id } if operation_id == reopened_id));
    // The transcript reproduced exactly what was committed before close.
    assert_eq!(
        entry_kinds(
            &snapshot
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| (i as u64, e.clone()))
                .collect::<Vec<_>>()
        ),
        entry_kinds(
            &before
                .entries
                .iter()
                .enumerate()
                .map(|(i, e)| (i as u64, e.clone()))
                .collect::<Vec<_>>()
        )
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[test]
fn store_refuses_a_database_from_a_newer_schema() {
    let db = temp_db("future");
    {
        let store = SessionStore::open(&db).expect("open");
        let _ = store;
    }
    // Simulate a future Ion bumping the schema version.
    let connection = rusqlite::Connection::open(&db).expect("raw open");
    connection
        .pragma_update(None, "user_version", 99)
        .expect("bump version");
    drop(connection);
    let err = SessionStore::open(&db).expect_err("foreign schema must be refused");
    assert!(err.to_string().contains("does not match"), "got: {err}");

    // A database from an older dev build is refused too: v0 migrates
    // nothing (no compatibility guarantees across builds).
    let connection = rusqlite::Connection::open(&db).expect("raw open");
    connection
        .pragma_update(None, "user_version", 2)
        .expect("stale version");
    drop(connection);
    let err = SessionStore::open(&db).expect_err("stale schema must be refused");
    assert!(err.to_string().contains("does not match"), "got: {err}");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn settlement_must_match_a_pending_effect_of_the_operation() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // A settlement for an unknown or already-settled effect must be
    // rejected, not silently succeed on zero rows.
    let loaded = store.load(session_id).await.expect("load");
    let operation_id = loaded.operations[0].id;
    let ghost = crate::ids::EffectId::generate();
    let err = store
        .commit(CommitRequest {
            session_id,
            operation_id,
            checkpoint: CheckpointRecord {
                state_seq: 999,
                payload: CheckpointPayload {
                    state: OperationState::Finished(OperationOutcome::Completed),
                    cancel_requested: false,
                    prompt: String::new(),
                    tools: Vec::new(),
                    open_effect: None,
                },
            },
            entries: Vec::new(),
            open_effects: Vec::new(),
            settled_effects: vec![crate::store::SettledEffect {
                id: ghost,
                settlement: serde_json::json!({}),
            }],
            indeterminate_effects: Vec::new(),
            inbox: Vec::new(),
            inbox_applied: Vec::new(),
            usage: Vec::new(),
        })
        .await
        .expect_err("ghost settlement must fail");
    assert!(err.to_string().contains("matched no pending effect"));
}

// ---- Crash-window recovery (DESIGN.md §32 Step 3, §30.2) ----

async fn wait_for_state(session: &SessionHandle, predicate: impl Fn(&OperationState) -> bool) {
    for _ in 0..50 {
        let snapshot = session.snapshot().await.expect("snapshot");
        if matches!(
            snapshot.operation,
            OperationStatus::Active { ref state, .. } if predicate(state)
        ) {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("operation never reached the expected state");
}

#[tokio::test]
async fn crash_during_model_step_recovers_by_replay() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "never arrives",
        )]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    session.submit("goal").await.expect("submit");
    wait_for_state(&session, |state| {
        matches!(state, OperationState::AssistantEffectPending)
    })
    .await;

    // Process loss mid-model-step: no close, no settlement.
    runtime.crash();
    drop(runtime);
    drop(session);

    // Reopen: the pending model step replays with a bumped attempt.
    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("recovered\n")]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(|e| matches!(
            e,
            RuntimeEvent::AssistantTextDelta { text, .. } if text == "recovered\n"
        )),
        "the replayed model step must stream: {recorded:?}"
    );
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(
        checkpoint.state,
        OperationState::Finished(OperationOutcome::Completed)
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn crash_during_replayable_tool_recovers_by_reexecution() {
    let dir = std::env::temp_dir().join(format!("ion-crash-read-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("note.txt"), "persisted bytes").expect("write");

    let store = SessionStore::open_in_memory().expect("store");
    let registry = ToolRegistry::with_cwd(&dir);
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "read",
            json!({"path":"note.txt"}),
        )]),
        registry,
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("read it").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::ToolStarted { .. }) {
            break;
        }
    }

    // Process loss while the read effect is in flight.
    runtime.crash();
    drop(runtime);
    drop(session);

    // Reopen: read is ReplaySafe, so it re-executes and the operation
    // continues into the next model step.
    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("after recovery\n")]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(
        checkpoint.state,
        OperationState::Finished(OperationOutcome::Completed)
    );
    assert!(loaded.entries.iter().any(|(_, entry)| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. },
        } if output.contains("persisted bytes")
    )));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn crash_during_bash_settles_indeterminate_and_stays_usable() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "bash",
            json!({"command":"sleep 30"}),
        )]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("run").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::ToolStarted { .. }) {
            break;
        }
    }

    // Process loss while a NeverReplay effect is in flight.
    runtime.crash();
    drop(runtime);
    drop(session);

    // Reopen: bash must NOT re-execute; the operation settles as
    // indeterminate and the session stays usable.
    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.operation, OperationStatus::Idle);

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(
        checkpoint.state,
        OperationState::Finished(OperationOutcome::Indeterminate)
    );
    assert!(
        !loaded
            .entries
            .iter()
            .any(|(_, entry)| matches!(entry, SessionEntry::ToolResult { .. }))
    );

    // The session accepts new work after the indeterminate settlement.
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit("next").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { operation_id: id, .. }) if *id == operation_id
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

// ---- Context projection and usage ledger (DESIGN.md §32 Step 4 slice 1) ----

fn plan_of(entries: &[SessionEntry]) -> crate::context::ContextPlan {
    crate::context::project(entries, 1)
}

#[test]
fn projector_is_deterministic_and_pairs_tool_calls() {
    let entries = vec![
        SessionEntry::UserMessage {
            text: "goal".to_owned(),
        },
        SessionEntry::AssistantMessage {
            text: "reading".to_owned(),
        },
        SessionEntry::ToolCall {
            call: ToolCall {
                operation_id: OperationId::generate(),
                call_id: 7,
                name: "read".to_owned(),
                arguments: serde_json::json!({ "path": "a.txt" }),
            },
        },
        SessionEntry::ToolResult {
            result: ToolResult::Ok {
                call_id: 7,
                output: "contents".to_owned(),
            },
        },
    ];
    let first = plan_of(&entries);
    let second = plan_of(&entries);
    assert_eq!(first, second, "the same entries must project identically");
    assert_eq!(first.messages.len(), 3);
    let crate::context::ContextMessage::Assistant { tool_calls, .. } = &first.messages[1] else {
        panic!("tool call must attach to the assistant message");
    };
    assert_eq!(tool_calls.len(), 1);
    let crate::context::ContextMessage::Tool { call_id, content } = &first.messages[2] else {
        panic!("tool result must project as a tool message");
    };
    assert_eq!((*call_id, content.as_str()), (7, "contents"));
}

#[tokio::test]
async fn usage_persists_with_the_settlement() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::Usage(crate::provider::TokenUsage {
                input: 100,
                output: 20,
                cache_read: 60,
                cache_write: 0,
            }),
            ScriptedMessage::text("done"),
        ]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let rows = store.usage(session_id).await.expect("usage rows");
    assert_eq!(
        rows.len(),
        1,
        "usage must persist exactly once at the settlement boundary"
    );
    assert_eq!(rows[0].input_tokens, 100);
    assert_eq!(rows[0].output_tokens, 20);
    assert_eq!(rows[0].cache_read_tokens, 60);
    assert_eq!(rows[0].cache_write_tokens, 0);
    assert_eq!(rows[0].step, 1);
}

#[tokio::test]
async fn usage_survives_a_failed_operation() {
    // §27.2: usage is independent of operation success — a failed
    // step's tokens still land in the ledger via the settlement commit.
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::Usage(crate::provider::TokenUsage {
                input: 5,
                output: 1,
                cache_read: 0,
                cache_write: 0,
            }),
            ScriptedMessage::Fail {
                message: "boom".to_owned(),
            },
        ]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    // The step fails visibly; the usage row must still be committed.
    collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let rows = store.usage(session_id).await.expect("usage rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].input_tokens, 5);
}

// ---- Policy, trust, and approvals (DESIGN.md §17, §32 Step 4 slice 2) ----

#[test]
fn canonicalization_resolves_and_normalizes_paths() {
    let registry = ToolRegistry::with_cwd("/tmp/project");
    let target = registry
        .canonicalize("read", &json!({ "path": "src/../src/main.rs" }))
        .expect("canonicalize");
    assert_eq!(
        target,
        crate::tool::CanonicalTarget::Path {
            path: "/tmp/project/src/main.rs".into()
        }
    );
    let target = registry
        .canonicalize("read", &json!({ "path": "/etc/hosts" }))
        .expect("canonicalize");
    assert_eq!(
        target,
        crate::tool::CanonicalTarget::Path {
            path: "/etc/hosts".into()
        }
    );
    let target = registry
        .canonicalize("bash", &json!({ "command": "echo hi" }))
        .expect("canonicalize");
    assert_eq!(
        target,
        crate::tool::CanonicalTarget::Command {
            command: "echo hi".into()
        }
    );
    assert!(registry.canonicalize("read", &json!({})).is_err());
}

#[test]
fn approval_required_terminates_from_tools_planned_only() {
    let (mut machine, _) = OperationMachine::accept(OperationId::generate(), "goal", Vec::new());
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start model step");
    let applied = machine
        .apply(Transition::ProviderCompleted {
            text: String::new(),
            tool_calls: vec![ToolCall {
                operation_id: machine.operation_id(),
                call_id: 1,
                name: "bash".to_owned(),
                arguments: json!({ "command": "echo hi" }),
            }],
        })
        .expect("plan tools");
    assert!(matches!(applied.state, OperationState::ToolsPlanned { .. }));

    let applied = machine
        .apply(Transition::ApprovalRequired {
            tool: "bash".to_owned(),
        })
        .expect("approval-required from ToolsPlanned");
    assert_eq!(
        applied.state,
        OperationState::Finished(OperationOutcome::ApprovalRequired {
            tool: "bash".to_owned()
        })
    );
    assert!(applied.intents.is_empty(), "no effect intent may exist");
    assert!(applied.entries.is_empty());

    // Terminal: the transition is invalid everywhere else.
    assert!(
        machine
            .apply(Transition::ApprovalRequired {
                tool: "bash".to_owned(),
            })
            .is_err()
    );
}

#[tokio::test]
async fn default_policy_terminates_bash_as_approval_required() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "bash",
            json!({ "command": "echo hi" }),
        )]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(
            |e| matches!(e, RuntimeEvent::OperationApprovalRequired { tool, .. } if tool == "bash")
        ),
        "default policy must gate bash: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Durable: the outcome and the planned call survive reopen, and no
    // tool result ever exists because nothing executed.
    let loaded = store.load(session_id).await.expect("load");
    assert!(matches!(
        loaded.operations[0].latest.1.state,
        OperationState::Finished(OperationOutcome::ApprovalRequired { ref tool }) if tool == "bash"
    ));
    let has_call = loaded
        .entries
        .iter()
        .any(|(_, entry)| matches!(entry, SessionEntry::ToolCall { call } if call.name == "bash"));
    let has_result = loaded
        .entries
        .iter()
        .any(|(_, entry)| matches!(entry, SessionEntry::ToolResult { .. }));
    assert!(has_call, "the planned call stays visible");
    assert!(!has_result, "nothing executed");
}

#[tokio::test]
async fn allowlist_policy_admits_bash_and_denial_is_model_visible() {
    // Granted: the documented mechanism admits bash end to end.
    let store = SessionStore::open_in_memory().expect("store");
    let policy: Arc<dyn PolicyEngine> = Arc::new(AllowlistPolicy::new(["bash"]));
    let runtime = Runtime::start_with_policy(
        ScriptedProvider::new(vec![
            ScriptedMessage::tool("bash", json!({ "command": "echo granted" })),
            ScriptedMessage::text("done"),
        ]),
        ToolRegistry::default(),
        store.clone(),
        policy,
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. }))
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    // Denied: the policy rejection settles as a model-visible tool
    // error so the model can choose another path (§17.4).
    struct DenyAll;
    impl PolicyEngine for DenyAll {
        fn decide(
            &self,
            _tool: &str,
            _target: &crate::tool::CanonicalTarget,
        ) -> crate::policy::PolicyDecision {
            crate::policy::PolicyDecision::Deny("denied by test policy".to_owned())
        }
    }
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_policy(
        ScriptedProvider::new(vec![
            ScriptedMessage::tool("read", json!({ "path": "x" })),
            ScriptedMessage::text("after denial"),
        ]),
        ToolRegistry::default(),
        store.clone(),
        Arc::new(DenyAll),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let loaded = store.load(session_id).await.expect("load");
    let denied = loaded.entries.iter().any(|(_, entry)| {
        matches!(entry, SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. },
        } if error.contains("denied by test policy"))
    });
    assert!(denied, "policy denial must be model-visible: {loaded:?}");
}

// ---- File-write reconciliation (DESIGN.md §12.3, §32 Step 4 slice 3) ----

mod reconcile {
    use super::*;
    use crate::tool::{ReconcileVerdict, classify_reconciliation, reconciliation_evidence};
    use std::path::PathBuf;

    fn sha_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        hex_encode(Sha256::digest(bytes).as_slice())
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[tokio::test]
    async fn write_evidence_records_preimage_and_postimage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let existing = dir.path().join("a.txt");
        std::fs::write(&existing, "old").expect("seed");
        let evidence = reconciliation_evidence(
            dir.path(),
            "write",
            &json!({ "path": "a.txt", "contents": "new" }),
        )
        .await
        .expect("evidence");
        assert_eq!(evidence["preimage"]["exists"], true);
        assert_eq!(evidence["preimage"]["hash"], sha_hex(b"old"));
        assert_eq!(evidence["postimage_hash"], sha_hex(b"new"));
        assert!(evidence["path"].as_str().unwrap().ends_with("a.txt"));

        // Absent preimage is recorded as absent, not as a fake hash.
        let evidence = reconciliation_evidence(
            dir.path(),
            "write",
            &json!({ "path": "missing.txt", "contents": "x" }),
        )
        .await
        .expect("evidence");
        assert_eq!(evidence["preimage"]["exists"], false);
        assert!(evidence["preimage"].get("hash").is_none());
    }

    #[tokio::test]
    async fn edit_evidence_hashes_the_patched_result() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("b.txt");
        std::fs::write(&file, "hello world").expect("seed");
        let evidence = reconciliation_evidence(
            dir.path(),
            "edit",
            &json!({ "path": "b.txt", "old_str": "world", "new_str": "ion" }),
        )
        .await
        .expect("evidence");
        assert_eq!(evidence["postimage_hash"], sha_hex(b"hello ion"));
        // A missing old_str cannot be classified: evidence fails.
        assert!(
            reconciliation_evidence(
                dir.path(),
                "edit",
                &json!({ "path": "b.txt", "old_str": "nope", "new_str": "x" }),
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn classification_covers_all_verdicts() {
        let preimage_hash = sha_hex(b"preimage");
        let postimage_hash = sha_hex(b"postimage");
        let evidence = json!({
            "path": "/tmp/x",
            "preimage": { "exists": true, "hash": preimage_hash },
            "postimage_hash": postimage_hash,
        });
        let digest = |bytes: &[u8]| {
            use sha2::{Digest, Sha256};
            Some(Sha256::digest(bytes).into())
        };
        assert_eq!(
            classify_reconciliation(&evidence, digest(b"postimage")),
            ReconcileVerdict::AlreadyApplied
        );
        assert_eq!(
            classify_reconciliation(&evidence, digest(b"preimage")),
            ReconcileVerdict::SafeToExecute
        );
        assert_eq!(
            classify_reconciliation(&evidence, digest(b"conflict")),
            ReconcileVerdict::Conflict
        );
        assert_eq!(
            classify_reconciliation(&evidence, None),
            ReconcileVerdict::Conflict
        );
        // Absent preimage must match an absent file.
        let created = json!({
            "path": "/tmp/x",
            "preimage": { "exists": false },
            "postimage_hash": postimage_hash,
        });
        assert_eq!(
            classify_reconciliation(&created, None),
            ReconcileVerdict::SafeToExecute
        );
        assert_eq!(
            classify_reconciliation(&json!(null), Some([0u8; 32])),
            ReconcileVerdict::Unknown
        );
    }

    /// Build a session whose operation is pending on one Reconcile
    /// write effect, then reopen and assert the recovery verdict.
    async fn pending_write_session(
        store: &SessionStore,
        cwd: &std::path::Path,
    ) -> crate::SessionId {
        let session_id = crate::SessionId::generate();
        store
            .create_session(SessionRecord {
                id: session_id,
                cwd: cwd.to_string_lossy().into_owned(),
                title: "reconcile".to_owned(),
            })
            .await
            .expect("create session");
        let operation_id = OperationId::generate();
        let (mut machine, _) = OperationMachine::accept(operation_id, "go", Vec::new());
        let root_inbox = InboxRecord {
            id: InboxId::generate(),
            kind: InboxKind::Prompt,
            text: "go".to_owned(),
            status: crate::InboxStatus::Applied,
        };
        let entry = EntryRecord {
            seq: 1,
            entry: SessionEntry::UserMessage {
                text: "go".to_owned(),
            },
        };
        let checkpoint = CheckpointRecord {
            state_seq: 1,
            payload: CheckpointPayload {
                state: machine.state().clone(),
                cancel_requested: false,
                prompt: "go".to_owned(),
                tools: Vec::new(),
                open_effect: None,
            },
        };
        store
            .begin_operation(session_id, operation_id, root_inbox, checkpoint, entry)
            .await
            .expect("begin");

        // The intended write: a.txt from "old" to "new".
        let target = cwd.join("a.txt");
        let evidence =
            reconciliation_evidence(cwd, "write", &json!({ "path": "a.txt", "contents": "new" }))
                .await
                .expect("evidence");
        machine
            .apply(Transition::StartModelStep {
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new(),
                },
            })
            .expect("start model step");
        machine
            .apply(Transition::ProviderCompleted {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    operation_id,
                    call_id: 1,
                    name: "write".to_owned(),
                    arguments: json!({ "path": "a.txt", "contents": "new" }),
                }],
            })
            .expect("plan the write");
        let applied = machine
            .apply(Transition::AdmitNextTool)
            .expect("admit the write");
        let EffectIntent::Tool { call } = applied.intents[0].clone() else {
            panic!("tool intent expected");
        };
        let effect = crate::EffectRecord {
            id: EffectId::generate(),
            kind: "tool:write".to_owned(),
            recovery_class: RecoveryClass::Reconcile,
            effective_input: json!({
                "tool": "write",
                "arguments": call.arguments,
                "call_id": call.call_id,
                "canonical": { "Path": { "path": target } },
                "reconciliation": evidence,
            }),
            attempt: 1,
        };
        let checkpoint = CheckpointRecord {
            state_seq: 2,
            payload: CheckpointPayload {
                state: machine.state().clone(),
                cancel_requested: false,
                prompt: "go".to_owned(),
                tools: Vec::new(),
                open_effect: Some(effect.clone()),
            },
        };
        store
            .commit(CommitRequest {
                session_id,
                operation_id,
                checkpoint,
                entries: Vec::new(),
                open_effects: vec![effect],
                settled_effects: Vec::new(),
                indeterminate_effects: Vec::new(),
                inbox: Vec::new(),
                inbox_applied: Vec::new(),
                usage: Vec::new(),
            })
            .await
            .expect("commit pending write");
        session_id
    }

    /// Recovery events are live-only and predate any subscriber (P6, §21.2);
    /// reopen tests observe recovery through the snapshot and the store.
    async fn wait_until_idle(session: &SessionHandle) {
        for _ in 0..100 {
            let snapshot = session.snapshot().await.expect("snapshot");
            if snapshot.operation == OperationStatus::Idle {
                return;
            }
            sleep(Duration::from_millis(20)).await;
        }
        panic!("operation never went idle after reopen");
    }

    #[tokio::test]
    async fn preimage_intact_reexecutes_the_write_exactly_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "old").expect("seed preimage");
        let store = SessionStore::open_in_memory().expect("store");
        let session_id = pending_write_session(&store, dir.path()).await;

        let runtime = Runtime::open_session(
            ScriptedProvider::echo(),
            ToolRegistry::with_cwd(dir.path()),
            store.clone(),
            session_id,
        )
        .await
        .expect("reopen");
        let session = runtime.session();
        wait_until_idle(&session).await;
        session.close().await.expect("close");
        runtime.join().await.expect("join");

        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("file"),
            "new",
            "recovery must execute the intended write"
        );
        let loaded = store.load(session_id).await.expect("load");
        let recovered = loaded.entries.iter().any(|(_, entry)| {
            matches!(entry, SessionEntry::ToolResult {
                result: ToolResult::Ok { output, .. },
            } if output == "written")
        });
        assert!(recovered, "{loaded:?}");
    }

    #[tokio::test]
    async fn postimage_present_settles_without_repeating() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The write already happened before the crash.
        std::fs::write(dir.path().join("a.txt"), "new").expect("seed postimage");
        let store = SessionStore::open_in_memory().expect("store");
        let session_id = pending_write_session(&store, dir.path()).await;

        let runtime = Runtime::open_session(
            ScriptedProvider::echo(),
            ToolRegistry::with_cwd(dir.path()),
            store.clone(),
            session_id,
        )
        .await
        .expect("reopen");
        let session = runtime.session();
        wait_until_idle(&session).await;
        session.close().await.expect("close");
        runtime.join().await.expect("join");

        let loaded = store.load(session_id).await.expect("load");
        let settled = loaded.entries.iter().any(|(_, entry)| {
            matches!(entry, SessionEntry::ToolResult {
                result: ToolResult::Ok { output, .. },
            } if output.contains("already applied"))
        });
        assert!(
            settled,
            "postimage must settle without repeating: {loaded:?}"
        );
    }

    #[tokio::test]
    async fn conflicting_file_state_settles_indeterminate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::open_in_memory().expect("store");
        // Evidence records the absent preimage FIRST; only then does
        // someone else create the file with unrelated contents.
        let session_id = pending_write_session(&store, dir.path()).await;
        std::fs::write(dir.path().join("a.txt"), "something else").expect("seed conflict");

        let runtime = Runtime::open_session(
            ScriptedProvider::echo(),
            ToolRegistry::with_cwd(dir.path()),
            store.clone(),
            session_id,
        )
        .await
        .expect("reopen");
        let session = runtime.session();
        wait_until_idle(&session).await;
        session.close().await.expect("close");
        runtime.join().await.expect("join");

        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("file"),
            "something else",
            "a conflicting file must never be overwritten"
        );
        let loaded = store.load(session_id).await.expect("load");
        assert!(matches!(
            loaded.operations[0].latest.1.state,
            OperationState::Finished(OperationOutcome::Indeterminate)
        ));
    }

    #[allow(dead_code)]
    fn path_type_check(_: PathBuf) {}
}

// ---- Readable compaction (DESIGN.md §14.7, §32 Step 4 slice 4) ----

#[test]
fn projector_uses_compaction_baseline_plus_suffix() {
    let entries = vec![
        SessionEntry::UserMessage {
            text: "early".to_owned(),
        },
        SessionEntry::AssistantMessage {
            text: "old answer".to_owned(),
        },
        SessionEntry::Compaction {
            covers_through_seq: 2,
            summary: "the user asked X; it was answered".to_owned(),
        },
        SessionEntry::UserMessage {
            text: "latest".to_owned(),
        },
    ];
    let plan = crate::context::project(&entries, 1);
    // The summary replaces everything through its coverage boundary.
    assert_eq!(plan.messages.len(), 2);
    assert!(
        plan.messages[0].prompt_text().contains("the user asked X"),
        "{:?}",
        plan.messages[0]
    );
    assert!(plan.messages[1].prompt_text().contains("latest"));
    assert!(!plan.messages[0].prompt_text().contains("early"));
}

#[test]
fn compaction_transitions_are_total() {
    // Invalid from Accepted.
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    assert!(
        machine
            .apply(Transition::StartCompaction {
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new()
                },
            })
            .is_err()
    );

    // NeedAssistant -> CompactionPending -> Completed appends the
    // readable entry and returns to NeedAssistant.
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine
        .apply(Transition::StartModelStep {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start");
    // Queue a steer so ProviderCompleted lands in NeedContinuation.
    machine
        .apply(Transition::ApplyInbox {
            item: InboxItem {
                kind: InboxKind::Steer,
                text: "more".to_owned(),
            },
        })
        .expect("queue steer");
    machine
        .apply(Transition::ProviderCompleted {
            text: String::new(),
            tool_calls: Vec::new(),
        })
        .expect("complete with queued steer");
    machine
        .drain_steers()
        .expect("drain steers at the reasoning boundary");
    let applied = machine
        .apply(Transition::StartCompaction {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start compaction from NeedAssistant");
    assert!(matches!(applied.state, OperationState::CompactionPending));
    assert!(matches!(
        applied.intents[0],
        EffectIntent::Compaction { .. }
    ));

    let applied = machine
        .apply(Transition::CompactionCompleted {
            summary: "handoff".to_owned(),
            covers_through_seq: 3,
        })
        .expect("complete compaction");
    assert_eq!(applied.state, OperationState::NeedAssistant);
    assert_eq!(
        applied.entries,
        vec![SessionEntry::Compaction {
            covers_through_seq: 3,
            summary: "handoff".to_owned(),
        }]
    );

    // Failure without a cancellation request continues the operation.
    machine
        .apply(Transition::StartCompaction {
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("compact again");
    let applied = machine
        .apply(Transition::CompactionFailed)
        .expect("abandon compaction");
    assert_eq!(applied.state, OperationState::NeedAssistant);
    let _ = applied;
}

#[derive(Clone, Default)]
struct CompactionProbe {
    log: Arc<Mutex<Vec<ProviderRequest>>>,
}

impl CompactionProbe {
    fn requests(&self) -> Vec<ProviderRequest> {
        self.log.lock().expect("log poisoned").clone()
    }
}

impl Provider for CompactionProbe {
    fn run(
        &self,
        request: ProviderRequest,
        _cancel: CancellationToken,
        out: mpsc::Sender<EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        let operation_id = request.operation_id;
        let step = request.step;
        let is_compaction = request.plan.messages.iter().any(|message| {
            matches!(message, crate::context::ContextMessage::User { content }
                if content.contains("Summarize the conversation"))
        });
        self.log.lock().expect("log poisoned").push(request);
        async move {
            if step == 1 {
                let _ = out
                    .send(EngineSignal::UsageUpdate {
                        operation_id,
                        step,
                        usage: crate::provider::TokenUsage {
                            input: 200_000,
                            output: 10,
                            cache_read: 0,
                            cache_write: 0,
                        },
                    })
                    .await;
            }
            let text = if is_compaction {
                "compact-summary: user asked X; answered; next: Y"
            } else {
                "answer"
            };
            let _ = out
                .send(EngineSignal::TextDelta {
                    operation_id,
                    step,
                    text: text.to_owned(),
                })
                .await;
            // Hold step 1 open so the test's steer lands mid-flight,
            // after the delta the test waits for.
            if step == 1 {
                sleep(Duration::from_millis(300)).await;
            }
            let _ = out
                .send(EngineSignal::Completed { operation_id, step })
                .await;
        }
    }
}

#[tokio::test]
async fn large_context_compacts_at_the_continuation_boundary() {
    let probe = CompactionProbe::default();
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(probe.clone(), ToolRegistry::default(), store.clone());
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit("go").await.expect("submit");
    // Steer while step 1 streams so the operation continues; compaction
    // before an idle operation buys nothing.
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    session.steer("and also this").await.expect("steer");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Step 1: plain history. Step 2: compaction (history + instruction).
    // Step 3: summary baseline + the steered prompt only.
    let requests = probe.requests();
    assert_eq!(requests.len(), 3, "compaction must run between the steps");
    assert!(
        requests[1]
            .plan
            .messages
            .iter()
            .any(|m| m.prompt_text().contains("Summarize the conversation")),
        "the compaction step must carry the summarize instruction"
    );
    let third = &requests[2].plan.messages;
    assert!(
        third.len() <= 3
            && third
                .iter()
                .any(|m| m.prompt_text().contains("compact-summary")),
        "the steered step must project the summary baseline + suffix: {third:?}"
    );

    // Canonical history is untouched: the compaction entry joins the
    // early entries, it does not replace them.
    let loaded = store.load(session_id).await.expect("load");
    let kinds: Vec<_> = loaded
        .entries
        .iter()
        .map(|(_, e)| entry_kind_name(e))
        .collect();
    assert!(kinds.contains(&"compaction"), "{kinds:?}");
    assert_eq!(
        kinds.iter().filter(|k| **k == "user_message").count(),
        2,
        "the prompt and the steer stay durable: {kinds:?}"
    );
}

fn entry_kind_name(entry: &SessionEntry) -> &'static str {
    match entry {
        SessionEntry::UserMessage { .. } => "user_message",
        SessionEntry::AssistantMessage { .. } => "assistant_message",
        SessionEntry::ToolCall { .. } => "tool_call",
        SessionEntry::ToolResult { .. } => "tool_result",
        SessionEntry::Compaction { .. } => "compaction",
    }
}
