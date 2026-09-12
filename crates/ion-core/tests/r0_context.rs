mod r0_support;

use r0_support::{ContextEdit, ContextError, ContextStore, ModelMessage, ToolCall};

fn user(text: &str) -> Vec<ModelMessage> {
    vec![ModelMessage::User(text.to_owned())]
}

fn assistant(text: &str) -> Vec<ModelMessage> {
    vec![ModelMessage::Assistant {
        text: text.to_owned(),
        calls: Vec::new(),
    }]
}

fn assistant_with_calls(text: &str, calls: &[(&str, &str)]) -> Vec<ModelMessage> {
    vec![ModelMessage::Assistant {
        text: text.to_owned(),
        calls: calls
            .iter()
            .map(|(id, name)| ToolCall {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
            })
            .collect(),
    }]
}

fn tool_result(call_id: &str, tool_name: &str, text: &str) -> Vec<ModelMessage> {
    vec![ModelMessage::ToolResult {
        call_id: call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        text: text.to_owned(),
    }]
}

#[test]
fn transcript_keeps_completion_order_but_projection_restores_tool_call_order() {
    let mut store = ContextStore::default();
    let root = store.create_root();
    let user_id = store
        .append(root, "user", user("inspect"), None, Vec::new())
        .expect("user");
    let assistant_id = store
        .append(
            root,
            "assistant",
            assistant_with_calls("", &[("a", "read"), ("b", "grep")]),
            None,
            Vec::new(),
        )
        .expect("assistant");
    let result_b = store
        .append(
            root,
            "tool_result",
            tool_result("b", "grep", "B"),
            None,
            Vec::new(),
        )
        .expect("result b");
    let result_a = store
        .append(
            root,
            "tool_result",
            tool_result("a", "read", "A"),
            None,
            Vec::new(),
        )
        .expect("result a");

    assert_eq!(
        store.logical_entry_ids(root).expect("logical transcript"),
        vec![user_id, assistant_id, result_b, result_a]
    );
    let projected = store.project(root, None).expect("projection");
    assert_eq!(
        projected.messages,
        vec![
            ModelMessage::User("inspect".to_owned()),
            ModelMessage::Assistant {
                text: String::new(),
                calls: vec![
                    ToolCall {
                        id: "a".to_owned(),
                        name: "read".to_owned(),
                    },
                    ToolCall {
                        id: "b".to_owned(),
                        name: "grep".to_owned(),
                    },
                ],
            },
            ModelMessage::ToolResult {
                call_id: "a".to_owned(),
                tool_name: "read".to_owned(),
                text: "A".to_owned(),
            },
            ModelMessage::ToolResult {
                call_id: "b".to_owned(),
                tool_name: "grep".to_owned(),
                text: "B".to_owned(),
            },
        ]
    );
}

#[test]
fn summary_head_projects_before_retained_tail_without_mutating_history() {
    let mut store = ContextStore::default();
    let root = store.create_root();
    let first_user = store
        .append(root, "user", user("old question"), None, Vec::new())
        .expect("first user");
    let first_answer = store
        .append(root, "assistant", assistant("old answer"), None, Vec::new())
        .expect("first answer");
    let retained_user = store
        .append(root, "user", user("new question"), None, Vec::new())
        .expect("retained user");
    let retained_answer = store
        .append(root, "assistant", assistant("new answer"), None, Vec::new())
        .expect("retained answer");
    let summary = store
        .append(
            root,
            "summary",
            user("summary of old work"),
            Some(retained_user),
            Vec::new(),
        )
        .expect("summary");

    assert_eq!(
        store.logical_entry_ids(root).expect("history"),
        vec![
            first_user,
            first_answer,
            retained_user,
            retained_answer,
            summary
        ]
    );
    let projected = store.project(root, None).expect("projection");
    assert_eq!(
        projected.entry_ids,
        vec![summary, retained_user, retained_answer]
    );
    assert_eq!(
        projected.messages,
        vec![
            ModelMessage::User("summary of old work".to_owned()),
            ModelMessage::User("new question".to_owned()),
            ModelMessage::Assistant {
                text: "new answer".to_owned(),
                calls: Vec::new(),
            },
        ]
    );
}

#[test]
fn handoff_and_reset_are_new_heads_not_history_rewrites() {
    let mut store = ContextStore::default();
    let root = store.create_root();
    let old_user = store
        .append(root, "user", user("old"), None, Vec::new())
        .expect("old user");
    let handoff = store
        .append_self_head(root, "handoff", user("handoff"))
        .expect("handoff");
    let after_handoff = store
        .append(root, "user", user("continue"), None, Vec::new())
        .expect("continuation");

    let projected = store.project(root, None).expect("handoff projection");
    assert_eq!(projected.entry_ids, vec![handoff, after_handoff]);
    assert_eq!(
        projected.messages,
        vec![
            ModelMessage::User("handoff".to_owned()),
            ModelMessage::User("continue".to_owned()),
        ]
    );

    let reset = store
        .append_self_head(root, "reset", Vec::new())
        .expect("reset");
    let after_reset = store
        .append(root, "user", user("fresh"), None, Vec::new())
        .expect("fresh user");
    let projected = store.project(root, None).expect("reset projection");
    assert_eq!(projected.entry_ids, vec![reset, after_reset]);
    assert_eq!(
        projected.messages,
        vec![ModelMessage::User("fresh".to_owned())]
    );

    assert_eq!(
        store.logical_entry_ids(root).expect("history remains"),
        vec![old_user, handoff, after_handoff, reset, after_reset]
    );
}

