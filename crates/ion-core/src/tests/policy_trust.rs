//! Policy trust tests.

use super::support::*;

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
    let has_call = loaded.entries.iter().any(
        |record| matches!(&record.entry, SessionEntry::ToolCall { call } if call.name == "bash"),
    );
    let has_result = loaded
        .entries
        .iter()
        .any(|record| matches!(&record.entry, SessionEntry::ToolResult { .. }));
    assert!(has_call, "the planned call stays visible");
    assert!(!has_result, "nothing executed");
}

// ---- Interactive durable approvals (DESIGN.md §17.4) ----

#[test]
fn request_approval_parks_tools_planned_without_committing_effects() {
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
    let bash = ToolCall {
        operation_id: machine.operation_id(),
        call_id: 1,
        name: "bash".to_owned(),
        arguments: json!({ "command": "echo hi" }),
    };
    let read = ToolCall {
        operation_id: machine.operation_id(),
        call_id: 2,
        name: "read".to_owned(),
        arguments: json!({ "path": "x" }),
    };
    machine
        .apply(Transition::ProviderCompleted {
            text: String::new(),
            tool_calls: vec![bash.clone(), read.clone()],
        })
        .expect("plan tools");

    // Parking commits no entries and no effect intent: nothing may
    // execute before the decision.
    let applied = machine.apply(Transition::RequestApproval).expect("park");
    assert_eq!(
        applied.state,
        OperationState::ApprovalPending {
            call: bash.clone(),
            pending: vec![read.clone()],
        }
    );
    assert!(
        applied.intents.is_empty(),
        "no effect intent before the decision"
    );
    assert!(applied.entries.is_empty());

    // Approval turns the exact staged call into the durable tool intent.
    let applied = machine.apply(Transition::ApproveCall).expect("approve");
    assert_eq!(
        applied.state,
        OperationState::ToolEffectPending {
            pending: vec![read.clone()]
        }
    );
    assert!(
        matches!(applied.intents[..], [EffectIntent::Tool { ref call }] if *call == bash),
        "the approved call is the one that executes: {:?}",
        applied.intents
    );

    // Every approval transition is invalid outside its state.
    let (mut other, _) = OperationMachine::accept(OperationId::generate(), "goal", Vec::new());
    assert!(other.apply(Transition::RequestApproval).is_err());
    assert!(other.apply(Transition::ApproveCall).is_err());
    assert!(other.apply(Transition::DenyCall).is_err());
}

#[test]
fn deny_call_records_model_visible_denial_and_keeps_queue() {
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
    machine
        .apply(Transition::ProviderCompleted {
            text: String::new(),
            tool_calls: vec![
                ToolCall {
                    operation_id: machine.operation_id(),
                    call_id: 1,
                    name: "bash".to_owned(),
                    arguments: json!({ "command": "echo hi" }),
                },
                ToolCall {
                    operation_id: machine.operation_id(),
                    call_id: 2,
                    name: "read".to_owned(),
                    arguments: json!({ "path": "x" }),
                },
            ],
        })
        .expect("plan tools");
    machine.apply(Transition::RequestApproval).expect("park");

    // Denying with calls remaining: the queue resumes and the denial is
    // model-visible so the model can choose another path (§17.4).
    let applied = machine.apply(Transition::DenyCall).expect("deny");
    assert!(
        matches!(applied.state, OperationState::ToolsPlanned { ref pending } if pending.len() == 1 && pending[0].name == "read")
    );
    assert!(
        matches!(applied.entries[..], [SessionEntry::ToolResult { result: ToolResult::Err { call_id, ref error, .. } }] if call_id == 1 && error == "user denied approval for this call"),
        "denial is a durable model-visible tool error: {:?}",
        applied.entries
    );
    assert!(applied.intents.is_empty());
}

#[test]
fn deny_last_parked_call_returns_control_to_the_model() {
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
    machine
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
    machine.apply(Transition::RequestApproval).expect("park");

    let applied = machine.apply(Transition::DenyCall).expect("deny");
    assert_eq!(applied.state, OperationState::NeedAssistant);
    assert_eq!(applied.entries.len(), 1);
}

