use ion_ai::{Content, Message, Role};
use ion_core::conversation::context::ContextControl;
use ion_core::{
    ConversationSpec, EntryKind, EntryRequest, HistoryParent, InputBody, InputMode, InputRequest,
    InputSender, RequestKey, Session, SessionError, TaskKindName, TaskRequest,
};

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(text.to_owned())],
        provider_replay: None,
    }
}

#[test]
fn root_and_commands_share_one_monotonic_sequence() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    assert_eq!(root.get(), 1);
    assert_eq!(session.snapshot().last_commit.get(), 2);

    let conversation = session
        .create_conversation(ConversationSpec::independent())
        .expect("conversation");
    assert_eq!(conversation.conversation_id.get(), 3);
    assert_eq!(conversation.commit_seq.get(), 4);

    let entry = session
        .append_entry(EntryRequest {
            conversation_id: conversation.conversation_id,
            kind: EntryKind::new("user").expect("entry kind"),
            data: serde_json::json!({"text": "hello"}),
            projection: vec![user_message("hello")],
            context: ContextControl::none(),
        })
        .expect("entry");
    assert_eq!(entry.entry_id.get(), 5);
    assert_eq!(entry.commit_seq.get(), 6);
}

#[test]
fn failed_fork_does_not_consume_or_publish_ids() {
    let mut session = Session::new().expect("session");
    let before = session.snapshot().last_commit;

    let error = session
        .create_conversation(ConversationSpec::fork(
            session.root_conversation(),
            ion_core::EntryId::new(99).expect("entry id"),
        ))
        .expect_err("invisible cutoff must fail");
    assert!(matches!(error, SessionError::InvalidFork(_)));
    assert_eq!(session.snapshot().last_commit, before);

    let next = session
        .create_conversation(ConversationSpec::independent())
        .expect("conversation");
    assert_eq!(next.conversation_id.get(), 3);
    assert_eq!(next.commit_seq.get(), 4);
}

#[test]
fn request_key_replay_returns_original_receipt_without_new_commit() {
    let mut session = Session::new().expect("session");
    let key = RequestKey::new("request-1").expect("request key");
    let request = InputRequest {
        target: session.root_conversation(),
        sender: InputSender::User,
        mode: InputMode::Submit,
        request_key: Some(key.clone()),
        body: InputBody::Text("ship it".to_owned()),
    };

    let first = session
        .admit_input(request.clone())
        .expect("first admission");
    let second = session.admit_input(request).expect("replay");
    assert!(!first.replayed);
    assert!(second.replayed);
    assert_eq!(first.input_id, second.input_id);
    assert_eq!(first.commit_seq, second.commit_seq);
    assert_eq!(session.snapshot().inputs.len(), 1);
    assert_eq!(session.snapshot().last_commit, first.commit_seq);

    let conflict = session
        .admit_input(InputRequest {
            target: session.root_conversation(),
            sender: InputSender::User,
            mode: InputMode::Submit,
            request_key: Some(key),
            body: InputBody::Text("different".to_owned()),
        })
        .expect_err("rebound request key must fail");
    assert!(matches!(conflict, SessionError::IdempotencyConflict(_)));
    assert_eq!(session.snapshot().last_commit, first.commit_seq);
}

#[test]
fn owned_conversation_and_reciprocal_task_link_commit_atomically() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let task = session
        .create_task(TaskRequest {
            conversation_id: root,
            kind: TaskKindName::new("worker.spawn").expect("task kind"),
            schema_version: 1,
            input: serde_json::Value::Null,
            dependencies: Vec::new(),
        })
        .expect("task");
    let parent_entry = session
        .append_entry(EntryRequest {
            conversation_id: root,
            kind: EntryKind::new("user").expect("entry kind"),
            data: serde_json::Value::Null,
            projection: vec![user_message("context")],
            context: ContextControl::none(),
        })
        .expect("entry");

    let worker = session
        .create_conversation(ConversationSpec::owned(
            task.task_id,
            Some(HistoryParent {
                conversation_id: root,
                at: parent_entry.entry_id,
            }),
        ))
        .expect("worker");

    let snapshot = session.snapshot();
    let task = snapshot
        .tasks
        .iter()
        .find(|record| record.id == task.task_id)
        .expect("task record");
    assert_eq!(task.owned_conversations, vec![worker.conversation_id]);
    let conversation = snapshot
        .conversations
        .iter()
        .find(|conversation| conversation.id == worker.conversation_id)
        .expect("worker conversation");
    assert_eq!(conversation.owner_task, Some(task.id));
}

#[test]
fn bounded_observations_require_resnapshot_after_overflow() {
    let mut session = Session::new().expect("session");
    let initial = session.snapshot().last_commit;
    for _ in 0..130 {
        session
            .create_conversation(ConversationSpec::independent())
            .expect("conversation");
    }

    let observations = session.observations_after(Some(initial));
    assert!(observations.reset_required);
    assert!(observations.events.is_empty());

    let current = session.snapshot().last_commit;
    let observations = session.observations_after(Some(current));
    assert!(!observations.reset_required);
    assert!(observations.events.is_empty());
}