#[test]
fn newest_context_edit_wins_without_patching_target_entry() {
    let mut store = ContextStore::default();
    let root = store.create_root();
    let original = store
        .append(root, "user", user("verbose"), None, Vec::new())
        .expect("original");
    let answer = store
        .append(root, "assistant", assistant("answer"), None, Vec::new())
        .expect("answer");
    let first_edit = store
        .append(
            root,
            "context_edit",
            Vec::new(),
            None,
            vec![ContextEdit::Replace {
                target: original,
                messages: user("short"),
            }],
        )
        .expect("first edit");
    let second_edit = store
        .append(
            root,
            "context_edit",
            Vec::new(),
            None,
            vec![
                ContextEdit::Replace {
                    target: original,
                    messages: user("shorter"),
                },
                ContextEdit::Omit { target: answer },
            ],
        )
        .expect("second edit");

    let projected = store.project(root, None).expect("projection");
    assert_eq!(
        projected.entry_ids,
        vec![original, answer, first_edit, second_edit]
    );
    assert_eq!(
        projected.messages,
        vec![ModelMessage::User("shorter".to_owned())]
    );
    assert_eq!(
        store.logical_entry_ids(root).expect("unmodified history"),
        vec![original, answer, first_edit, second_edit]
    );
}

#[test]
fn fork_shares_source_prefix_but_never_observes_later_source_appends() {
    let mut store = ContextStore::default();
    let root = store.create_root();
    let first = store
        .append(root, "user", user("one"), None, Vec::new())
        .expect("first");
    let cutoff = store
        .append(root, "assistant", assistant("two"), None, Vec::new())
        .expect("cutoff");
    let child = store.fork(root, cutoff).expect("fork");
    let later_source = store
        .append(root, "user", user("source-only"), None, Vec::new())
        .expect("source append");
    let child_local = store
        .append(child, "user", user("child-only"), None, Vec::new())
        .expect("child append");

    assert_eq!(
        store.logical_entry_ids(root).expect("root history"),
        vec![first, cutoff, later_source]
    );
    assert_eq!(
        store.logical_entry_ids(child).expect("child history"),
        vec![first, cutoff, child_local]
    );
    assert_eq!(
        store
            .project(child, None)
            .expect("child projection")
            .messages,
        vec![
            ModelMessage::User("one".to_owned()),
            ModelMessage::Assistant {
                text: "two".to_owned(),
                calls: Vec::new(),
            },
            ModelMessage::User("child-only".to_owned()),
        ]
    );
}

#[test]
fn worker_style_fork_rejects_incomplete_tool_exchange() {
    let mut store = ContextStore::default();
    let root = store.create_root();
    store
        .append(root, "user", user("inspect"), None, Vec::new())
        .expect("user");
    let assistant = store
        .append(
            root,
            "assistant",
            assistant_with_calls("", &[("a", "read"), ("b", "grep")]),
            None,
            Vec::new(),
        )
        .expect("assistant");
    assert_eq!(
        store.fork(root, assistant),
        Err(ContextError::IncompleteExchange(assistant))
    );
    store
        .append(
            root,
            "tool_result",
            tool_result("a", "read", "A"),
            None,
            Vec::new(),
        )
        .expect("result a");
    let final_result = store
        .append(
            root,
            "tool_result",
            tool_result("b", "grep", "B"),
            None,
            Vec::new(),
        )
        .expect("result b");
    assert!(store.fork(root, final_result).is_ok());
}

#[test]
fn context_head_rejects_incomplete_tool_exchange_boundary() {
    let mut store = ContextStore::default();
    let root = store.create_root();
    store
        .append(root, "user", user("inspect"), None, Vec::new())
        .expect("user");
    let assistant = store
        .append(
            root,
            "assistant",
            assistant_with_calls("", &[("a", "read")]),
            None,
            Vec::new(),
        )
        .expect("assistant");
    assert_eq!(
        store.append(
            root,
            "summary",
            user("invalid summary"),
            Some(assistant),
            Vec::new(),
        ),
        Err(ContextError::IncompleteExchange(assistant))
    );
}

#[test]
fn context_heads_are_monotonic() {
    let mut store = ContextStore::default();
    let root = store.create_root();
    let first = store
        .append(root, "user", user("one"), None, Vec::new())
        .expect("first");
    let second = store
        .append(root, "assistant", assistant("two"), None, Vec::new())
        .expect("second");
    store
        .append(root, "summary", user("summary"), Some(second), Vec::new())
        .expect("first head");
    assert_eq!(
        store.append(
            root,
            "summary",
            user("backwards"),
            Some(first),
            Vec::new()
        ),
        Err(ContextError::HeadMovedBackwards {
            previous: second,
            next: first,
        })
    );
}
