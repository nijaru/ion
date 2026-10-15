use super::*;
use ion_ai::{Content, Message, ModelRef, Role, ToolCall, ToolSpec, Usage};
use std::fs;

#[test]
fn continuation_commit_is_atomic_checkpointed_and_does_not_grant_copies_authority() {
    let root = std::env::temp_dir().join(format!("ion-turn-recovery-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    let session = Session::create(root.join("session.db"), None).unwrap();
    let binding = RecoveryBinding {
        session: session.id(),
        authority: AuthorityScope::new("app/principal/environment/routes-v1").unwrap(),
        fingerprint: [42; 32],
    };
    session
        .admit_turn(
            Message::user_input("work".into(), []),
            ModelRef {
                provider: "fixture".into(),
                model: "fixture".into(),
            },
            None,
            Some(binding.clone()),
        )
        .unwrap();
    let call = ToolCall {
        id: "effect".into(),
        name: "effect".into(),
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
            true,
        )
        .unwrap();
    let parent = crate::ToolOccurrence {
        assistant_entry: session.view().unwrap().entries.len() as u64,
        ordinal: 0,
    };
    session
        .record_child_intent(
            1,
            crate::ChildIntent {
                parent,
                child: 0,
                activity: ToolActivity::external("effect"),
                call,
                definition: ToolSpec {
                    name: "effect".into(),
                    description: "effect".into(),
                    input_schema: serde_json::json!({}),
                },
            },
        )
        .unwrap();
    let point = session.recovery_state().unwrap().unwrap().point;
    let before = session.view().unwrap().entries;
    session.store.lock().unwrap().connection.execute_batch("CREATE TRIGGER fail_resume BEFORE INSERT ON entries WHEN json_extract(NEW.body, '$.kind') = 'tool_result' BEGIN SELECT RAISE(FAIL, 'injected'); END;").unwrap();
    assert!(session.resume_turn(point, &binding).is_err());
    assert_eq!(session.view().unwrap().entries, before);
    assert_eq!(session.recovery_state().unwrap().unwrap().point, point);
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute_batch("DROP TRIGGER fail_resume")
        .unwrap();
    let (accepted, interrupted) = session.resume_turn(point, &binding).unwrap();
    assert_eq!(interrupted, 1);
    assert!(accepted.checkpoint > point.checkpoint);
    assert!(matches!(
        session.resume_turn(point, &binding),
        Err(SessionError::StaleRecoveryPoint)
    ));
    let view = session.view().unwrap();
    assert_eq!(view.unfinished_turn, Some(1));
    assert!(
        matches!(&view.entries[before.len()..], [SessionEntry::TurnResumed { .. }, SessionEntry::ChildToolResult { outcome: crate::ChildOutcome::Unknown, .. }, SessionEntry::ToolResult { result, .. }] if matches!(result.outcome, crate::ToolOutcome::Unknown))
    );
    let copy = session.clone_to(root.join("copy.db")).unwrap();
    assert!(copy.recovery_state().unwrap().is_none());
    let candidate = RecoveryBinding {
        session: copy.id(),
        ..binding.clone()
    };
    assert!(matches!(
        copy.resume_turn(
            RecoveryPoint {
                session: copy.id(),
                ..accepted
            },
            &candidate
        ),
        Err(SessionError::RecoveryAuthorityChanged)
    ));
    session
        .end_turn(1, TurnEndReason::Cancelled, &BTreeSet::new())
        .unwrap();
    assert!(session.recovery_state().unwrap().is_none());
    assert!(matches!(
        session.resume_turn(accepted, &binding),
        Err(SessionError::StaleRecoveryPoint)
    ));
    drop(copy);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}
