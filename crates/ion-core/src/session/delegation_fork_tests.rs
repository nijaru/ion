use super::*;
use crate::{
    ChildIntent, ChildOutcome, DelegationContext, DelegationOutcome, DelegationSite, ForegroundSpec,
};
use ion_ai::{Content, ProviderReplay, ToolCall};

fn spec(root: &Path) -> ForegroundSpec {
    ForegroundSpec {
        context: DelegationContext::Fresh,
        directory: root.join("children"),
        cwd: None,
        model: ModelRef {
            provider: "child".into(),
            model: "child".into(),
        },
        reasoning: Reasoning::Off,
        authority: AuthorityScope::new("explicit-child/environment/v1").unwrap(),
        input: Message::user_input("new child task".into(), []),
    }
}
fn assistant(session: &Session, turn: u64, name: &str) -> crate::ToolOccurrence {
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "call".into(),
                    name: name.into(),
                    arguments: serde_json::json!({}),
                    raw_arguments: None,
                })],
                provider_replay: Some(
                    ProviderReplay::new(
                        "parent",
                        "opaque",
                        serde_json::json!({"signature": "preserve as data"}),
                    )
                    .with_prefix_binding(true),
                ),
            },
            Usage::unknown(),
            true,
        )
        .unwrap();
    session.tool_occurrence(turn, "call").unwrap()
}

