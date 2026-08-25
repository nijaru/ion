//! Transitions tests.

use super::support::*;

// ---- Pure operation transition core (DESIGN.md §30.1) ----

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
