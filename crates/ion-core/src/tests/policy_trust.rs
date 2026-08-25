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