#[test]
fn fork_copies_literal_prefix_and_coordinates_but_not_parent_ownership_or_configuration() {
    for point in [ForkPoint::AfterTurn(1), ForkPoint::BeforeTurn(2)] {
        let root = std::env::temp_dir().join(format!("ion-child-fork-{}", uuid::Uuid::new_v4()));
        let ancestor = Session::create(root.join("ancestor.sqlite"), None).unwrap();
        let parent_model = ModelRef {
            provider: "parent".into(),
            model: "parent".into(),
        };
        ancestor
            .begin_turn("create parent".into(), parent_model.clone())
            .unwrap();
        let origin = assistant(&ancestor, 1, "delegate");
        let (binding, context) = ancestor
            .admit_delegation(
                DelegationSite {
                    session: ancestor.id(),
                    turn: 1,
                    tool: origin,
                    composed_child: None,
                },
                spec(&root),
            )
            .unwrap();
        let parent = Session::materialize_delegation(&binding, context).unwrap();
        {
            let idle = parent.operations.try_lock().unwrap();
            parent
                .record_selection(&idle, parent_model.clone(), Some(Reasoning::High))
                .unwrap();
        }
        let shell = parent
            .admit_user_shell("historical shell context".into(), false)
            .unwrap();
        parent
            .record_user_shell(
                shell,
                serde_json::json!({"stdout": "historical result"}),
                false,
            )
            .unwrap();
        let old_key = RequestKey::new("source-delivery".into(), "accepted".into()).unwrap();
        let authority = RecoveryBinding {
            session: parent.id(),
            authority: AuthorityScope::new("parent-only").unwrap(),
            fingerprint: [7; 32],
        };
        parent
            .admit_turn(
                Message::user_input("history".into(), []),
                parent_model.clone(),
                Some(old_key.clone()),
                Some(authority),
            )
            .unwrap();
        let occurrence = assistant(&parent, 1, "code_mode");
        parent
            .record_child_intent(
                1,
                ChildIntent {
                    parent: occurrence,
                    child: 0,
                    call: ToolCall {
                        id: "nested".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({}),
                        raw_arguments: None,
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
        let (historical, _) = parent
            .admit_delegation(
                DelegationSite {
                    session: parent.id(),
                    turn: 1,
                    tool: occurrence,
                    composed_child: Some(0),
                },
                spec(&root),
            )
            .unwrap();
        parent
            .settle_delegation(
                &historical,
                DelegationOutcome::Joined {
                    receipt: None,
                    error: Some("stopped before materialization".into()),
                },
            )
            .unwrap();
        parent
            .record_child_outcome(1, occurrence, 0, ChildOutcome::Unknown)
            .unwrap();
        parent
            .record_tool_result(
                1,
                ion_ai::ToolResult {
                    call_id: "call".into(),
                    name: "code_mode".into(),
                    result: serde_json::json!({"historical": true}),
                    images: Vec::new(),
                    is_error: false,
                },
            )
            .unwrap();
        parent
            .end_turn(1, TurnEndReason::Completed, &BTreeSet::new())
            .unwrap();
        let end = parent.view().unwrap().entries.len();
        parent
            .record_compaction(
                end as u64,
                "settled summary".into(),
                ModelExecution {
                    route: ion_ai::ModelRoute::direct(
                        parent_model.clone(),
                        ion_ai::ModelRouteReason::Auxiliary,
                    ),
                    returned_model: None,
                },
                Usage::unknown(),
            )
            .unwrap();
        parent
            .begin_turn("delegate fork".into(), parent_model)
            .unwrap();
        let site = assistant(&parent, 2, "delegate");
        let original = parent.view().unwrap().entries;
        let mut child_spec = spec(&root);
        child_spec.context = DelegationContext::Fork { point };
        let workspace = root.join("explicit-child-workspace");
        fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        child_spec.cwd = Some(workspace.clone());
        let (record, context) = parent
            .admit_delegation(
                DelegationSite {
                    session: parent.id(),
                    turn: 2,
                    tool: site,
                    composed_child: None,
                },
                child_spec,
            )
            .unwrap();
        let cutoff = match point {
            ForkPoint::AfterTurn(_) => end,
            ForkPoint::BeforeTurn(_) => original
                .iter()
                .position(|entry| matches!(entry, SessionEntry::TurnStarted { turn: 2, .. }))
                .unwrap(),
        };
        let mut expected = original[..cutoff].to_vec();
        for entry in &mut expected {
            if let SessionEntry::TurnStarted { request, .. } = entry {
                *request = None;
            }
        }
        assert_eq!(
            context, expected,
            "capture must be the literal cut, not ordinary fork's preference augmentation"
        );
        let child = Session::materialize_delegation(&record, context.clone()).unwrap();
        let view = child.view().unwrap();
        assert_eq!(&view.entries[..cutoff], expected);
        assert!(
            matches!(&view.entries[cutoff], SessionEntry::ChildSessionOf { record: actual } if actual == &record)
        );
        assert_eq!(
            view.parent_delegation(),
            Some(&record),
            "ancestor parentage must not shadow the new binding"
        );
        assert!(
            view.delegations().is_empty(),
            "historical children remain owned by the source"
        );
        assert!(child.request_receipt(&old_key).unwrap().is_none());
        assert_eq!(child.cwd(), Some(workspace.as_path()));
        assert_eq!(
            child.model_preference().unwrap(),
            Some(record.spec.model.clone())
        );
        assert_eq!(child.reasoning().unwrap(), Reasoning::Off);
        assert_eq!(
            view.compacted_through,
            matches!(point, ForkPoint::BeforeTurn(_)).then_some(end as u64)
        );
        let mut ambiguous = view.entries.clone();
        ambiguous.push(SessionEntry::ChildSessionOf {
            record: record.clone(),
        });
        assert!(
            matches!(
                project(ambiguous.iter().map(Ok), ProjectionMode::Metadata),
                Err(SessionError::InvalidHistory)
            ),
            "one logical child cannot have two parent bindings"
        );
        let copy = child.clone_to(root.join("copy.sqlite")).unwrap();
        assert!(copy.view().unwrap().parent_delegation().is_none());
        drop(copy);
        drop(child);
        // Same header and link are insufficient if the copied prefix differs.
        let exact = Session::materialize_delegation(&record, context.clone()).unwrap();
        drop(exact);
        let mut wrong = context;
        if let SessionEntry::UserShellSettled {
            outcome: UserShellOutcome::Observed { output, .. },
            ..
        } = wrong
            .iter_mut()
            .find(|entry| matches!(entry, SessionEntry::UserShellSettled { .. }))
            .unwrap()
        {
            *output = serde_json::json!("different history");
        }
        assert!(matches!(
            Session::materialize_delegation(&record, wrong),
            Err(SessionError::InvalidHistory)
        ));
        drop((parent, ancestor));
        fs::remove_dir_all(root).unwrap();
    }
}
