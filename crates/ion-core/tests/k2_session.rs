use ion_ai::{Content, Message, Role};
use ion_core::conversation::context::{ContextControl, ContextEdit};
use ion_core::{
    ConversationSpec, EntryId, EntryKind, EntryRequest, HistoryParent, InputBody, InputMode,
    InputRequest, InputSender, RequestKey, Session, SessionError, TaskKindName, TaskRequest,
};

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![Content::Text(text.to_owned())],
        provider_replay: None,
    }
}

fn assistant_call(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(ion_ai::ToolCall {
            id: id.to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({"path": "a"}),
        })],
        provider_replay: None,
    }
}

fn tool_result(id: &str) -> Message {
    Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ion_ai::ToolResult {
            call_id: id.to_owned(),
            name: "read".to_owned(),
            result: serde_json::json!("A"),
        })],
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
        .queue_input(request.clone())
        .expect("first admission");
    let second = session.queue_input(request).expect("replay");
    assert!(!first.replayed);
    assert!(second.replayed);
    assert_eq!(first.input_id, second.input_id);
    assert_eq!(first.commit_seq, second.commit_seq);
    assert_eq!(session.snapshot().inputs.len(), 1);
    assert_eq!(session.snapshot().last_commit, first.commit_seq);

    let conflict = session
        .queue_input(InputRequest {
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
fn context_controls_must_form_a_complete_provider_context() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let append = |session: &mut Session, projection: Vec<Message>, context: ContextControl| {
        session.append_entry(EntryRequest {
            conversation_id: root,
            kind: EntryKind::new("test").expect("entry kind"),
            data: serde_json::Value::Null,
            projection,
            context,
        })
    };

    // A plain assistant tool call stays durably appendable while tools run.
    let call = append(
        &mut session,
        vec![assistant_call("a")],
        ContextControl::none(),
    )
    .expect("tool call");
    let result =
        append(&mut session, vec![tool_result("a")], ContextControl::none()).expect("tool result");

    // A head boundary that starts at the bare tool result is incomplete.
    let before = session.snapshot().last_commit;
    let error = append(
        &mut session,
        vec![user_message("summary")],
        ContextControl::head(result.entry_id),
    )
    .expect_err("head on an orphaned result must be rejected");
    assert!(matches!(error, SessionError::IncompleteContextControl(_)));
    assert_eq!(session.snapshot().last_commit, before);

    // Omitting the originating call but keeping its result is also invalid.
    let error = append(
        &mut session,
        vec![user_message("note")],
        ContextControl {
            head: None,
            edits: vec![ContextEdit::Omit {
                target: call.entry_id,
            }],
        },
    )
    .expect_err("orphan tool result must be rejected");
    assert!(matches!(error, SessionError::IncompleteContextControl(_)));
    assert_eq!(session.snapshot().last_commit, before);

    // Retaining the complete exchange from its originating call commits.
    let summary = append(
        &mut session,
        vec![user_message("summary")],
        ContextControl::head(call.entry_id),
    )
    .expect("complete exchange head boundary");
    assert_eq!(summary.entry_id.get(), before.get() + 1);

    // A fork at the now-unreferenced assistant call is still incomplete.
    let error = session
        .create_conversation(ConversationSpec::fork(
            root,
            EntryId::new(call.entry_id.get()).expect("entry id"),
        ))
        .expect_err("incomplete fork cutoff must be rejected");
    assert!(matches!(error, SessionError::InvalidFork(_)));
}

#[test]
fn bounded_summary_and_paginated_transcript_reads() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let mut ids = Vec::new();
    for index in 0..5 {
        ids.push(
            session
                .append_entry(EntryRequest {
                    conversation_id: root,
                    kind: EntryKind::new("user").expect("entry kind"),
                    data: serde_json::Value::Null,
                    projection: vec![user_message(&format!("message {index}"))],
                    context: ContextControl::none(),
                })
                .expect("entry")
                .entry_id,
        );
    }

    let summary = session.summary();
    assert_eq!(summary.entries, 5);
    assert_eq!(summary.conversations, 1);
    assert_eq!(summary.inputs, 0);
    assert_eq!(summary.tasks, ion_core::TaskCounts::default());
    assert_eq!(summary.last_commit, session.snapshot().last_commit);

    let first = session
        .conversation_entries(root, None, 2)
        .expect("first page");
    assert_eq!(first.entries.len(), 2);
    assert_eq!(first.entries[0].id, ids[0]);
    let cursor = first.next.expect("more pages");
    assert_eq!(cursor, ids[1]);

    let second = session
        .conversation_entries(root, Some(cursor), 2)
        .expect("second page");
    assert_eq!(second.entries[0].id, ids[2]);
    let last = session
        .conversation_entries(root, second.next, 2)
        .expect("last page");
    assert_eq!(last.entries.len(), 1);
    assert_eq!(last.entries[0].id, ids[4]);
    assert_eq!(last.next, None);

    assert!(matches!(
        session.conversation_entries(root, Some(EntryId::new(9_999).expect("id")), 2),
        Err(SessionError::InvisibleContextReference(_))
    ));
    assert!(
        session
            .conversation_entries(root, None, 0)
            .expect("empty")
            .entries
            .is_empty()
    );
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