#[test]
fn cancel_and_deny_while_parked_terminate_without_effects() {
    let parked = || {
        let (mut machine, _) =
            OperationMachine::accept(OperationId::generate(), "goal", Vec::new());
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
                    operation_id: machine.operation_id(),
                    call_id: 1,
                    name: "bash".to_owned(),
                    arguments: json!({ "command": "echo hi" }),
                }],
            })
            .expect("plan tools");
        machine.apply(Transition::RequestApproval).expect("park");
        machine
    };

    // A cancel request while parked terminates immediately: there is no
    // live effect the flag would otherwise wait on (§9.4).
    let mut machine = parked();
    let applied = machine
        .apply(Transition::CancelRequested)
        .expect("cancel request");
    assert_eq!(
        applied.state,
        OperationState::Finished(OperationOutcome::Cancelled)
    );
    assert!(applied.cancel_effects);

    // A cancel requested before parking (while the queue was forming)
    // carries through: a denial finishes cancelled.
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
    machine
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
    machine
        .apply(Transition::CancelRequested)
        .expect("cancel request");
    machine.apply(Transition::RequestApproval).expect("park");
    let applied = machine.apply(Transition::DenyCall).expect("deny");
    assert_eq!(
        applied.state,
        OperationState::Finished(OperationOutcome::Cancelled)
    );
    assert_eq!(applied.entries.len(), 1, "the denial stays model-visible");
}

#[test]
fn approval_required_also_terminates_a_parked_approval() {
    // Non-interactive hosts cannot decide a parked call; the same
    // terminal outcome applies from the parked state (DESIGN.md §17.2).
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
    machine
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
    machine.apply(Transition::RequestApproval).expect("park");

    let applied = machine
        .apply(Transition::ApprovalRequired {
            tool: "bash".to_owned(),
        })
        .expect("terminate parked approval");
    assert_eq!(
        applied.state,
        OperationState::Finished(OperationOutcome::ApprovalRequired {
            tool: "bash".to_owned()
        })
    );
}

#[tokio::test]
async fn interactive_runtime_parks_bash_and_approval_executes_it() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_interactive(
        ScriptedProvider::new(vec![
            ScriptedMessage::tool("bash", json!({ "command": "echo granted" })),
            ScriptedMessage::text("done"),
        ]),
        ToolRegistry::default(),
        store.clone(),
        Arc::new(crate::policy::DefaultPolicy),
        Vec::new(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");

    let parked = wait_for_park(&mut events).await;
    let RuntimeEvent::ApprovalPending {
        operation_id,
        tool,
        target,
        ..
    } = parked
    else {
        panic!("park event shape");
    };
    assert_eq!(tool, "bash");
    assert!(
        target
            .as_deref()
            .is_some_and(|t| t.contains("echo granted")),
        "the parked prompt names the exact staged invocation: {target:?}"
    );

    // The decision is durable before it returns.
    session
        .decide_approval(operation_id, true)
        .await
        .expect("approve");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ToolStarted { tool, .. } if tool == "bash")),
        "the approved call executes: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. }))
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Durable: the approved call ran exactly once and completed.
    let loaded = store.load(session_id).await.expect("load");
    let started = loaded
        .entries
        .iter()
        .filter(|record| matches!(&record.entry, SessionEntry::ToolResult { .. }))
        .count();
    assert_eq!(
        started, 1,
        "one tool result for the approved call: {loaded:?}"
    );
}

