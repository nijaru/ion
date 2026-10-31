use super::*;
use ion_ai::{Content, Message, ResponseTermination, ToolCall};
use ion_core::{
    StoredToolActivity, ToolActivity, ToolActivityKind, ToolOutcome, ToolOutput,
    ToolResultProjection,
};

#[tokio::test]
async fn sealed_blocks_publish_during_a_turn_but_provisional_boundaries_do_not() {
    let mut fixture = crate::terminal_client::tests::Fixture::new();
    let operation = fixture
        .control
        .compact(
            None,
            crate::terminal_client::EditorDraft::at_end(String::new()),
        )
        .unwrap();
    let mut conversation = fixtures::document(TranscriptProjection::default());
    let activity = ToolActivity {
        kind: ToolActivityKind::Read,
        subject: Some("src/lib.rs".into()),
    };
    for event in [
        AgentEvent::TurnAccepted {
            entry: 1,
            turn: 1,
            input: Message::user_input("request".into(), []),
        },
        AgentEvent::AssistantCommitted {
            entry: 2,
            turn: 1,
            content: vec![Content::ToolCall(ToolCall {
                id: "read".into(),
                name: "read".into(),
                arguments: serde_json::json!({"path":"src/lib.rs"}),
                raw_arguments: None,
            })],
            tool_activities: vec![StoredToolActivity {
                call_id: "read".into(),
                activity: activity.clone(),
            }],
            termination: ResponseTermination::Completed,
        },
        AgentEvent::ToolFinished {
            call_id: "read".into(),
            name: "read".into(),
            activity,
            outcome: ToolOutcome::Observed {
                projection: ToolResultProjection::Observed,
                output: ToolOutput {
                    value: serde_json::json!({"content":"observed"}),
                    images: vec![],
                    is_error: false,
                },
            },
        },
    ] {
        assert!(!conversation.observe(operation, &event));
    }
    let sources = |conversation: &Conversation, pending| {
        conversation
            .segments(pending)
            .into_iter()
            .filter_map(|segment| match segment {
                Segment::Item(item) => item.source(),
                Segment::Notice { .. } => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        sources(&conversation, true),
        [TranscriptSource { entry: 1, part: 0 }]
    );
    fixtures::publish(&mut conversation);

    conversation.observe(
        operation,
        &AgentEvent::TextDelta("not yet committed".into()),
    );
    assert!(
        sources(&conversation, true).is_empty(),
        "provisional prose cannot seal a growable group"
    );
    conversation.observe(operation, &AgentEvent::ResponseRestarted);
    assert!(sources(&conversation, true).is_empty());
    assert_eq!(
        conversation.items().count(),
        2,
        "restart removes only provisional prose"
    );
    conversation.observe(
        operation,
        &AgentEvent::AssistantCommitted {
            entry: 4,
            turn: 1,
            content: vec![Content::Text("committed boundary".into())],
            tool_activities: vec![],
            termination: ResponseTermination::Completed,
        },
    );
    assert_eq!(
        sources(&conversation, true),
        [
            TranscriptSource { entry: 2, part: 0 },
            TranscriptSource { entry: 4, part: 0 }
        ]
    );
    assert_eq!(
        conversation.items().count(),
        3,
        "promoted blocks do not duplicate their live counterparts"
    );
    fixtures::publish(&mut conversation);
    conversation.clear_live();
    assert_eq!(
        conversation.items().count(),
        3,
        "discarding the overlay retains sealed facts"
    );
    assert!(sources(&conversation, true).is_empty());
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn prepared_publication_is_prefix_scoped_and_cannot_replay_after_an_empty_paint_or_scope_return()
 {
    let mut fixture = crate::terminal_client::tests::Fixture::new();
    let session = fixture.control.session();
    let mut view = session.view().unwrap();
    let original = view.id;
    let mut conversation = Conversation::default();
    conversation.reconcile(session, &view);
    let original_scope = conversation.prepare_publication();
    let first = conversation.prepare_publication();
    let duplicate = conversation.prepare_publication();
    assert!(conversation.acknowledge(first));
    assert!(
        !conversation.acknowledge(duplicate),
        "even an empty paint consumes its receipt"
    );

    conversation.note("first".into(), false);
    let prepared = conversation.prepare_publication();
    conversation.note("later".into(), false);
    assert!(conversation.acknowledge(prepared));
    let pending = conversation.segments(true);
    assert!(matches!(
        pending.as_slice(),
        [Segment::Notice { text: "later", .. }]
    ));

    view.id = uuid::Uuid::now_v7();
    assert_eq!(conversation.reconcile(session, &view), Change::Session);
    assert!(conversation.banner().is_some());
    let banner = conversation.prepare_publication();
    let duplicate_banner = conversation.prepare_publication();
    assert!(conversation.acknowledge(banner));
    assert!(!conversation.acknowledge(duplicate_banner));
    view.id = original;
    conversation.reconcile(session, &view);
    assert!(
        !conversation.acknowledge(original_scope),
        "returning to an identical scope cannot revive an old frame"
    );
    assert!(
        conversation.segments(false).is_empty(),
        "local output does not leak across sessions"
    );
    fixture.control.shutdown().await;
}
