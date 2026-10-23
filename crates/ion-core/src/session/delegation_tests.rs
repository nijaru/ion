use super::*;
use crate::{DelegationOutcome, DelegationSite, DelegationSpec};
use ion_ai::{Content, Message, Role, ToolCall, ToolSpec};

fn pending(composed: bool) -> (PathBuf, Session, RecoveryBinding, crate::DelegationRecord) {
    pending_for(composed, crate::DelegationOwnership::Foreground)
}

pub(super) fn pending_for(
    composed: bool,
    ownership: crate::DelegationOwnership,
) -> (PathBuf, Session, RecoveryBinding, crate::DelegationRecord) {
    let root = std::env::temp_dir().join(format!("ion-delegation-state-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let session = Session::create(root.join("parent.sqlite"), None).unwrap();
    let binding = RecoveryBinding {
        session: session.id(),
        authority: AuthorityScope::new("parent/v1").unwrap(),
        fingerprint: [42; 32],
    };
    let model = ModelRef {
        provider: "fixture".into(),
        model: "fixture".into(),
    };
    session
        .admit_turn(
            Message::user_input("delegate".into(), []),
            model.clone(),
            None,
            Some(binding.clone()),
        )
        .unwrap();
    let call = ToolCall {
        id: "call".into(),
        name: if composed { "code_mode" } else { "delegate" }.into(),
        arguments: serde_json::json!({}),
        raw_arguments: None,
    };
    session
        .record_assistant(
            1,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(call.clone())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let tool = session.tool_occurrence(1, "call").unwrap();
    if composed {
        session
            .record_child_intent(
                1,
                crate::ChildIntent {
                    parent: tool,
                    child: 0,
                    call: ToolCall {
                        name: "delegate".into(),
                        ..call
                    },
                    definition: ToolSpec {
                        name: "delegate".into(),
                        description: String::new(),
                        input_schema: serde_json::json!({}),
                    },
                    activity: ToolActivity::external("delegate"),
                },
            )
            .unwrap();
    }
    let record = session
        .admit_delegation(
            DelegationSite {
                session: session.id(),
                turn: 1,
                tool,
                composed_child: composed.then_some(0),
            },
            DelegationSpec {
                context: crate::DelegationContext::Fresh,
                directory: root.join("children"),
                cwd: None,
                model,
                reasoning: Reasoning::ProviderDefault,
                execution_timeout_ms: None,
                authority: AuthorityScope::new("child/v1").unwrap(),
                input: Message::user_input("child".into(), []),
            },
            ownership,
        )
        .unwrap()
        .0;
    (root, session, binding, record)
}

#[test]
fn recovery_orders_lost_relationships_before_tools_and_rolls_back_as_one_batch() {
    for composed in [false, true] {
        let (root, session, binding, record) = pending(composed);
        let before = session.view().unwrap().entries;
        let result = crate::ToolSettlement {
            call_id: "call".into(),
            name: if composed { "code_mode" } else { "delegate" }.into(),
            outcome: crate::ToolOutcome::Unknown,
        };
        assert!(matches!(
            session.record_tool_settlement_with_context(1, result, None),
            Err(SessionError::InvalidHistory)
        ));
        if composed {
            assert!(matches!(
                session.record_child_outcome(1, record.site.tool, 0, crate::ChildOutcome::Unknown),
                Err(SessionError::InvalidHistory)
            ));
        }
        assert_eq!(session.view().unwrap().entries, before);
        let copy = session.clone_to(root.join("copy.sqlite")).unwrap();
        assert!(copy.view().unwrap().delegations().is_empty());
        assert!(copy.recovery_state().unwrap().is_none());
        drop(copy);
        let point = session.recovery_state().unwrap().unwrap().point;
        session.store.lock().unwrap().connection.execute_batch("CREATE TRIGGER fail_outer BEFORE INSERT ON entries WHEN json_extract(NEW.body,'$.kind') = 'tool_result' BEGIN SELECT RAISE(FAIL, 'injected'); END;").unwrap();
        assert!(session.resume_turn(point, &binding).is_err());
        assert_eq!(session.view().unwrap().entries, before);
        assert_eq!(session.recovery_state().unwrap().unwrap().point, point);
        session
            .store
            .lock()
            .unwrap()
            .connection
            .execute_batch("DROP TRIGGER fail_outer")
            .unwrap();
        session.resume_turn(point, &binding).unwrap();
        let view = session.view().unwrap();
        let suffix = &view.entries[before.len()..];
        assert!(matches!(
            suffix[0],
            SessionEntry::TurnResumed { turn: 1, .. }
        ));
        assert!(
            matches!(suffix[1], SessionEntry::DelegationSettled { child, outcome: DelegationOutcome::Unknown } if child == record.child)
        );
        if composed {
            assert!(matches!(
                suffix[2],
                SessionEntry::ChildToolResult {
                    outcome: crate::ChildOutcome::Unknown,
                    ..
                }
            ));
        }
        assert!(
            matches!(suffix.last(), Some(SessionEntry::ToolResult { result, .. }) if matches!(result.outcome, crate::ToolOutcome::Unknown))
        );
        assert_eq!(view.unfinished_turn, Some(1));
        assert_eq!(
            view.delegations()[0].outcome,
            Some(DelegationOutcome::Unknown)
        );
        assert!(
            !record.path.exists(),
            "recovery must not materialize a lost child"
        );
        drop(session);
        let reopened = Session::open(root.join("parent.sqlite")).unwrap();
        assert_eq!(reopened.view().unwrap().entries, view.entries);
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn materialization_uses_only_the_committed_identity_and_binding() {
    let (root, session, _, record) = pending(false);
    let materialize =
        |record: &crate::DelegationRecord| Session::materialize_delegation(record, Vec::new());
    let child = materialize(&record).unwrap();
    assert_eq!(child.id(), record.child);
    assert_eq!(
        child.view().unwrap().entries,
        [
            SessionEntry::ChildSessionOf {
                record: record.clone()
            },
            SessionEntry::ModelSelected {
                model: record.spec.model.clone()
            },
            SessionEntry::ReasoningSelected {
                reasoning: record.spec.reasoning
            },
        ]
    );
    assert!(
        materialize(&record).is_err(),
        "a live writer's lease must not be adopted"
    );
    drop(child);
    let mut changed = record.clone();
    changed.spec.authority = AuthorityScope::new("different/principal").unwrap();
    assert!(matches!(
        materialize(&changed),
        Err(SessionError::InvalidHistory)
    ));
    let child = materialize(&record).unwrap();
    assert_eq!(child.id(), record.child);
    drop(child);
    // A valid but different database at that exact locator is not adoptable.
    fs::remove_file(&record.path).unwrap();
    let unrelated = Session::create(&record.path, None).unwrap();
    let unrelated_id = unrelated.id();
    drop(unrelated);
    assert!(matches!(
        materialize(&record),
        Err(SessionError::InvalidHistory)
    ));
    assert_eq!(Session::inspect_id(&record.path).unwrap(), unrelated_id);
    fs::remove_file(&record.path).unwrap();
    let partial = Connection::open(&record.path).unwrap();
    initialize_new(&partial).unwrap();
    drop(partial);
    assert!(materialize(&record).is_err());
    let partial = Connection::open(&record.path).unwrap();
    assert_eq!(
        partial
            .query_row("SELECT count(*) FROM session", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0,
        "ambiguous initialization must not invent a header"
    );
    drop(partial);
    assert_eq!(session.view().unwrap().delegations()[0].record, record);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn repeated_child_identity_is_invalid_even_without_the_derived_index() {
    let (root, session, _, mut record) = pending(false);
    let (turn, _) = session
        .begin_turn("fresh work".into(), record.spec.model.clone())
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "next".into(),
                    name: "delegate".into(),
                    arguments: serde_json::json!({}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    record.site.turn = turn;
    record.site.tool = session.tool_occurrence(turn, "next").unwrap();
    let entry = SessionEntry::DelegationAdmitted { record };
    // The candidate is otherwise legal at this second occurrence. The derived
    // index is not the authority for uniqueness across historical relationships.
    let mut state = session.store.lock().unwrap().state.clone();
    state
        .apply(&entry, &mut Vec::new(), &BTreeSet::new(), &mut Vec::new())
        .unwrap();
    drop(session);
    let connection = Connection::open(root.join("parent.sqlite")).unwrap();
    connection
        .execute_batch("DROP INDEX delegation_children")
        .unwrap();
    connection
        .execute(
            "INSERT INTO entries(body) VALUES (?1)",
            params![serde_json::to_vec(&entry).unwrap()],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        Session::open(root.join("parent.sqlite")),
        Err(SessionError::InvalidHistory)
    ));
    assert!(matches!(
        Session::inspect_metadata(root.join("parent.sqlite")),
        Err(SessionError::InvalidHistory)
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn deadline_settlement_cannot_hide_failure_or_retain_a_successful_answer() {
    for answer in [None, Some("not an on-time answer".into())] {
        let (root, session, _, record) = pending(false);
        let outcome = DelegationOutcome::Joined {
            execution_timed_out: true,
            receipt: Some(crate::RequestReceipt {
                session: record.child,
                key: crate::delegation::request_key(record.child),
                turn: 1,
                end: Some(TurnEndReason::Completed),
            }),
            error: answer.as_ref().map(|_| "deadline exceeded".into()),
            answer,
        };
        assert!(matches!(
            session.settle_delegation(&record, outcome),
            Err(SessionError::InvalidHistory)
        ));
        assert!(session.view().unwrap().delegations()[0].outcome.is_none());
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn conservative_recovery_preserves_joined_relationships() {
    let (root, session, _, record) = pending(false);
    let outcome = DelegationOutcome::Joined {
        execution_timed_out: false,
        receipt: None,
        error: Some("stopped before materialization".into()),
        answer: None,
    };
    session.settle_delegation(&record, outcome.clone()).unwrap();
    session
        .begin_turn("fresh work".into(), record.spec.model.clone())
        .unwrap();
    let view = session.view().unwrap();
    assert_eq!(view.delegations()[0].outcome, Some(outcome));
    assert_eq!(
        view.entries
            .iter()
            .filter(|e| matches!(e, SessionEntry::DelegationSettled { .. }))
            .count(),
        1
    );
    assert!(matches!(
        view.last_end,
        Some((1, TurnEndReason::Interrupted))
    ));
    assert!(!record.path.exists());
    drop(session);
    fs::remove_dir_all(root).unwrap();
}
