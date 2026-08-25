use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;

use crate::context::{CapabilitySnapshot, ContextMessage, ContextPlan, load_trusted_resources};
use crate::error::{CommandError, RuntimeError};
use crate::ids::{EffectId, InboxId, OperationId};
use crate::policy::{AllowlistPolicy, PolicyEngine};
use crate::provider::{EngineSignal, Provider, ProviderRequest, ScriptedMessage, ScriptedProvider};
use crate::runtime::{
    EffectBoundary, EffectGate, OperationStatus, Runtime, RuntimeEvent, SaturatedHandle,
    SessionHandle,
};
use crate::session::{
    Applied, EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome,
    OperationState, SessionEntry, Transition,
};
use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EntryRecord, InboxRecord, SessionRecord,
    SessionStore,
};
use crate::tool::{
    RecoveryClass, Tool, ToolCall, ToolCatalog, ToolOutcome, ToolRegistry, ToolResult, ToolSpec,
};

const STEP: Duration = Duration::from_millis(50);

/// Model snapshot used by transition tests.
fn step_model() -> crate::provider::ModelConfig {
    crate::provider::ModelConfig {
        model_ref: "test-model".to_owned(),
        context_window: None,
        capabilities: crate::provider::ModelCapabilities::default(),
    }
}

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
                | RuntimeEvent::OperationIndeterminate { .. }
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
            RuntimeEvent::ThinkingDelta { .. } => "thinking_delta",
            RuntimeEvent::ToolStarted { .. } => "tool_started",
            RuntimeEvent::ToolSettled { .. } => "tool_settled",
            RuntimeEvent::OperationFinished { .. } => "operation_finished",
            RuntimeEvent::OperationFailed { .. } => "operation_failed",
            RuntimeEvent::OperationIndeterminate { .. } => "operation_indeterminate",
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
            model: step_model(),
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
            model: step_model(),
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
            model: step_model(),
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
            model: step_model(),
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
                artifact: None,
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
                output: "contents".to_owned(),
                artifact: None,
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
                artifact: None,
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
            model: step_model(),
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
                artifact: None,
            },
        })
        .expect("settle");
    assert_eq!(machine.state(), &OperationState::NeedAssistant);
    assert_eq!(
        applied.entries,
        vec![SessionEntry::ToolResult {
            result: ToolResult::Err {
                call_id: 1,
                error: "read failed".to_owned(),
                artifact: None,
            }
        }]
    );
}

