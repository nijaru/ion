use super::*;
use ion_ai::{Content, Message, ResponseTermination, ToolCall};
use ion_core::{
    StoredToolActivity, ToolActivity, ToolActivityKind, ToolOutcome, ToolOutput,
    ToolResultProjection,
};

#[tokio::test]
async fn terminated_turn_publishes_once_and_survives_canonical_reconciliation() {
    for reason in [
        ion_core::TurnEndReason::Cancelled,
        ion_core::TurnEndReason::Failed("provider: InvalidRequest".into()),
    ] {
        let mut fixture = crate::terminal_client::tests::Fixture::new();
        let operation = fixture
            .control
            .compact(
                None,
                crate::terminal_client::EditorDraft::at_end(String::new()),
            )
            .unwrap();
        let session = fixture.control.session();
        let mut view = session.view().unwrap();
        let mut conversation = Conversation::default();
        let base = session.presentation(None).unwrap();
        let checkpoint = base.checkpoint.expect("empty session is an idle cut");
        conversation.reconcile_prepared(
            session.path(),
            Prepared::committed(
                base.transcript,
                base.id,
                base.selection,
                base.head_entry,
                None,
                Some(checkpoint),
            ),
        );
        let input = Message::user_input("request".into(), []);
        conversation.observe(
            operation,
            &AgentEvent::TurnAccepted {
                entry: 1,
                turn: 1,
                input: input.clone(),
            },
        );
        fixtures::publish(&mut conversation);
        conversation.observe(
            operation,
            &AgentEvent::TextDelta {
                block: 0,
                text: "provisional".into(),
            },
        );
        conversation.observe(
            operation,
            &AgentEvent::TurnEnded {
                entry: 2,
                turn: 1,
                reason: reason.clone(),
            },
        );
        assert_eq!(
            conversation.items().count(),
            2,
            "provisional content is removed, not promoted"
        );
        assert_eq!(
            conversation
                .segments(Rows::Pending)
                .iter()
                .filter(|segment| matches!(
                    segment,
                    Segment::Item(TranscriptItem::TurnTermination { .. })
                ))
                .count(),
            1
        );
        fixtures::publish(&mut conversation);
        conversation.clear_live();
        view.entries = vec![
            ion_core::SessionEntry::TurnStarted {
                turn: 1,
                input,
                model: ion_ai::ModelRef {
                    provider: "fixture".into(),
                    model: "fixture".into(),
                },
                completion: vec![],
                request: None,
                recovery: None,
            },
            ion_core::SessionEntry::TurnEnded { turn: 1, reason },
        ];
        view.context_path = vec![1, 2];
        conversation.reconcile_prepared(
            session.path(),
            Prepared::committed(
                TranscriptProjection::from_session(&view),
                base.id,
                base.selection,
                2,
                Some(checkpoint),
                None,
            ),
        );
        assert_eq!(
            conversation
                .items()
                .filter(|item| matches!(item, TranscriptItem::TurnTermination { .. }))
                .count(),
            1
        );
        assert!(
            conversation.segments(Rows::Pending).is_empty(),
            "canonical refresh cannot republish an acknowledged closure"
        );
        fixture.control.shutdown().await;
    }
}

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
            content: vec![
                Content::Thinking("Leading reasoning remains a separate source.".into()),
                Content::ToolCall(ToolCall {
                    id: "read".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path":"src/lib.rs"}),
                    raw_arguments: None,
                }),
                Content::Thinking("Interleaved reasoning does not seal the run.".into()),
            ],
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
                output: ToolOutput::success(serde_json::json!({"content":"observed"})),
            },
        },
    ] {
        assert!(!conversation.observe(operation, &event));
    }
    let sources = |conversation: &Conversation, pending| {
        conversation
            .segments(if pending { Rows::Pending } else { Rows::All })
            .into_iter()
            .filter_map(|segment| match segment {
                Segment::Item(item) => item.source(),
                Segment::Notice { .. } => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        sources(&conversation, true),
        [
            TranscriptSource { entry: 1, part: 0 },
            TranscriptSource { entry: 2, part: 0 }
        ]
    );
    fixtures::publish(&mut conversation);

    conversation.observe(
        operation,
        &AgentEvent::TextDelta {
            block: 0,
            text: "not yet committed".into(),
        },
    );
    assert!(
        sources(&conversation, true).is_empty(),
        "provisional prose cannot seal a growable group"
    );
    conversation.observe(operation, &AgentEvent::ResponseRestarted);
    assert!(sources(&conversation, true).is_empty());
    assert_eq!(
        conversation.items().count(),
        3,
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
            TranscriptSource { entry: 2, part: 1 },
            TranscriptSource { entry: 4, part: 0 }
        ]
    );
    assert_eq!(
        conversation.items().count(),
        4,
        "promoted blocks do not duplicate their live counterparts"
    );
    fixtures::publish(&mut conversation);
    conversation.clear_live();
    assert_eq!(
        conversation.items().count(),
        4,
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
    let pending = conversation.segments(Rows::Pending);
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
        conversation.segments(Rows::All).is_empty(),
        "local output does not leak across sessions"
    );
    fixture.control.shutdown().await;
}