#[tokio::test]
async fn interactive_deny_is_model_visible_and_the_operation_continues() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_interactive(
        ScriptedProvider::new(vec![
            ScriptedMessage::tool("bash", json!({ "command": "echo hi" })),
            ScriptedMessage::text("fine, nothing else"),
        ]),
        ToolRegistry::default(),
        store.clone(),
        Arc::new(crate::policy::DefaultPolicy),
        Vec::new(),
    );
    let session_id = runtime.session_id();
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");

    let parked = wait_for_park(&mut events).await;
    let operation_id = parked
        .operation_id()
        .expect("park event carries the operation");
    session
        .decide_approval(operation_id, false)
        .await
        .expect("deny");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ToolStarted { .. })),
        "nothing executed: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationFinished { .. })),
        "the operation continues to completion after a denial: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    // Durable: the denial is a model-visible tool error; no result
    // exists for a call that never started.
    let loaded = store.load(session_id).await.expect("load");
    assert!(
        loaded.entries.iter().any(|record| {
            let entry = &record.entry;
            matches!(entry, SessionEntry::ToolResult { result: ToolResult::Err { error, .. } } if error.contains("denied"))
        }),
        "the denial is model-visible: {loaded:?}"
    );
}

#[tokio::test]
async fn interactive_parked_operation_cancels_directly() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_interactive(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "bash",
            json!({ "command": "echo hi" }),
        )]),
        ToolRegistry::default(),
        store.clone(),
        Arc::new(crate::policy::DefaultPolicy),
        Vec::new(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");

    let parked = wait_for_park(&mut events).await;
    let operation_id = parked
        .operation_id()
        .expect("park event carries the operation");
    session.cancel(operation_id).await.expect("cancel");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::OperationCancelled { .. })),
        "a parked operation cancels directly: {recorded:?}"
    );
    assert!(
        !recorded
            .iter()
            .any(|e| matches!(e, RuntimeEvent::ToolStarted { .. })),
        "nothing executed: {recorded:?}"
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[test]
fn decide_approval_rejects_states_without_a_parked_call() {
    // Non-interactive runtimes keep the fail-closed terminal outcome,
    // so these guards only matter where the decision can race; the
    // machine-level rejections are the contract.
    let (mut machine, _) = OperationMachine::accept(OperationId::generate(), "goal", Vec::new());
    assert!(machine.apply(Transition::ApproveCall).is_err());
    assert!(machine.apply(Transition::DenyCall).is_err());
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
    let denied = loaded.entries.iter().any(|record| {
        let entry = &record.entry;
        matches!(entry, SessionEntry::ToolResult {
            result: ToolResult::Err { error, .. },
        } if error.contains("denied by test policy"))
    });
    assert!(denied, "policy denial must be model-visible: {loaded:?}");
}

#[tokio::test]
async fn parked_edit_carries_a_diff_preview_for_the_approval_prompt() {
    let root = tempfile::tempdir().expect("root tempdir");
    std::fs::write(root.path().join("target.txt"), "alpha\nbeta\ngamma\n").expect("seed file");
    // Edit is outside the allowlist, so the interactive runtime parks
    // for a decision instead of executing.
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = Runtime::start_interactive(
        ScriptedProvider::new(vec![ScriptedMessage::tool(
            "edit",
            json!({"path":"target.txt","old_str":"beta","new_str":"BETA"}),
        )]),
        ToolCatalog::with_cwd(root.path()),
        store.clone(),
        Arc::new(AllowlistPolicy::new(["read"])),
        Vec::new(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("go").await.expect("submit");
    let mut preview = None;
    let mut parked_operation = None;
    for _ in 0..40 {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("approval park timed out");
        if let RuntimeEvent::ApprovalPending {
            tool,
            preview: p,
            operation_id,
            ..
        } = event.expect("event")
        {
            assert_eq!(tool, "edit");
            preview = p;
            parked_operation = Some(operation_id);
            break;
        }
    }
    let preview = preview.expect("parked edit must carry a preview");
    assert!(preview.contains("-beta"), "removed line missing: {preview}");
    assert!(preview.contains("+BETA"), "added line missing: {preview}");
    assert!(preview.contains("@@ "), "hunk header missing: {preview}");
    // Declining leaves the file untouched: the preview was display-only.
    session
        .decide_approval(parked_operation.expect("parked operation"), false)
        .await
        .expect("decline the parked edit");
    session.close().await.expect("close");
    runtime.join().await.expect("join");
    let content = std::fs::read_to_string(root.path().join("target.txt")).expect("read back");
    assert_eq!(content, "alpha\nbeta\ngamma\n");
}