#[test]
fn steer_during_effect_queues_and_applies_at_the_boundary() {
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine
        .apply(Transition::StartModelStep {
            model: step_model(),
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
    assert!(machine.has_queued_steers());

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
            model: step_model(),
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
            model: step_model(),
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
fn cancel_request_sets_effects_updating_and_settles_cancelled() {
    // During a model step.
    let (mut machine, _) = machine_with_tools("goal", vec![]);
    machine
        .apply(Transition::StartModelStep {
            model: step_model(),
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
            model: step_model(),
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
                artifact: None,
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
            model: step_model(),
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
                model: step_model(),
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new(),
                },
            });
        },
        |m| {
            let _ = m.apply(Transition::StartModelStep {
                model: step_model(),
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
                artifact: None,
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
        model: step_model(),
        plan: ContextPlan {
            system: String::new(),
            messages: Vec::new(),
        },
    })
    .expect("start");
    let err = m
        .apply(Transition::StartModelStep {
            model: step_model(),
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
        model: step_model(),
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
            model: step_model(),
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
                artifact: None,
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
            model: step_model(),
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

    let operation_id = session.submit_if_idle("hi").await.expect("submit");
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
    let operation_id = session.submit_if_idle("slow").await.expect("submit");

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
    let first = session.submit_if_idle("a").await.expect("first submit");
    let err = session
        .submit_if_idle("b")
        .await
        .expect_err("second submit");
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
async fn enqueue_promotes_distinct_operations_in_acceptance_order() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![
            ScriptedMessage::delayed(Duration::from_millis(100), "first"),
            ScriptedMessage::text("second"),
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let first = session.submit_if_idle("one").await.expect("first submit");
    sleep(STEP).await;
    let second = session.enqueue("two").await.expect("enqueue");
    assert_ne!(first, second);

    let mut starts = Vec::new();
    let mut finishes = Vec::new();
    while finishes.len() < 2 {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        match event {
            RuntimeEvent::OperationStarted { operation_id, .. } => starts.push(operation_id),
            RuntimeEvent::OperationFinished { operation_id, .. } => finishes.push(operation_id),
            _ => {}
        }
    }
    assert_eq!(starts, vec![first, second]);
    assert_eq!(finishes, vec![first, second]);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn queued_operation_survives_close_and_promotes_after_reopen() {
    let db = temp_db("queued-reopen");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "active never settles",
        )]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let first = session.submit_if_idle("active").await.expect("submit");
    wait_for_state(&session, |state| {
        matches!(state, OperationState::AssistantEffectPending)
    })
    .await;
    let second = session.enqueue("queued").await.expect("enqueue");

    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);

    let loaded = store.load(session_id).await.expect("load queued state");
    assert_eq!(loaded.operations.len(), 2);
    assert_eq!(loaded.operations[0].id, first);
    assert_eq!(loaded.operations[1].id, second);
    assert_eq!(
        loaded.operations[0].latest.1.state,
        OperationState::Suspended
    );
    assert_eq!(
        loaded.operations[1].latest.1.state,
        OperationState::Accepted
    );

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("queued result")]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::OperationFinished { operation_id, .. } if *operation_id == second
    )));

    let loaded = store.load(session_id).await.expect("load settled state");
    assert_eq!(
        loaded.operations[0].latest.1.state,
        OperationState::Finished(OperationOutcome::Cancelled)
    );
    assert_eq!(
        loaded.operations[1].latest.1.state,
        OperationState::Finished(OperationOutcome::Completed)
    );

    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn steer_requires_an_active_operation() {
    let runtime = tool_runtime();
    let session = runtime.session();
    assert_eq!(
        session.steer("nope").await,
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
    let err = session.submit_if_idle("after").await.expect_err("closed");
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
    let operation_id = session.submit_if_idle("wait").await.expect("submit");
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
    let first = session.submit_if_idle("a").await.expect("first");
    let _ = collect_until_terminal(&mut events).await.expect("first op");

    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.operation, OperationStatus::Idle);

    let second = session.submit_if_idle("b").await.expect("second");
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

#[tokio::test]
async fn operation_state_is_one_replaceable_authoritative_row() {
    let db = temp_db("replaceable-operation-state");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("state me").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("settle");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    drop(session);
    drop(store);

    let connection = rusqlite::Connection::open(&db).expect("open sqlite");
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM operation_state WHERE operation_id = ?1",
            [operation_id.as_uuid().to_string()],
            |row| row.get(0),
        )
        .expect("count operation state rows");
    assert_eq!(count, 1);
    let kind: String = connection
        .query_row(
            "SELECT kind FROM operation_state WHERE operation_id = ?1",
            [operation_id.as_uuid().to_string()],
            |row| row.get(0),
        )
        .expect("read operation state");
    assert_eq!(kind, "finished");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn new_runtime_persists_explicit_workspace_identity() {
    let db = temp_db("explicit-cwd");
    let store = SessionStore::open(&db).expect("open store");
    let workspace = std::env::temp_dir().join(format!("ion-runtime-cwd-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&workspace).expect("workspace");
    let runtime = Runtime::start_with_policy_and_resources_in_cwd(
        ScriptedProvider::echo(),
        ToolRegistry::with_cwd(&workspace),
        store.clone(),
        permissive_policy(),
        Vec::new(),
        workspace.to_string_lossy().into_owned(),
    );
    let session_id = runtime.session_id();
    runtime.session().close().await.expect("close");
    runtime.join().await.expect("join");

    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(loaded.session.cwd, workspace.to_string_lossy());
    let _ = std::fs::remove_dir_all(workspace);
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
}

#[tokio::test]
async fn reopened_runtime_gets_a_new_instance_identity() {
    let db = temp_db("runtime-instance-id");
    let store = SessionStore::open(&db).expect("open store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let first = runtime
        .session()
        .snapshot()
        .await
        .expect("first snapshot")
        .runtime_instance_id;
    runtime
        .session()
        .close()
        .await
        .expect("close first runtime");
    runtime.join().await.expect("join first runtime");

    let reopened = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::default(),
        store,
        session_id,
    )
    .await
    .expect("reopen");
    let second = reopened
        .session()
        .snapshot()
        .await
        .expect("second snapshot")
        .runtime_instance_id;
    assert_ne!(first, second);
    reopened
        .session()
        .close()
        .await
        .expect("close reopened runtime");
    reopened.join().await.expect("join reopened runtime");
    let _ = std::fs::remove_dir_all(db.parent().expect("temp parent"));
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
    assert!(!names.contains(&"compact"));
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
        artifact: None,
    };
    let err = ToolResult::Err {
        call_id: 2,
        error: "boom".into(),
        artifact: None,
    };
    assert_eq!(ok.call_id(), 1);
    assert!(ok.is_ok());
    assert_eq!(err.call_id(), 2);
    assert!(!err.is_ok());
    assert_eq!(
        ToolResult::Ok {
            call_id: 3,
            output: "x".into(),
            artifact: None,
        }
        .model_text(),
        "x"
    );
    assert_eq!(err.model_text(), "boom");
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

#[cfg(unix)]
#[tokio::test]
async fn native_file_tools_reject_symlink_targets_and_parents() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    std::fs::write(outside.path().join("secret.txt"), "outside").expect("seed outside file");
    std::fs::create_dir(outside.path().join("nested")).expect("seed outside directory");
    std::fs::write(outside.path().join("nested/secret.txt"), "nested outside")
        .expect("seed nested outside file");
    symlink(
        outside.path().join("secret.txt"),
        root.path().join("link.txt"),
    )
    .expect("link file");
    symlink(outside.path().join("nested"), root.path().join("link-dir")).expect("link directory");

    let registry = ToolRegistry::with_cwd(root.path());
    let cancel = tokio_util::sync::CancellationToken::new();
    for (name, arguments) in [
        ("read", json!({"path": "link.txt"})),
        (
            "write",
            json!({"path": "link.txt", "contents": "overwritten"}),
        ),
        (
            "write",
            json!({"path": "link-dir/new.txt", "contents": "escaped"}),
        ),
        (
            "edit",
            json!({"path": "link.txt", "old_str": "outside", "new_str": "changed"}),
        ),
        ("search", json!({"path": "link-dir", "pattern": "secret"})),
        ("find", json!({"path": "link-dir", "pattern": "*.txt"})),
    ] {
        let outcome = registry.execute(name, &arguments, cancel.clone()).await;
        assert!(outcome.is_error, "{name} followed a symlink: {outcome:?}");
        assert!(
            outcome.output.contains("symlink"),
            "{name} did not explain the rejected link: {outcome:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(outside.path().join("secret.txt")).expect("outside file"),
        "outside"
    );
}

#[tokio::test]
async fn native_file_tools_reject_protected_git_paths() {
    let root = tempfile::tempdir().expect("root tempdir");
    std::fs::create_dir(root.path().join(".git")).expect("git directory");
    std::fs::write(root.path().join(".git/config"), "private").expect("git config");
    let registry = ToolRegistry::with_cwd(root.path());
    let cancel = tokio_util::sync::CancellationToken::new();

    for (name, arguments) in [
        ("read", json!({"path": ".git/config"})),
        (
            "write",
            json!({"path": ".git/config", "contents": "changed"}),
        ),
        (
            "edit",
            json!({"path": ".git/config", "old_str": "private", "new_str": "changed"}),
        ),
        ("search", json!({"path": ".git", "pattern": "private"})),
        ("find", json!({"path": ".git", "pattern": "*"})),
    ] {
        let outcome = registry.execute(name, &arguments, cancel.clone()).await;
        assert!(
            outcome.is_error,
            "{name} accessed protected path: {outcome:?}"
        );
        assert!(
            outcome.output.contains("protected"),
            "{name} did not explain the protected path: {outcome:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join(".git/config")).expect("git config"),
        "private"
    );
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
async fn bash_progress_checkpoint_is_bounded_and_cleared() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::tool("bash", json!({"command": "sleep 1 && echo done"})),
            ScriptedMessage::text("finished"),
        ]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("run").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::ToolStarted { .. }) {
            break;
        }
    }
    sleep(Duration::from_millis(100)).await;
    let loaded = store.load(runtime.session_id()).await.expect("load");
    assert_eq!(loaded.tool_progress.len(), 1);
    assert!(loaded.tool_progress[0].output.len() <= 16 * 1024);
    collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        store
            .load(runtime.session_id())
            .await
            .expect("load after settle")
            .tool_progress
            .is_empty()
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
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
    let operation_id = session.submit_if_idle("go").await.expect("submit");
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
async fn file_backed_runtime_persists_bounded_result_and_raw_artifact() {
    let data = tempfile::tempdir().expect("data directory");
    let store = SessionStore::open(data.path().join("sessions.db")).expect("file-backed store");
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool(
            "bash",
            json!({"command":"i=0; while [ \"$i\" -lt 20000 ]; do printf x; i=$((i+1)); done"}),
        ),
        ScriptedMessage::text("done\n"),
    ]);
    let runtime = Runtime::start_with_policy(
        provider,
        ToolRegistry::default(),
        store,
        permissive_policy(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|event| matches!(event, RuntimeEvent::OperationFinished { .. }))
    );

    let snapshot = session.snapshot().await.expect("snapshot");
    let (output, artifact) = snapshot
        .entries
        .iter()
        .find_map(|entry| match entry {
            SessionEntry::ToolResult {
                result: ToolResult::Ok {
                    output, artifact, ..
                },
            } => Some((output.clone(), artifact.clone())),
            _ => None,
        })
        .expect("durable bash result");
    let artifact = artifact.expect("durable raw artifact");
    assert!(output.contains("tool output abbreviated"));
    assert!(output.len() <= 16 * 1024);
    assert_eq!(artifact.total_bytes, 20_000);
    let id = artifact
        .uri
        .strip_prefix("artifact://")
        .expect("artifact URI");
    let raw = std::fs::read(data.path().join("artifacts").join(id)).expect("raw artifact");
    assert_eq!(raw, vec![b'x'; 20_000]);

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
    let operation_id = session.submit_if_idle("go").await.expect("submit");
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
    session.submit_if_idle("go").await.expect("submit");
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
    session.submit_if_idle("go").await.expect("submit");
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
    let operation_id = session.submit_if_idle("go").await.expect("submit");

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
    let operation_id = session.submit_if_idle("go").await.expect("submit");
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
async fn model_step_request_carries_step_tool_specs() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("read", json!({"path":"Cargo.toml"})),
        ScriptedMessage::text("done"),
    ]);
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
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
async fn capability_snapshot_refreshes_at_each_model_step() {
    struct DynamicTool;
    impl Tool for DynamicTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "dynamic".to_owned(),
                description: "dynamic test capability".to_owned(),
                input_schema: json!({"type": "object", "required": []}),
            }
        }

        fn call<'a>(
            &'a self,
            _arguments: serde_json::Value,
            _cancel: CancellationToken,
        ) -> std::pin::Pin<Box<dyn Future<Output = ToolOutcome> + Send + 'a>> {
            Box::pin(async { ToolOutcome::text("dynamic") })
        }
    }

    let provider = SharedLogProvider {
        settle_delay: Duration::from_millis(100),
        ..SharedLogProvider::default()
    };
    let catalog = ToolCatalog::default();
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_policy(
        provider.clone(),
        catalog.clone(),
        store,
        permissive_policy(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    catalog.register_scope("dynamic", vec![Arc::new(DynamicTool)]);
    session.steer("continue").await.expect("steer");
    collect_until_terminal(&mut events).await.expect("collect");
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(!requests[0].tools.iter().any(|tool| tool.name == "dynamic"));
    assert!(requests[1].tools.iter().any(|tool| tool.name == "dynamic"));
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
    session.submit_if_idle("goal").await.expect("submit");
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
    session.submit_if_idle("slow").await.expect("submit");
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
            crate::SessionEntry::ModelChanged { .. } => "model_changed",
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
    session.submit_if_idle("read it").await.expect("submit");
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

    let err = session
        .submit_if_idle("lost")
        .await
        .expect_err("submit must fail");
    assert!(matches!(err, CommandError::Persistence(_)));
    // No operation was installed: the session is still idle and usable.
    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.operation, OperationStatus::Idle);
    assert!(snapshot.entries.is_empty());

    let operation_id = session
        .submit_if_idle("kept")
        .await
        .expect("retry succeeds");
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
    session.submit_if_idle("go").await.expect("submit");
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
    let operation_id = session.submit_if_idle("slow").await.expect("submit");
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
    session.submit_if_idle("goal").await.expect("submit");
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
                model: step_model(),
                plan: ContextPlan {
                    system: String::new(),
                    messages: Vec::new(),
                },
            });
        },
        |m: &mut OperationMachine| {
            let _ = m.apply(Transition::StartModelStep {
                model: step_model(),
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
            model: step_model(),
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
    session.submit_if_idle("goal").await.expect("submit");
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

    // Reopen the same session: the suspended operation settles as
    // cancelled (§9.5 — suspend is teardown with effects cancelled, so
    // it can never continue) and the session accepts new work.
    let store = SessionStore::open(&db).expect("reopen store");
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
    assert_eq!(
        snapshot.operation,
        OperationStatus::Idle,
        "a settled suspended operation must not block the session"
    );
    assert_eq!(
        snapshot.reopen_entry_count,
        Some(before.entries.len()),
        "the runtime owns the reopen boundary used by frontends"
    );
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
    // The settlement is durable: the latest checkpoint is terminal.
    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(loaded.operations.len(), 1);
    assert!(matches!(
        loaded.operations[0].latest.1.state,
        OperationState::Finished(_) | OperationState::Suspended
    ));
    // New work is accepted immediately.
    session
        .submit_if_idle("new")
        .await
        .expect("submit after settle");
    collect_until_terminal(&mut events).await.expect("collect");
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
async fn store_close_joins_writer_and_rejects_later_requests() {
    let store = SessionStore::open_in_memory().expect("store");
    store.close().await.expect("close store");
    assert!(matches!(
        store.latest_session().await,
        Err(crate::store::StoreError::Closed)
    ));
    store.close().await.expect("idempotent close");
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
    session.submit_if_idle("go").await.expect("submit");
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
                    capability_snapshot_id: loaded.operations[0].capability_snapshot.id.clone(),
                    open_effect: None,
                },
                capability_snapshot: loaded.operations[0].capability_snapshot.clone(),
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
            context_manifests: Vec::new(),
            assistant_frames_delete: Vec::new(),
            tool_progress_delete: Vec::new(),
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
async fn effect_gate_crash_prefix_reopens_after_durable_model_intent() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ModelExecution);
    let provider = SharedLogProvider::default();
    let runtime = Runtime::start_with_effect_gate(
        provider.clone(),
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, _events) = session.subscribe().await.expect("subscribe");
    let submit_session = session.clone();
    let submit = tokio::spawn(async move { submit_session.submit_if_idle("goal").await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("effect gate reached");

    assert!(
        provider.requests().is_empty(),
        "the provider must not start before the gate"
    );
    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(checkpoint.state, OperationState::AssistantEffectPending);
    assert!(checkpoint.open_effect.is_some());

    // Abort exactly at the committed-intent / external-execution boundary.
    runtime.crash();
    gate.release();
    let _ = submit.await.expect("submit task");
    drop(session);
    drop(runtime);

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
    assert!(recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::AssistantTextDelta { text, .. } if text == "recovered\n"
    )));
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_after_model_settlement() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ModelSettlement);
    let runtime = Runtime::start_with_effect_gate(
        ScriptedProvider::new(vec![ScriptedMessage::text("before crash\n")]),
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, _events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("effect gate reached");

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert_eq!(checkpoint.state, OperationState::AssistantEffectPending);
    assert!(checkpoint.open_effect.is_some());

    runtime.crash();
    gate.release();
    drop(session);
    drop(runtime);

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
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_before_tool_execution() {
    let dir = std::env::temp_dir().join(format!("ion-gate-read-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("note.txt"), "persisted bytes").expect("write");
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ToolExecution);
    let runtime = Runtime::start_with_effect_gate(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "read",
            json!({"path": "note.txt"}),
        )]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, _events) = session.subscribe().await.expect("subscribe");
    let submit_session = session.clone();
    let submit = tokio::spawn(async move { submit_session.submit_if_idle("read").await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("effect gate reached");

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(matches!(
        checkpoint.state,
        OperationState::ToolEffectPending { .. }
    ));
    assert!(checkpoint.open_effect.is_some());
    assert!(
        !loaded
            .entries
            .iter()
            .any(|(_, entry)| matches!(entry, SessionEntry::ToolResult { .. }))
    );

    runtime.crash();
    gate.release();
    let _ = submit.await.expect("submit task");
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("recovered\n")]),
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
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_after_tool_execution() {
    let dir = std::env::temp_dir().join(format!("ion-gate-settle-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("note.txt"), "persisted bytes").expect("write");
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::ToolSettlement);
    let runtime = Runtime::start_with_effect_gate(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "read",
            json!({"path": "note.txt"}),
        )]),
        ToolRegistry::with_cwd(&dir),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, _events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("read").await.expect("submit");
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("effect gate reached");

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(matches!(
        checkpoint.state,
        OperationState::ToolEffectPending { .. }
    ));
    assert!(checkpoint.open_effect.is_some());

    runtime.crash();
    gate.release();
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("recovered\n")]),
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
    assert!(loaded.entries.iter().any(|(_, entry)| matches!(
        entry,
        SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. }
        } if output.contains("persisted bytes")
    )));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn effect_gate_close_waits_for_suspend_commit() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::CloseSuspendCommit);
    let runtime = Runtime::start_with_effect_gate(
        SharedLogProvider {
            settle_delay: Duration::from_millis(250),
            ..SharedLogProvider::default()
        },
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }

    let close_session = session.clone();
    let close = tokio::spawn(async move { close_session.close().await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("suspend gate reached");
    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(matches!(
        checkpoint.state,
        OperationState::AssistantEffectPending
    ));

    gate.release();
    close.await.expect("close task").expect("close");
    runtime.join().await.expect("join");
    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(matches!(checkpoint.state, OperationState::Suspended));
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_before_compaction_execution() {
    let probe = CompactionProbe::with_window(128_000);
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::CompactionExecution);
    let runtime = Runtime::start_with_effect_gate(
        probe.clone(),
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
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
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("compaction gate reached");

    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(matches!(
        checkpoint.state,
        OperationState::CompactionPending
    ));
    assert!(checkpoint.open_effect.is_some());
    assert!(
        loaded
            .entries
            .iter()
            .all(|(_, entry)| !matches!(entry, SessionEntry::Compaction { .. }))
    );

    runtime.crash();
    gate.release();
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(probe, ToolRegistry::default(), store.clone(), session_id)
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
    assert!(
        loaded
            .entries
            .iter()
            .any(|(_, entry)| matches!(entry, SessionEntry::Compaction { .. }))
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_after_durable_cancellation() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::CancellationSignal);
    let runtime = Runtime::start_with_effect_gate(
        SharedLogProvider {
            settle_delay: Duration::from_secs(30),
            ..SharedLogProvider::default()
        },
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("cancel me").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }

    let cancel_session = session.clone();
    let cancel = tokio::spawn(async move { cancel_session.cancel(operation_id).await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("cancellation gate reached");
    let loaded = store.load(session_id).await.expect("load");
    let (_, checkpoint) = &loaded.operations[0].latest;
    assert!(checkpoint.cancel_requested);
    assert!(matches!(
        checkpoint.state,
        OperationState::AssistantEffectPending
    ));

    runtime.crash();
    gate.release();
    let _ = cancel.await.expect("cancel task");
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::text("late provider result")]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::OperationCancelled { operation_id: id, .. } if *id == operation_id
    )));
    let loaded = store.load(session_id).await.expect("load");
    assert!(matches!(
        loaded.operations[0].latest.1.state,
        OperationState::Finished(OperationOutcome::Cancelled)
    ));
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn effect_gate_crash_prefix_reopens_with_a_durable_queued_operation() {
    let store = SessionStore::open_in_memory().expect("store");
    let gate = EffectGate::new(EffectBoundary::QueuedAcceptanceCommit);
    let runtime = Runtime::start_with_effect_gate(
        SharedLogProvider {
            settle_delay: Duration::from_secs(30),
            ..SharedLogProvider::default()
        },
        ToolRegistry::default(),
        store.clone(),
        gate.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("first").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }

    let enqueue_session = session.clone();
    let enqueue = tokio::spawn(async move { enqueue_session.enqueue("second").await });
    timeout(Duration::from_secs(2), gate.wait_until_reached())
        .await
        .expect("queue gate reached");
    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(loaded.operations.len(), 2);
    assert!(matches!(
        loaded.operations[1].latest.1.state,
        OperationState::Accepted
    ));

    runtime.crash();
    gate.release();
    let _ = enqueue.await.expect("enqueue task");
    drop(session);
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("first recovered"),
            ScriptedMessage::text("second recovered"),
        ]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let first = collect_until_terminal(&mut events).await.expect("first");
    assert!(matches!(
        first.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let second = collect_until_terminal(&mut events).await.expect("second");
    assert!(matches!(
        second.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    let loaded = store.load(session_id).await.expect("load");
    assert_eq!(
        loaded
            .entries
            .iter()
            .filter(|(_, entry)| matches!(entry, SessionEntry::UserMessage { .. }))
            .count(),
        2
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn interrupted_recovery_can_restart_from_its_replacement_intent() {
    let store = SessionStore::open_in_memory().expect("store");
    let initial_gate = EffectGate::new(EffectBoundary::ModelExecution);
    let initial = Runtime::start_with_effect_gate(
        SharedLogProvider::default(),
        ToolRegistry::default(),
        store.clone(),
        initial_gate.clone(),
    );
    let session_id = initial.session_id();
    let session = initial.session();
    let submit_session = session.clone();
    let submit = tokio::spawn(async move { submit_session.submit_if_idle("goal").await });
    timeout(Duration::from_secs(2), initial_gate.wait_until_reached())
        .await
        .expect("initial gate reached");
    initial.crash();
    initial_gate.release();
    let _ = submit.await.expect("submit task");
    drop(session);
    drop(initial);

    let recovering = SharedLogProvider {
        settle_delay: Duration::from_secs(30),
        ..SharedLogProvider::default()
    };
    let runtime = Runtime::open_session(
        recovering.clone(),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("first reopen");
    for _ in 0..100 {
        if !recovering.requests().is_empty() {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(recovering.requests().len(), 1, "recovery provider started");
    runtime.crash();
    drop(runtime);

    let runtime = Runtime::open_session(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_millis(50),
            "recovered twice",
        )]),
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("second reopen");
    let session = runtime.session();
    for _ in 0..100 {
        let loaded = store.load(session_id).await.expect("poll recovery");
        if matches!(
            loaded.operations[0].latest.1.state,
            OperationState::Finished(OperationOutcome::Completed)
        ) {
            break;
        }
        sleep(Duration::from_millis(20)).await;
    }
    let loaded = store.load(session_id).await.expect("load recovered");
    assert!(
        matches!(
            loaded.operations[0].latest.1.state,
            OperationState::Finished(OperationOutcome::Completed)
        ),
        "final recovery state: {:?}",
        loaded.operations[0].latest.1.state
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn crash_during_model_step_recovers_by_replay() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        SharedLogProvider {
            settle_delay: Duration::from_secs(30),
            ..SharedLogProvider::default()
        },
        ToolRegistry::default(),
        store.clone(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }
    assert_eq!(
        store
            .load(session_id)
            .await
            .expect("load")
            .assistant_frames
            .len(),
        1
    );

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
    assert!(loaded.assistant_frames.is_empty());
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
    session.submit_if_idle("read it").await.expect("submit");
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
    session.submit_if_idle("run").await.expect("submit");
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
    let warning = snapshot
        .indeterminate
        .as_ref()
        .expect("recovery warning must survive until a frontend attaches");
    assert!(warning.message.contains("inspect it before retrying"));

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
    let operation_id = session.submit_if_idle("next").await.expect("submit");
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
                artifact: None,
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
async fn trusted_resources_enter_future_model_steps_only_when_host_supplies_them() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("AGENTS.md"), "project rules").expect("resource");
    let trusted = load_trusted_resources(root.path(), true).expect("trusted resources");
    assert_eq!(trusted.len(), 1);

    let provider = SharedLogProvider::default();
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_policy_and_resources(
        provider.clone(),
        ToolRegistry::default(),
        store,
        Arc::new(crate::policy::DefaultPolicy),
        trusted,
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");
    collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        provider
            .requests()
            .first()
            .expect("model request")
            .plan
            .system
            .contains("[Trusted project resource: AGENTS.md]\nproject rules")
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
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
    session.submit_if_idle("go").await.expect("submit");
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
    session.submit_if_idle("go").await.expect("submit");
    // The step fails visibly; the usage row must still be committed.
    collect_until_terminal(&mut events).await.expect("collect");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let rows = store.usage(session_id).await.expect("usage rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].input_tokens, 5);
}

#[tokio::test]
async fn cache_expectation_records_cold_start_then_stable_prefix_reuse() {
    let db = temp_db("cache-expectation");
    let store = SessionStore::open(&db).expect("store");
    let runtime = Runtime::start_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("first"),
            ScriptedMessage::text("second"),
        ])
        .with_prompt_cache(true),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("one").await.expect("first submit");
    collect_until_terminal(&mut events)
        .await
        .expect("first collect");
    session.submit_if_idle("two").await.expect("second submit");
    collect_until_terminal(&mut events)
        .await
        .expect("second collect");
    let session_id = runtime.session_id();
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let connection = rusqlite::Connection::open(&db).expect("open db");
    let expectations: Vec<String> = connection
        .prepare("SELECT cache_expectation FROM model_steps ORDER BY created_at")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    assert_eq!(expectations, ["cold_start", "prefix_reuse_expected"]);
    let fingerprints: Vec<String> = connection
        .prepare("SELECT context_fingerprint FROM model_steps ORDER BY created_at")
        .expect("prepare fingerprints")
        .query_map([], |row| row.get(0))
        .expect("query fingerprints")
        .collect::<Result<Vec<_>, _>>()
        .expect("fingerprint rows");
    assert_eq!(fingerprints.len(), 2);
    assert_eq!(fingerprints[0], fingerprints[1]);
    let _ = store.load(session_id).await.expect("load");
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
    assert!(
        registry
            .canonicalize("read", &json!({ "path": "/etc/hosts" }))
            .is_err(),
        "canonicalization must reject the same absolute path the executor rejects"
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
            model: step_model(),
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
    session.submit_if_idle("go").await.expect("submit");
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
    session.submit_if_idle("go").await.expect("submit");
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
    session.submit_if_idle("go").await.expect("submit");
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
    use crate::tool::{
        FileSnapshot, ReconcileVerdict, classify_reconciliation, classify_reconciliation_snapshot,
        reconciliation_evidence,
    };
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
        assert!(evidence["preimage"]["identity"].is_string());
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

    #[tokio::test]
    async fn write_rejects_same_content_file_replacement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "old").expect("seed");
        let arguments = json!({ "path": "a.txt", "contents": "new" });
        let evidence = reconciliation_evidence(dir.path(), "write", &arguments)
            .await
            .expect("evidence");

        let replacement = dir.path().join("replacement.txt");
        std::fs::write(&replacement, "old").expect("replacement");
        std::fs::rename(&replacement, &file).expect("replace file");

        let registry = ToolRegistry::with_cwd(dir.path());
        let outcome = registry
            .execute_with_reconciliation(
                "write",
                &arguments,
                Some(&evidence),
                None,
                tokio_util::sync::CancellationToken::new(),
                None,
            )
            .await;
        assert!(
            outcome.is_error,
            "replacement must not be overwritten: {outcome:?}"
        );
        assert!(outcome.output.contains("precondition"), "{outcome:?}");
        assert_eq!(std::fs::read_to_string(&file).expect("file"), "old");
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

        let identity_evidence = json!({
            "preimage": {
                "exists": true,
                "hash": sha_hex(b"preimage"),
                "identity": "1:1",
            },
            "postimage_hash": sha_hex(b"postimage"),
        });
        let replacement = FileSnapshot {
            hash: digest(b"preimage").expect("digest"),
            identity: Some("2:2".to_owned()),
        };
        assert_eq!(
            classify_reconciliation_snapshot(&identity_evidence, Some(&replacement)),
            ReconcileVerdict::Conflict,
            "same-content replacement must not be replayed"
        );
        let original = FileSnapshot {
            identity: Some("1:1".to_owned()),
            ..replacement
        };
        assert_eq!(
            classify_reconciliation_snapshot(&identity_evidence, Some(&original)),
            ReconcileVerdict::SafeToExecute
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
                initial_model_ref: "test-model".to_owned(),
                parent_session_id: None,
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
                capability_snapshot_id: CapabilitySnapshot::new(Vec::new()).id.clone(),
                open_effect: None,
            },
            capability_snapshot: CapabilitySnapshot::new(Vec::new()),
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
                model: step_model(),
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
                capability_snapshot_id: CapabilitySnapshot::new(Vec::new()).id.clone(),
                open_effect: Some(effect.clone()),
            },
            capability_snapshot: CapabilitySnapshot::new(Vec::new()),
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
                context_manifests: Vec::new(),
                assistant_frames_delete: Vec::new(),
                tool_progress_delete: Vec::new(),
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
    async fn same_content_replacement_settles_indeterminate_without_overwrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "old").expect("seed preimage");
        let store = SessionStore::open_in_memory().expect("store");
        let session_id = pending_write_session(&store, dir.path()).await;

        let replacement = dir.path().join("replacement.txt");
        std::fs::write(&replacement, "old").expect("replacement");
        std::fs::rename(&replacement, dir.path().join("a.txt")).expect("replace file");

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
            "old",
            "recovery must not overwrite a same-content replacement"
        );
        let loaded = store.load(session_id).await.expect("load");
        assert!(matches!(
            loaded.operations[0].latest.1.state,
            OperationState::Finished(OperationOutcome::Indeterminate)
        ));
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
            model: step_model(),
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

#[derive(Clone)]
struct CompactionProbe {
    log: Arc<Mutex<Vec<ProviderRequest>>>,
    context_window: Option<u64>,
}

impl CompactionProbe {
    fn with_window(tokens: u64) -> Self {
        Self {
            log: Arc::new(Mutex::new(Vec::new())),
            context_window: Some(tokens),
        }
    }
}

impl CompactionProbe {
    fn requests(&self) -> Vec<ProviderRequest> {
        self.log.lock().expect("log poisoned").clone()
    }
}

impl Provider for CompactionProbe {
    async fn context_window(&self) -> Option<u64> {
        self.context_window
    }

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
    // 200k usage against a 128k window trips the safety net
    // (context_tokens > window - 16k reserve, 14.7.3).
    let probe = CompactionProbe::with_window(128_000);
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(probe.clone(), ToolRegistry::default(), store.clone());
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
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
        SessionEntry::ModelChanged { .. } => "model_changed",
        SessionEntry::AssistantMessage { .. } => "assistant_message",
        SessionEntry::ToolCall { .. } => "tool_call",
        SessionEntry::ToolResult { .. } => "tool_result",
        SessionEntry::Compaction { .. } => "compaction",
    }
}

// ---- Overflow recovery: one compaction, one retry (DESIGN.md §14.7.4) ----

#[derive(Clone)]
struct OverflowProbe {
    log: Arc<Mutex<Vec<ProviderRequest>>>,
    always_overflow: bool,
}

impl OverflowProbe {
    fn requests(&self) -> Vec<ProviderRequest> {
        self.log.lock().expect("log poisoned").clone()
    }
}

impl Provider for OverflowProbe {
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
            if is_compaction {
                let _ = out
                    .send(EngineSignal::TextDelta {
                        operation_id,
                        step,
                        text: "overflow-summary: goal kept; next step kept".to_owned(),
                    })
                    .await;
                let _ = out
                    .send(EngineSignal::Completed { operation_id, step })
                    .await;
                return;
            }
            if step == 1 || self.always_overflow {
                let _ = out
                    .send(EngineSignal::Failed {
                        operation_id,
                        step,
                        message: "provider error: This model's maximum context length is \
                                  exceeded"
                            .to_owned(),
                    })
                    .await;
                return;
            }
            let _ = out
                .send(EngineSignal::TextDelta {
                    operation_id,
                    step,
                    text: "retry answer".to_owned(),
                })
                .await;
            let _ = out
                .send(EngineSignal::Completed { operation_id, step })
                .await;
        }
    }
}

#[tokio::test]
async fn context_overflow_compacts_once_and_retries() {
    let probe = OverflowProbe {
        log: Arc::new(Mutex::new(Vec::new())),
        always_overflow: false,
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(probe.clone(), ToolRegistry::default(), store.clone());
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "the retried step must complete the operation: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Step 1: plain. Step 2: the overflow compaction. Step 3: the retry
    // over the summary baseline.
    let requests = probe.requests();
    assert_eq!(requests.len(), 3, "compact once, retry once: {requests:?}");
    assert!(
        requests[2]
            .plan
            .messages
            .iter()
            .any(|m| m.prompt_text().contains("overflow-summary")),
        "the retry must project the summary baseline"
    );
    let loaded = store.load(session_id).await.expect("load");
    assert!(
        loaded
            .entries
            .iter()
            .any(|(_, entry)| matches!(entry, crate::session::SessionEntry::Compaction { .. })),
        "the overflow compaction must be durable"
    );
}

#[tokio::test]
async fn repeated_overflow_fails_visibly() {
    let probe = OverflowProbe {
        log: Arc::new(Mutex::new(Vec::new())),
        always_overflow: true,
    };
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_store(probe.clone(), ToolRegistry::default(), store.clone());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFailed { .. })),
        "a second overflow must fail visibly: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Exactly one compaction happened; the retry's failure was terminal.
    let requests = probe.requests();
    assert_eq!(requests.len(), 3, "no compaction loop: {requests:?}");
    let summarize_steps = requests
        .iter()
        .filter(|request| {
            request
                .plan
                .messages
                .iter()
                .any(|m| m.prompt_text().contains("Summarize the conversation"))
        })
        .count();
    assert_eq!(summarize_steps, 1, "only one summarize step may run");
}

// ---- MCP service (DESIGN.md §19) ----

fn fake_mcp_server() -> crate::ServerDef {
    let script = format!(
        "{}/tests/fixtures/fake_mcp_server.py",
        env!("CARGO_MANIFEST_DIR")
    );
    crate::ServerDef {
        name: "fake".to_owned(),
        command: "python3".to_owned(),
        args: vec![script],
    }
}

fn restarting_mcp_server(marker: &std::path::Path) -> crate::ServerDef {
    let script = format!(
        "{}/tests/fixtures/restarting_mcp_server.py",
        env!("CARGO_MANIFEST_DIR")
    );
    crate::ServerDef {
        name: "restarting".to_owned(),
        command: "python3".to_owned(),
        args: vec![script, marker.to_string_lossy().into_owned()],
    }
}

#[tokio::test]
async fn mcp_server_publishes_and_serves_tools_through_the_catalog() {
    let catalog = crate::ToolCatalog::default();
    catalog.activate_mcp_server("fake");
    crate::McpService::new()
        .start_into(&[fake_mcp_server()], &catalog)
        .await;

    // Published under a namespaced scope, visible to model steps.
    let specs = catalog.specs();
    let echo = specs
        .iter()
        .find(|spec| spec.name == "fake__echo")
        .expect("the fake server's echo tool must be registered");
    assert_eq!(echo.description, "Echo the message back");

    // Invocation through the normal Tool contract.
    let outcome = catalog
        .execute(
            "fake__echo",
            &json!({ "message": "hello" }),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.output);
    assert_eq!(outcome.output, "echo: hello");

    // Server-side failures stay model-visible tool errors.
    let outcome = catalog
        .execute(
            "fake__echo",
            &json!({ "message": "hi", "fail": true }),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(outcome.is_error);
    assert!(outcome.output.contains("forced failure"));
    catalog.close().await.expect("catalog close");
    assert!(catalog.get("fake__echo").is_none());
}

#[tokio::test]
async fn mcp_peer_restarts_after_discovery_crash_with_a_bounded_delay() {
    let temp = tempfile::tempdir().expect("tempdir");
    let marker = temp.path().join("restarted");
    let catalog = crate::ToolCatalog::default();
    catalog.activate_mcp_server("restarting");
    crate::McpService::new()
        .start_into(&[restarting_mcp_server(&marker)], &catalog)
        .await;

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut outcome = None;
    while Instant::now() < deadline {
        if marker.exists() && catalog.get("restarting__echo").is_some() {
            let candidate = catalog
                .execute(
                    "restarting__echo",
                    &json!({"message":"after restart"}),
                    CancellationToken::new(),
                )
                .await;
            if !candidate.is_error {
                outcome = Some(candidate);
                break;
            }
        }
        sleep(Duration::from_millis(25)).await;
    }
    let outcome = outcome.expect("the peer must recover after its first discovery crash");
    assert_eq!(outcome.output, "echo: after restart");
}

#[tokio::test]
async fn broken_mcp_server_never_blocks_startup() {
    let catalog = crate::ToolCatalog::default();
    catalog.activate_mcp_server("fake");
    let mut defs = vec![fake_mcp_server()];
    defs.push(crate::ServerDef {
        name: "missing".to_owned(),
        command: "/nonexistent/ion-missing-binary".to_owned(),
        args: vec![],
    });
    crate::McpService::new().start_into(&defs, &catalog).await;
    assert!(
        catalog.specs().iter().any(|s| s.name == "fake__echo"),
        "the healthy server still publishes"
    );
}

#[tokio::test]
async fn mcp_tool_flows_through_the_normal_operation_path() {
    // Catalog with the fake server's tool published.
    let catalog = crate::ToolCatalog::default();
    catalog.activate_mcp_server("fake");
    crate::McpService::new()
        .start_into(&[fake_mcp_server()], &catalog)
        .await;

    // Scripted model: call the MCP tool, then summarize the result.
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "fake__echo".to_owned(),
            arguments: json!({ "message": "through the runtime" }),
        },
        ScriptedMessage::text("done"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    // AllowlistPolicy is the documented grant mechanism (§17.2).
    let policy = Arc::new(AllowlistPolicy::new(["fake__echo"]));
    let runtime = Runtime::start_with_policy(provider, catalog, store.clone(), policy);
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // The remote effect ran through admission/policy/recovery like any
    // native tool and its output reached the next model step.
    let loaded = store.load(session_id).await.expect("load");
    // user -> assistant -> tool call -> tool result -> assistant(done)
    assert_eq!(loaded.entries.len(), 5);
    let last = &loaded.entries.last().expect("entries").1;
    assert!(
        serde_json::to_string(last)
            .map(|text| text.contains("done"))
            .unwrap_or(false),
        "the continuation step must see the tool output: {last:?}"
    );
}

#[tokio::test]
async fn default_policy_requires_approval_for_mcp_tools() {
    let catalog = crate::ToolCatalog::default();
    catalog.activate_mcp_server("fake");
    crate::McpService::new()
        .start_into(&[fake_mcp_server()], &catalog)
        .await;

    let provider = ScriptedProvider::new(vec![ScriptedMessage::ToolCall {
        name: "fake__echo".to_owned(),
        arguments: json!({ "message": "hi" }),
    }]);
    let store = SessionStore::open_in_memory().expect("store");
    // DefaultPolicy is the runtime default: unbounded remote effects
    // need an explicit grant.
    let runtime = Runtime::start_with_store(provider, catalog, store);
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(
            |e| matches!(e, RuntimeEvent::OperationApprovalRequired { tool, .. }
                if tool == "fake__echo")
        ),
        "unapproved MCP tools terminate with ApprovalRequired semantics: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

// ---- Runtime budgets (§20.5) ----

#[tokio::test]
async fn model_step_budget_fails_the_operation_visibly() {
    // The model keeps requesting tool calls; the budget stops the loop
    // after one step.
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo one" }),
        },
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo two" }),
        },
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_budgeted(
        provider,
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
        crate::RuntimeBudget {
            max_model_steps: Some(1),
            max_tool_calls: None,
        },
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(
            |e| matches!(e, RuntimeEvent::OperationFailed { message, .. }
                if message.contains("budget"))
        ),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let loaded = store.load(session_id).await.expect("load");
    // The failed operation is durable: it appears in the operation
    // records with a Failed outcome, not just as a live event.
    assert!(
        loaded.operations.iter().any(|op| {
            serde_json::to_string(&op.latest)
                .map(|text| text.contains("budget"))
                .unwrap_or(false)
        }),
        "failed outcome persisted"
    );
}

#[tokio::test]
async fn tool_call_budget_denies_further_tools_model_visibly() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo one" }),
        },
        ScriptedMessage::ToolCall {
            name: "bash".to_owned(),
            arguments: json!({ "command": "echo two" }),
        },
        ScriptedMessage::text("done"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_budgeted(
        provider,
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
        crate::RuntimeBudget {
            max_model_steps: None,
            max_tool_calls: Some(1),
        },
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "the model can finish its turn after denials: {recorded:?}"
    );
    let session_id = runtime.session_id();
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Exactly one tool effect was admitted; the second call settled as
    // a model-visible denial.
    let loaded = store.load(session_id).await.expect("load");
    let tool_intents = loaded
        .entries
        .iter()
        .filter(|(_, entry)| {
            serde_json::to_string(entry)
                .map(|text| text.contains("echo two"))
                .unwrap_or(false)
        })
        .count();
    assert_eq!(tool_intents, 1, "second call denied, first admitted");
}

// ---- Bounded child delegation (§20, Step 7) ----

fn delegate_tool(
    store: SessionStore,
    child_script: Vec<ScriptedMessage>,
    parent: crate::SessionId,
    budget: crate::RuntimeBudget,
) -> Arc<dyn crate::tool::Tool> {
    Arc::new(crate::DelegateTool::new(
        crate::DelegateConfig {
            store,
            make_provider: Arc::new(move || ScriptedProvider::new(child_script.clone())),
            make_provider_for_model: None,
            max_active_children: 4,
            child_budget: budget,
            trusted_resources: Vec::new(),
        },
        parent,
    ))
}

/// Extract `session-<uuid>` references from a delegate result.
fn child_ids(output: &str) -> Vec<crate::SessionId> {
    output
        .split("[child session: ")
        .skip(1)
        .filter_map(|part| {
            let end = part.find(']')?;
            crate::ids::SessionId::parse(part[..end].trim_start_matches("session-"))
        })
        .collect()
}

#[tokio::test]
async fn two_read_only_children_run_and_report_lineage() {
    let child_script = vec![ScriptedMessage::text("child answer")];
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "delegate".to_owned(),
            arguments: json!({
                "children": [
                    { "objective": "investigate a" },
                    { "objective": "investigate b", "context": "seed text" }
                ]
            }),
        },
        ScriptedMessage::text("done"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::default();
    let runtime = Runtime::start_with_store(provider, catalog.clone(), store.clone());
    let parent_id = runtime.session_id();
    catalog.register_scope(
        "delegate",
        vec![delegate_tool(
            store.clone(),
            child_script,
            parent_id,
            crate::RuntimeBudget::unbounded(),
        )],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("fan out").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // The tool result reached the parent transcript with both children
    // referenced.
    let loaded = store.load(parent_id).await.expect("load");
    let tool_output = loaded
        .entries
        .iter()
        .find_map(|(_, entry)| {
            serde_json::to_string(entry)
                .ok()
                .filter(|text| text.contains("child answer"))
        })
        .expect("child results in parent transcript");

    // Both children are durable sessions with lineage to the parent.
    let ids = child_ids(&tool_output);
    assert_eq!(ids.len(), 2, "{tool_output}");
    for child in ids {
        let child_loaded = store.load(child).await.expect("child session");
        assert_eq!(child_loaded.session.parent_session_id, Some(parent_id));
    }
}

#[tokio::test]
async fn fork_context_and_model_override_are_explicit() {
    let child_script = vec![ScriptedMessage::text("child answer")];
    let parent_provider = crate::SwitchingProvider::new(
        "parent-model",
        ScriptedProvider::new(vec![
            ScriptedMessage::text("parent answer"),
            ScriptedMessage::tool(
                "delegate",
                json!({
                    "children": [{
                        "objective": "continue the parent investigation",
                        "context_mode": "fork_context",
                        "model_override": "child-model"
                    }]
                }),
            ),
            ScriptedMessage::text("done"),
        ]),
    );
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::default();
    let runtime = Runtime::start_with_store(parent_provider, catalog.clone(), store.clone());
    let parent_id = runtime.session_id();
    let override_script = child_script.clone();
    catalog.register_scope(
        "delegate",
        vec![Arc::new(crate::DelegateTool::new(
            crate::DelegateConfig {
                store: store.clone(),
                make_provider: Arc::new(move || {
                    crate::SwitchingProvider::new(
                        "default-child-model",
                        ScriptedProvider::new(vec![ScriptedMessage::text("wrong child model")]),
                    )
                }),
                make_provider_for_model: Some(Arc::new(move |model| {
                    crate::SwitchingProvider::new(
                        model,
                        ScriptedProvider::new(override_script.clone()),
                    )
                })),
                max_active_children: 4,
                child_budget: crate::RuntimeBudget::unbounded(),
                trusted_resources: Vec::new(),
            },
            parent_id,
        ))],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session
        .submit_if_idle("parent prompt")
        .await
        .expect("submit");
    collect_until_terminal(&mut events)
        .await
        .expect("parent turn");
    session
        .submit_if_idle("delegate with a fork")
        .await
        .expect("submit");
    collect_until_terminal(&mut events)
        .await
        .expect("delegate turn");
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let parent = store.load(parent_id).await.expect("parent session");
    let child_id = parent
        .entries
        .iter()
        .find_map(|(_, entry)| {
            serde_json::to_string(entry)
                .ok()
                .filter(|text| text.contains("child answer"))
                .and_then(|text| child_ids(&text).into_iter().next())
        })
        .expect("fork child reference");
    let child = store.load(child_id).await.expect("child session");
    assert_eq!(child.session.initial_model_ref, "child-model");
    let prompt = child
        .entries
        .iter()
        .find_map(|(_, entry)| match entry {
            crate::SessionEntry::UserMessage { text } => Some(text),
            _ => None,
        })
        .expect("child objective is durable");
    assert!(prompt.contains("continue the parent investigation"));
    assert!(prompt.contains("[Explicit fork of parent semantic context]"));
    assert!(prompt.contains("parent prompt"));
    assert!(prompt.contains("parent answer"));
}

#[tokio::test]
async fn child_cannot_widen_capabilities() {
    // The child's provider asks for bash; the read-only catalog has no
    // bash and the gate denies the unknown tool model-visibly.
    let child_script = vec![ScriptedMessage::ToolCall {
        name: "bash".to_owned(),
        arguments: json!({ "command": "rm -rf /" }),
    }];
    let provider = ScriptedProvider::new(vec![ScriptedMessage::ToolCall {
        name: "delegate".to_owned(),
        arguments: json!({ "children": [{ "objective": "try to escape" }] }),
    }]);
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::default();
    let runtime = Runtime::start_with_store(provider, catalog.clone(), store.clone());
    let parent_id = runtime.session_id();
    catalog.register_scope(
        "delegate",
        vec![delegate_tool(
            store.clone(),
            child_script,
            parent_id,
            crate::RuntimeBudget::unbounded(),
        )],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("escape").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "{recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let loaded = store.load(parent_id).await.expect("load");
    let tool_output = loaded
        .entries
        .iter()
        .find_map(|(_, entry)| {
            serde_json::to_string(entry)
                .ok()
                .filter(|text| text.contains("child failed"))
        })
        .expect("the escape attempt fails visibly");
    assert!(
        tool_output.contains("unknown tool") || tool_output.contains("approval"),
        "denial is about the capability, not a crash: {tool_output}"
    );
}

#[tokio::test]
async fn budget_stops_a_runaway_child() {
    // The child would loop forever; its budget stops it after one
    // model step. The parent still finishes.
    let looping_child = || {
        ScriptedProvider::new(vec![
            ScriptedMessage::ToolCall {
                name: "read".to_owned(),
                arguments: json!({ "path": "Cargo.toml" }),
            },
            ScriptedMessage::ToolCall {
                name: "read".to_owned(),
                arguments: json!({ "path": "Cargo.toml" }),
            },
        ])
    };
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::ToolCall {
            name: "delegate".to_owned(),
            arguments: json!({ "children": [{ "objective": "loop" }] }),
        },
        ScriptedMessage::text("gave up on the child"),
    ]);
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::default();
    let runtime = Runtime::start_with_store(provider, catalog.clone(), store.clone());
    let parent_id = runtime.session_id();
    catalog.register_scope(
        "delegate",
        vec![Arc::new(crate::DelegateTool::new(
            crate::DelegateConfig {
                store: store.clone(),
                make_provider: Arc::new(looping_child),
                make_provider_for_model: None,
                max_active_children: 4,
                child_budget: crate::RuntimeBudget {
                    max_model_steps: Some(1),
                    max_tool_calls: None,
                },
                trusted_resources: Vec::new(),
            },
            parent_id,
        ))],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("run away").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "parent survives a failed child: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let loaded = store.load(parent_id).await.expect("load");
    let tool_output = loaded
        .entries
        .iter()
        .find_map(|(_, entry)| {
            serde_json::to_string(entry)
                .ok()
                .filter(|text| text.contains("budget exceeded"))
        })
        .expect("budget failure surfaces to the parent");
    assert!(tool_output.contains("child session"));
}

/// A provider that records the cancellation tokens it is given and
/// then waits for cancellation - a child that hangs forever unless the
/// parent's cancel propagates (§20.6).
#[derive(Clone)]
struct HangingProvider {
    tokens: Arc<std::sync::Mutex<Vec<tokio_util::sync::CancellationToken>>>,
}

impl crate::Provider for HangingProvider {
    fn run(
        &self,
        request: crate::ProviderRequest,
        cancel: tokio_util::sync::CancellationToken,
        out: tokio::sync::mpsc::Sender<crate::EngineSignal>,
    ) -> impl Future<Output = ()> + Send {
        self.tokens.lock().expect("tokens").push(cancel.clone());
        async move {
            cancel.cancelled().await;
            let _ = out
                .send(crate::EngineSignal::Cancelled {
                    operation_id: request.operation_id,
                    step: request.step,
                })
                .await;
        }
    }
}

#[tokio::test]
async fn parent_cancel_cancels_running_children() {
    let tokens: Arc<std::sync::Mutex<Vec<tokio_util::sync::CancellationToken>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let spy_tokens = Arc::clone(&tokens);
    let provider = ScriptedProvider::new(vec![ScriptedMessage::ToolCall {
        name: "delegate".to_owned(),
        arguments: json!({ "children": [{ "objective": "hang" }] }),
    }]);
    let store = SessionStore::open_in_memory().expect("store");
    let catalog = crate::ToolCatalog::default();
    let runtime = Runtime::start_with_store(provider, catalog.clone(), store.clone());
    let parent_id = runtime.session_id();
    catalog.register_scope(
        "delegate",
        vec![Arc::new(crate::DelegateTool::new(
            crate::DelegateConfig {
                store: store.clone(),
                make_provider: Arc::new(move || HangingProvider {
                    tokens: Arc::clone(&spy_tokens),
                }),
                make_provider_for_model: None,
                max_active_children: 4,
                child_budget: crate::RuntimeBudget::unbounded(),
                trusted_resources: Vec::new(),
            },
            parent_id,
        ))],
    );

    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("cancel me").await.expect("submit");
    // Wait for the child provider to record its token, then cancel.
    for _ in 0..100 {
        if !tokens.lock().expect("tokens").is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !tokens.lock().expect("tokens").is_empty(),
        "child provider started"
    );
    session.cancel(operation_id).await.expect("cancel");

    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationCancelled { .. })),
        "{recorded:?}"
    );
    // §20.6: the descendant's token fired with the parent's.
    for _ in 0..100 {
        if tokens
            .lock()
            .expect("tokens")
            .iter()
            .all(|token| token.is_cancelled())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        tokens
            .lock()
            .expect("tokens")
            .iter()
            .all(|token| token.is_cancelled()),
        "parent cancellation must reach descendants"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

// ---- Dogfood round 2 regressions ----

#[tokio::test]
async fn find_matches_nested_paths_against_the_search_root() {
    // Found live: nested files were tested as bare names because the
    // walk stripped each visited directory instead of the search root.
    let dir = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(dir.path().join("crates").join("alpha")).expect("mkdir");
    std::fs::create_dir_all(dir.path().join("crates").join("beta")).expect("mkdir");
    std::fs::write(dir.path().join("crates").join("alpha").join("C.toml"), "x").expect("write");
    std::fs::write(dir.path().join("crates").join("beta").join("C.toml"), "x").expect("write");
    std::fs::write(dir.path().join("top.toml"), "x").expect("write");

    let registry = crate::ToolRegistry::read_only(dir.path());
    let outcome = registry
        .execute(
            "find",
            &json!({ "pattern": "crates/*/C.toml" }),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.output);
    let mut found: Vec<&str> = outcome.output.lines().collect();
    found.sort_unstable();
    assert_eq!(
        found,
        vec!["crates/alpha/C.toml", "crates/beta/C.toml"],
        "{outcome:?}"
    );
}

#[tokio::test]
async fn path_resolution_containment_survives_a_relative_cwd() {
    // Found live: with registry cwd ".", normalization dropped the
    // CurDir component and every subpath read as an escape.
    let dir = tempfile::tempdir().expect("tmpdir");
    std::fs::create_dir_all(dir.path().join("crates")).expect("mkdir");
    std::fs::write(dir.path().join("crates").join("a.txt"), "x").expect("write");

    // "." as cwd: the delegate child configuration.
    let registry = crate::ToolRegistry::read_only(".");
    // The tempdir must be the process cwd for this probe.
    let prev = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(dir.path()).expect("chdir");
    let outcome = registry
        .execute(
            "find",
            &json!({ "path": "crates", "pattern": "*.txt" }),
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    std::env::set_current_dir(prev).expect("restore cwd");
    assert!(!outcome.is_error, "{}", outcome.output);
    assert!(outcome.output.contains("a.txt"), "{outcome:?}");
}

// ---- Display-only surfaces: thinking deltas + tool previews ----

#[tokio::test]
async fn thinking_deltas_stream_but_stay_out_of_the_transcript() {
    let runtime = start_runtime(
        ScriptedProvider::new(vec![
            ScriptedMessage::Thinking {
                text: "pondering the request".to_owned(),
            },
            ScriptedMessage::text("answer\n"),
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("hi").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");

    let mut thinking = Vec::new();
    for event in &recorded {
        if let RuntimeEvent::ThinkingDelta { text, .. } = event {
            thinking.push(text.clone());
        }
    }
    assert_eq!(thinking, vec!["pondering the request".to_owned()]);
    // Text is unaffected and the operation finished cleanly.
    assert_eq!(texts(&recorded), vec!["answer\n".to_owned()]);
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));
    // Thinking never becomes a durable entry (display-only surface).
    let snapshot = session.snapshot().await.expect("snapshot");
    assert!(snapshot.entries.iter().all(|entry| match entry {
        SessionEntry::AssistantMessage { text } => text == "answer\n",
        _ => true,
    }));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn tool_settled_event_carries_a_bounded_preview() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool(
            "bash",
            json!({"command":"for i in $(seq 1 60); do echo line-$i; done"}),
        ),
        ScriptedMessage::text("done\n"),
    ]);
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");

    let previews: Vec<Option<String>> = recorded
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::ToolSettled { preview, .. } => Some(preview.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(previews.len(), 1);
    let preview = previews[0].as_ref().expect("bash produced output");
    // Tail-truncated: keeps the end, bounds the head.
    assert!(preview.contains("line-60"));
    assert!(!preview.contains("line-1\n"));
    assert!(preview.starts_with('…'));
    assert!(preview.lines().count() <= 21);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn switching_provider_swaps_models_at_step_boundaries() {
    use crate::provider::SwitchingProvider;

    // Two scripted providers with distinguishable replies; a switch
    // mid-operation lands on the next step's reply.
    let provider = SwitchingProvider::new(
        "a",
        ScriptedProvider::new(vec![
            ScriptedMessage::text("from-a\n"),
            ScriptedMessage::text("still-a\n"),
        ]),
    );
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(
        texts(&recorded),
        vec!["from-a\n".to_owned(), "still-a\n".to_owned()]
    );

    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // A durable mid-session change lands on the next operation's steps.
    // The host factory resolves each exact model id; selection itself
    // lives in the session.
    let make: std::sync::Arc<dyn Fn(String) -> ScriptedProvider + Send + Sync> =
        std::sync::Arc::new(|m| {
            ScriptedProvider::new(vec![ScriptedMessage::text(format!("switched-to-{m}\n"))])
        });
    let provider = SwitchingProvider::switchable(
        "a",
        ScriptedProvider::new(vec![ScriptedMessage::text("first\n")]),
        make,
    );
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(texts(&recorded), vec!["first\n".to_owned()]);

    let previous = session.switch_model("b").await.expect("switch");
    assert_eq!(previous, "a");
    session.submit_if_idle("again").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(texts(&recorded), vec!["switched-to-b\n".to_owned()]);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn model_switch_refuses_an_id_the_resolver_cannot_build() {
    // A fixed provider accepts only its own model; the refusal happens
    // before any durable change.
    use crate::provider::SwitchingProvider;

    let provider = SwitchingProvider::new("a", ScriptedProvider::echo());
    let runtime = start_runtime(provider, ToolRegistry::default());
    let session = runtime.session();
    let err = session.switch_model("nonexistent").await;
    assert!(matches!(err, Err(crate::CommandError::UnsupportedModel(_))));
    // The accepted model is unchanged and still works.
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(texts(&recorded), vec!["ok".to_owned()]);

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn reasoning_draft_clears_on_provider_failure() {
    // Found in review: only the Completed path cleared the live
    // reasoning buffer; failure paths retained stale reasoning into
    // the next operation.
    let runtime = start_runtime(
        ScriptedProvider::new(vec![
            ScriptedMessage::Thinking {
                text: "pondering".to_owned(),
            },
            ScriptedMessage::Fail {
                message: "provider exploded".to_owned(),
            },
        ]),
        ToolRegistry::default(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFailed { .. })
    ));

    // The next operation must not resurrect the failed step's reasoning
    // (the exhausted script completes silently with no deltas).
    session.submit_if_idle("again").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .all(|event| !matches!(event, RuntimeEvent::ThinkingDelta { .. }))
    );

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn crash_recovery_replays_with_the_persisted_model_not_the_launch_default() {
    // DESIGN.md §14.8: the restart's launch default must never
    // substitute for a pending step's frozen model snapshot.
    use crate::provider::SwitchingProvider;
    use std::sync::Arc;

    let db = temp_db("model-recovery");
    let store = SessionStore::open(&db).expect("store");
    let make: Arc<dyn Fn(String) -> ScriptedProvider + Send + Sync> = Arc::new(|m: String| {
        ScriptedProvider::new(vec![ScriptedMessage::text(format!("served-by-{m}\n"))])
    });
    let provider = SwitchingProvider::switchable(
        "a",
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "never arrives",
        )]),
        Arc::clone(&make),
    );
    let runtime = Runtime::start_with_policy(
        provider,
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    session.submit_if_idle("goal").await.expect("submit");
    wait_for_state(&session, |state| {
        matches!(state, OperationState::AssistantEffectPending)
    })
    .await;

    // Process loss mid-model-step under model "a".
    runtime.crash();
    drop(runtime);
    drop(session);

    // Reopen composed for model "b": recovery must still serve the
    // pending step from "a" via the resolver, never from "b".
    let make_reopen: Arc<dyn Fn(String) -> ScriptedProvider + Send + Sync> = Arc::new(|m| {
        if m == "a" {
            ScriptedProvider::new(vec![ScriptedMessage::text("recovered-under-a\n")])
        } else {
            ScriptedProvider::new(vec![ScriptedMessage::text(format!("served-by-{m}\n"))])
        }
    });
    let reopen_provider = SwitchingProvider::switchable(
        "b",
        ScriptedProvider::new(vec![ScriptedMessage::text("served-by-b\n")]),
        make_reopen,
    );
    let runtime = Runtime::open_session(
        reopen_provider,
        ToolRegistry::default(),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    // The persisted selection survives restart: the "b" launch default
    // never becomes this session's authority.
    let snapshot = session.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.model_ref, "a");

    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded.iter().any(|e| matches!(
            e,
            RuntimeEvent::AssistantTextDelta { text, .. } if text == "recovered-under-a\n"
        )),
        "recovery must replay the persisted model: {recorded:?}"
    );
    assert!(
        !recorded.iter().any(|e| matches!(
            e,
            RuntimeEvent::AssistantTextDelta { text, .. } if text.contains("served-by")
        )),
        "the launch default must not serve the recovered step"
    );
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::OperationFinished { .. })
    ));

    // §11.3: each attempt row names the exact model that ran.
    let connection = rusqlite::Connection::open(&db).expect("open db");
    let refs: Vec<(String, String, String, String, String, String)> = connection
        .prepare(
            "SELECT model_ref, capability_snapshot_id, context_manifest_id,
                    capabilities, context_fingerprint, cache_expectation
             FROM model_steps ORDER BY created_at",
        )
        .expect("prepare")
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .expect("query")
        .collect::<Result<Vec<_>, _>>()
        .expect("rows");
    assert_eq!(refs.len(), 2, "{refs:?}");
    assert!(refs.iter().all(
        |(model, snapshot, manifest, capabilities, fingerprint, expectation)| {
            let capabilities: crate::provider::ModelCapabilities =
                serde_json::from_str(capabilities).expect("capabilities json");
            model == "a"
                && snapshot.len() == 64
                && manifest.len() == 64
                && fingerprint.len() == 64
                && expectation == "unsupported"
                && capabilities.tool_calls
                && !capabilities.prompt_cache
        }
    ));
    let snapshot_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM capability_snapshots", [], |row| {
            row.get(0)
        })
        .expect("snapshot count");
    let manifest_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM context_manifests", [], |row| {
            row.get(0)
        })
        .expect("manifest count");
    assert_eq!(snapshot_count, 1);
    assert_eq!(manifest_count, 1);
    let effect_inputs: Vec<String> = connection
        .prepare("SELECT effective_input FROM effects WHERE kind = 'model_step'")
        .expect("prepare effects")
        .query_map([], |row| row.get(0))
        .expect("query effects")
        .collect::<Result<Vec<_>, _>>()
        .expect("effect rows");
    assert!(effect_inputs.iter().all(|input| {
        let value: serde_json::Value = serde_json::from_str(input).expect("effect json");
        value.get("tools").is_none()
            && value.get("capability_snapshot").is_none()
            && value.get("context_manifest").is_none()
    }));

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn event_lag_is_signaled_reliably_and_snapshot_carries_live_draft() {
    // DESIGN.md §21.4: a full queue cannot be used to enqueue its own
    // overflow signal, so lag is detected by the receiver against the
    // ring tail; the fresh snapshot then carries the authoritative
    // draft for reconstruction.
    let mut messages: Vec<ScriptedMessage> = (0..80)
        .map(|i| ScriptedMessage::text(format!("d{i} ")))
        .collect();
    // Keep the operation in flight so the live draft survives until
    // the resubscribe.
    messages.push(ScriptedMessage::delayed(Duration::from_secs(30), "never"));
    let runtime = start_runtime(ScriptedProvider::new(messages), ToolRegistry::default());
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("goal").await.expect("submit");

    // Do not read while 80 deltas overflow the 64-slot ring.
    sleep(Duration::from_millis(300)).await;
    let first = timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("recv");
    assert!(
        matches!(&first, Err(RuntimeError::SubscriptionLagged)),
        "lag must be delivered reliably, got {first:?}"
    );

    let (fresh_snapshot, mut fresh) = session.subscribe().await.expect("resubscribe");
    let live = fresh_snapshot.live.as_ref().expect("live state present");
    assert!(live.draft_text.contains("d0 "));
    assert!(live.draft_text.contains("d79 "));
    assert!(live.pending_tools.is_empty());

    // The fresh stream continues from the tail without lag errors;
    // only the still-pending delayed delta remains when we stop.
    while let Ok(Ok(_)) = timeout(Duration::from_millis(100), fresh.recv()).await {}

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_during_lag_is_visible_after_resubscribe() {
    // §21.4: critical lifecycle events are never silently dropped. A
    // subscriber that lags and resubscribes must see the terminal
    // state in the snapshot, not a stream that just goes quiet.
    let mut messages: Vec<ScriptedMessage> = (0..80)
        .map(|i| ScriptedMessage::text(format!("d{i} ")))
        .collect();
    messages.push(ScriptedMessage::delayed(Duration::from_secs(30), "never"));
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_with_policy(
        ScriptedProvider::new(messages),
        ToolRegistry::default(),
        store.clone(),
        permissive_policy(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    let operation_id = session.submit_if_idle("goal").await.expect("submit");
    // Overflow the ring without reading, then cancel mid-step.
    sleep(Duration::from_millis(300)).await;
    session.cancel(operation_id).await.expect("cancel");
    sleep(Duration::from_millis(200)).await;

    let first = timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("recv");
    assert!(matches!(first, Err(RuntimeError::SubscriptionLagged)));

    let (fresh, _events) = session.subscribe().await.expect("resubscribe");
    assert_eq!(fresh.operation, OperationStatus::Idle);
    assert!(fresh.live.is_none());
    for attempt in 0..5 {
        match store.load(runtime.session_id()).await {
            Ok(loaded) => {
                let (_, checkpoint) = &loaded.operations[0].latest;
                assert_eq!(
                    checkpoint.state,
                    OperationState::Finished(OperationOutcome::Cancelled)
                );
                break;
            }
            Err(err) => {
                eprintln!("attempt {attempt}: {err:?}");
                assert!(attempt < 4, "load never succeeded");
                sleep(Duration::from_millis(100)).await;
            }
        }
    }

    session.close().await.expect("close");
    runtime.join().await.expect("join");
}
