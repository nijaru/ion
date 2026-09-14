use ion_ai::{Content, Message, Role, ToolCall, ToolResult};
use ion_core::conversation::context::{
    ContextControl, ContextEdit, ContextError, ForkError, project, validate_fork_cutoff,
};
use ion_core::{ConversationId, Entry, EntryId, EntryKind};

fn conversation() -> ConversationId {
    ConversationId::new(1).expect("conversation id")
}

fn entry(id: i64, projection: Vec<Message>, context: ContextControl) -> Entry {
    Entry::new(
        EntryId::new(id).expect("entry id"),
        conversation(),
        EntryKind::new("test").expect("entry kind"),
        serde_json::Value::Null,
        projection,
        context,
    )
}

fn text(role: Role, value: &str) -> Message {
    Message {
        role,
        content: vec![Content::Text(value.to_owned())],
        provider_replay: None,
    }
}

#[test]
fn immutable_head_and_edit_projection_preserves_history() {
    let first = entry(
        1,
        vec![text(Role::User, "old question")],
        ContextControl::none(),
    );
    let second = entry(
        2,
        vec![text(Role::Assistant, "old answer")],
        ContextControl::none(),
    );
    let retained = entry(
        3,
        vec![text(Role::User, "new question")],
        ContextControl::none(),
    );
    let answer = entry(
        4,
        vec![text(Role::Assistant, "new answer")],
        ContextControl::none(),
    );
    let summary = entry(
        5,
        vec![text(Role::User, "summary")],
        ContextControl {
            head: Some(retained.id),
            edits: vec![ContextEdit::Replace {
                target: answer.id,
                messages: vec![text(Role::Assistant, "short answer")],
            }],
        },
    );
    let history = vec![first, second, retained.clone(), answer, summary.clone()];

    let projected = project(&history).expect("project context");

    assert_eq!(
        projected.contributing,
        vec![summary.id, retained.id, EntryId::new(4).unwrap()]
    );
    assert_eq!(
        projected.messages,
        vec![
            text(Role::User, "summary"),
            text(Role::User, "new question"),
            text(Role::Assistant, "short answer"),
        ]
    );
    assert_eq!(history.len(), 5);
}

#[test]
fn tool_completion_order_is_normalized_to_source_call_order() {
    let assistant = entry(
        1,
        vec![Message {
            role: Role::Assistant,
            content: vec![
                Content::ToolCall(ToolCall {
                    id: "a".to_owned(),
                    name: "read".to_owned(),
                    arguments: serde_json::json!({"path": "a"}),
                }),
                Content::ToolCall(ToolCall {
                    id: "b".to_owned(),
                    name: "grep".to_owned(),
                    arguments: serde_json::json!({"pattern": "b"}),
                }),
            ],
            provider_replay: None,
        }],
        ContextControl::none(),
    );
    let result_b = entry(
        2,
        vec![Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "b".to_owned(),
                name: "grep".to_owned(),
                result: serde_json::json!("B"),
            })],
            provider_replay: None,
        }],
        ContextControl::none(),
    );
    let result_a = entry(
        3,
        vec![Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(ToolResult {
                call_id: "a".to_owned(),
                name: "read".to_owned(),
                result: serde_json::json!("A"),
            })],
            provider_replay: None,
        }],
        ContextControl::none(),
    );
    let history = vec![assistant.clone(), result_b, result_a.clone()];

    assert!(matches!(
        validate_fork_cutoff(&history, assistant.id),
        Err(ForkError::IncompleteContext(_))
    ));
    validate_fork_cutoff(&history, result_a.id).expect("complete exchange cutoff");

    let projected = project(&history).expect("project context");
    let call_ids: Vec<_> = projected.messages[1..]
        .iter()
        .map(|message| match &message.content[0] {
            Content::ToolResult(result) => result.call_id.as_str(),
            other => panic!("expected tool result, got {other:?}"),
        })
        .collect();
    assert_eq!(call_ids, vec!["a", "b"]);
}

fn call(id: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(ToolCall {
            id: id.to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({"path": id}),
        })],
        provider_replay: None,
    }
}

fn result(id: &str, value: &str) -> Message {
    Message {
        role: Role::Tool,
        content: vec![Content::ToolResult(ToolResult {
            call_id: id.to_owned(),
            name: "read".to_owned(),
            result: serde_json::json!(value),
        })],
        provider_replay: None,
    }
}

#[test]
fn omitting_an_assistant_call_leaves_no_orphan_tool_result() {
    let assistant = entry(1, vec![call("a")], ContextControl::none());
    let tool = entry(2, vec![result("a", "A")], ContextControl::none());
    let omit = entry(
        3,
        vec![text(Role::User, "note")],
        ContextControl {
            head: None,
            edits: vec![ContextEdit::Omit {
                target: assistant.id,
            }],
        },
    );
    let history = vec![assistant, tool, omit];

    assert!(matches!(
        project(&history),
        Err(ContextError::OrphanToolResult)
    ));
}

#[test]
fn head_boundary_inside_a_tool_exchange_is_rejected() {
    let assistant = entry(1, vec![call("a")], ContextControl::none());
    let tool = entry(2, vec![result("a", "A")], ContextControl::none());
    let summary = entry(
        3,
        vec![text(Role::User, "summary")],
        ContextControl::head(tool.id),
    );
    let history = vec![assistant, tool, summary];

    // A head boundary on the tool result excludes its assistant call, so the
    // projected context would start with an orphan tool result.
    assert!(matches!(
        project(&history),
        Err(ContextError::OrphanToolResult)
    ));
}

#[test]
fn incomplete_exchange_is_rejected_but_independent_exchanges_pass() {
    let first = entry(1, vec![call("a")], ContextControl::none());
    let dangling = entry(
        2,
        vec![text(Role::User, "interrupt")],
        ContextControl::none(),
    );
    assert!(matches!(
        project(&[first.clone(), dangling]),
        Err(ContextError::MissingToolResult(id)) if id == "a"
    ));

    // Distinct exchanges may reuse a call id: it is only meaningful within its
    // own originating assistant message.
    let first_result = entry(3, vec![result("a", "A")], ContextControl::none());
    let bridge = entry(4, vec![text(Role::User, "next")], ContextControl::none());
    let second = entry(5, vec![call("a")], ContextControl::none());
    let second_result = entry(6, vec![result("a", "A2")], ContextControl::none());
    let history = vec![first, first_result, bridge, second, second_result];
    project(&history).expect("independent exchanges are valid");
}
