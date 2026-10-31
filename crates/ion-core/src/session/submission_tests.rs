use super::*;
use crate::session::submission::TurnAdmission;

fn input(text: &str) -> Message {
    Message::user_input(text.into(), [])
}
fn model() -> ModelRef {
    ModelRef {
        provider: "fixture".into(),
        model: "fixture".into(),
    }
}
fn root() -> PathBuf {
    std::env::temp_dir().join(format!("ion-request-{}", uuid::Uuid::now_v7()))
}

#[test]
fn failed_request_commit_preserves_interrupted_work_and_retry_authority() {
    let root = root();
    let session = Session::create(root.join("session.db"), None).unwrap();
    session.begin_turn("prior".into(), model()).unwrap();
    session
        .record_assistant(
            1,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "effect".into(),
                    name: "effect".into(),
                    arguments: serde_json::json!({}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            Usage::unknown(),
            true,
        )
        .unwrap();
    let before = session.view().unwrap().entries;
    session.store.lock().unwrap().connection.execute_batch(
        "CREATE TRIGGER fail_admission BEFORE INSERT ON entries WHEN json_extract(NEW.body, '$.kind') = 'turn_started' BEGIN SELECT RAISE(FAIL, 'injected'); END;"
    ).unwrap();
    let key = RequestKey::new("inbox".into(), "delivery".into()).unwrap();
    assert!(
        session
            .admit_turn(input("next"), model(), Some(key.clone()), None)
            .is_err()
    );
    assert_eq!(session.view().unwrap().entries, before);
    assert!(session.request_receipt(&key).unwrap().is_none());
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute_batch("DROP TRIGGER fail_admission")
        .unwrap();
    assert!(matches!(
        session
            .admit_turn(input("next"), model(), Some(key.clone()), None)
            .unwrap(),
        TurnAdmission::New {
            turn: 2,
            interrupted: 1,
            input: accepted,
        } if accepted == input("next")
    ));
    let receipt = session.request_receipt(&key).unwrap().unwrap();
    let entries = session.view().unwrap().entries;
    assert!(entries.iter().any(|entry| matches!(entry, SessionEntry::ToolResult { result, .. } if matches!(result.outcome, crate::ToolOutcome::Unknown))));
    assert!(
        matches!(session.admit_turn(input("next"), model(), Some(key.clone()), None).unwrap(), TurnAdmission::Existing(found) if found == receipt)
    );
    assert!(matches!(
        session
            .admit_turn(input("different"), model(), Some(key), None)
            .unwrap_err(),
        SessionError::RequestConflict
    ));
    assert_eq!(
        session.view().unwrap().entries,
        entries,
        "redelivery must not close or advance work"
    );
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn duplicate_request_facts_are_invalid_even_without_the_derived_index() {
    let root = root();
    let path = root.join("session.db");
    let session = Session::create(&path, None).unwrap();
    for scope in ["first", "second"] {
        session
            .admit_turn(
                input("literal"),
                model(),
                Some(RequestKey::new(scope.into(), "delivery".into()).unwrap()),
                None,
            )
            .unwrap();
    }
    drop(session);
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX request_keys;
        UPDATE entries SET body=CAST(json_set(CAST(body AS TEXT), '$.data.request.scope', 'first') AS BLOB)
        WHERE json_extract(body, '$.kind')='turn_started';",
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        Session::open(&path),
        Err(SessionError::InvalidHistory)
    ));
    assert!(matches!(
        Session::inspect_metadata(&path),
        Err(SessionError::InvalidHistory)
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn logical_identity_survives_moves_but_copies_do_not_inherit_deliveries() {
    let root = root();
    let path = root.join("source.db");
    let session = Session::create(&path, None).unwrap();
    let key = RequestKey::new("tenant-a".into(), "same-id".into()).unwrap();
    session
        .admit_turn(input("hello"), model(), Some(key.clone()), None)
        .unwrap();
    session
        .record_assistant(
            1,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("done".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let id = session.id();
    assert_ne!(id, session.provider_session_id());
    assert_eq!(Session::inspect(&path).unwrap().id, id);
    assert_eq!(Session::inspect_metadata(&path).unwrap().id, id);
    let receipt = session.request_receipt(&key).unwrap().unwrap();
    assert_eq!(receipt.end, Some(TurnEndReason::Completed));
    for copy in [
        session.clone_to(root.join("clone.db")).unwrap(),
        session
            .fork_to(root.join("fork.db"), ForkPoint::AfterTurn(1))
            .unwrap(),
    ] {
        assert_ne!(copy.id(), id);
        assert!(copy.request_receipt(&key).unwrap().is_none());
        assert_eq!(copy.messages().unwrap(), session.messages().unwrap());
    }
    let other_scope = RequestKey::new("tenant-b".into(), "same-id".into()).unwrap();
    assert!(matches!(
        session
            .admit_turn(input("hello"), model(), Some(other_scope), None)
            .unwrap(),
        TurnAdmission::New { turn: 2, .. }
    ));
    drop(session);
    let moved = root.join("moved.db");
    fs::rename(&path, &moved).unwrap();
    let reopened = Session::open(moved).unwrap();
    assert_eq!(reopened.id(), id);
    assert_eq!(reopened.request_receipt(&key).unwrap(), Some(receipt));
    assert!(reopened.view().unwrap().unfinished_turn.is_some());
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}
