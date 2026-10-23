use super::*;
use crate::{
    DelegationOutcome, DelegationOwnership, DelegationSite, DelegationSpec, DelegationTarget,
};
use ion_ai::{Content, Role, ToolCall};

fn scope(root: &Path, name: &str) -> (Session, DelegationSite) {
    let parent = Session::create(root.join(format!("{name}.sqlite")), None).unwrap();
    let site = invocation(&parent);
    (parent, site)
}
fn invocation(parent: &Session) -> DelegationSite {
    let (turn, _) = parent.begin_turn("delegate".into(), model()).unwrap();
    parent
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "delegate".into(),
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
    DelegationSite {
        session: parent.id(),
        turn,
        tool: parent.tool_occurrence(turn, "delegate").unwrap(),
        composed_child: None,
    }
}
fn model() -> ModelRef {
    ModelRef {
        provider: "fixture".into(),
        model: "child".into(),
    }
}
fn spec(target: DelegationTarget) -> DelegationSpec {
    DelegationSpec {
        target,
        cwd: None,
        model: model(),
        reasoning: Reasoning::Off,
        execution_timeout_ms: None,
        authority: AuthorityScope::new("current-application/v1").unwrap(),
        input: Message::user_input("new request".into(), []),
    }
}
fn complete(child: &Session, record: &crate::DelegationRecord, text: &str) -> RequestReceipt {
    let key = crate::delegation::request_key(record.run);
    let admission = child
        .admit_turn(
            record.spec.input.clone(),
            record.spec.model.clone(),
            Some(key.clone()),
            None,
        )
        .unwrap();
    let TurnAdmission::New { turn, .. } = admission else {
        panic!("a new run must admit a new request")
    };
    child
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text(text.into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    child.request_receipt(&key).unwrap().unwrap()
}
fn joined(receipt: RequestReceipt, text: &str) -> DelegationOutcome {
    DelegationOutcome::Joined {
        execution_timed_out: false,
        receipt: Some(receipt),
        error: None,
        answer: Some(text.into()),
    }
}

#[test]
fn follow_up_keeps_conversation_but_not_run_identity_receipt_or_authority() {
    for other_parent in [false, true] {
        let root = std::env::temp_dir().join(format!("ion-follow-up-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let (first_parent, first_site) = scope(&root, "parent");
        let (first, prepared) = first_parent
            .admit_delegation(
                first_site,
                spec(DelegationTarget::Fresh {
                    directory: root.join("children"),
                }),
                DelegationOwnership::Foreground,
            )
            .unwrap();
        assert_ne!(first.run, first.child);
        let child = Session::materialize_delegation(&first, prepared).unwrap();
        let first_receipt = complete(&child, &first, "first actual answer");
        first_parent
            .settle_delegation(&first, joined(first_receipt.clone(), "first actual answer"))
            .unwrap();
        let old_history = child.view().unwrap().entries;
        let old_result = first_parent.view().unwrap().delegations()[0].clone();
        let second_parent = other_parent.then(|| scope(&root, "other-parent"));
        let parent = second_parent
            .as_ref()
            .map(|(parent, _)| parent)
            .unwrap_or(&first_parent);
        let site = second_parent
            .as_ref()
            .map(|(_, site)| *site)
            .unwrap_or_else(|| invocation(parent));
        let mut next = spec(DelegationTarget::resume(&first.path).unwrap());
        next.input = Message::user_input("follow-up literal /task".into(), []);
        next.model.model = "new-route".into();
        next.reasoning = Reasoning::Low;
        next.authority = AuthorityScope::new("new-authenticated-application/v2").unwrap();
        let before = parent.view().unwrap().entries;
        assert!(matches!(
            parent.admit_delegation(site, next.clone(), DelegationOwnership::Foreground),
            Err(SessionError::DelegationTargetUnavailable(_))
        ));
        assert_eq!(
            parent.view().unwrap().entries,
            before,
            "a retained writer refuses before intent"
        );
        drop(child);
        let (second, prepared) = parent
            .admit_delegation(site, next, DelegationOwnership::Foreground)
            .unwrap();
        assert_eq!(second.child, first.child);
        assert_ne!(second.run, first.run);
        assert_eq!(
            second.spec.authority.as_str(),
            "new-authenticated-application/v2"
        );
        assert!(
            matches!(Session::open(&first.path), Err(SessionError::AlreadyOpen)),
            "preparation retains the exclusive writer before dispatch"
        );
        let admitted = parent.view().unwrap().entries;
        assert!(
            matches!(
                parent.settle_delegation(&second, joined(first_receipt.clone(), "old answer")),
                Err(SessionError::InvalidHistory)
            ),
            "the old receipt cannot fulfill a new run in the same Session"
        );
        assert_eq!(parent.view().unwrap().entries, admitted);
        let child = Session::materialize_delegation(&second, prepared).unwrap();
        let view = child.view().unwrap();
        assert!(
            view.entries.starts_with(&old_history),
            "resume must append, not replace or fork"
        );
        assert_eq!(view.parent_delegation(), Some(&second));
        assert_eq!(view.last_model, Some(second.spec.model.clone()));
        assert_eq!(view.reasoning, Reasoning::Low);
        let second_receipt = complete(&child, &second, "new actual answer");
        assert_eq!(second_receipt.turn, first_receipt.turn + 1);
        assert_eq!(
            child.request_receipt(&first_receipt.key).unwrap(),
            Some(first_receipt)
        );
        parent
            .settle_delegation(&second, joined(second_receipt, "new actual answer"))
            .unwrap();
        assert_eq!(first_parent.view().unwrap().delegations()[0], old_result);
        assert_eq!(
            parent.view().unwrap().delegations().last().unwrap().record,
            second
        );
        let history = child.view().unwrap().entries;
        drop(child);
        let reopened = Session::open(&first.path).unwrap();
        assert_eq!(reopened.id(), first.child);
        assert_eq!(reopened.view().unwrap().entries, history);
        drop((reopened, second_parent, first_parent));
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn resume_refuses_self_wrong_identity_workspace_and_unsettled_history_before_intent() {
    for case in [
        "self",
        "identity",
        "workspace",
        "unfinished",
        "missing",
        "background",
    ] {
        let root =
            std::env::temp_dir().join(format!("ion-follow-up-refusal-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let (parent, site) = scope(&root, "parent");
        let (target, _) = scope(&root, "target");
        if case != "unfinished" {
            target
                .end_turn(1, TurnEndReason::Cancelled, &BTreeSet::new())
                .unwrap();
        }
        if case == "background" {
            let nested = invocation(&target);
            let (record, _) = target
                .admit_delegation(
                    nested,
                    spec(DelegationTarget::Fresh {
                        directory: root.join("nested"),
                    }),
                    DelegationOwnership::Background {
                        owner: uuid::Uuid::new_v4(),
                    },
                )
                .unwrap();
            let lease = Arc::new(());
            target.transfer_delegation(&record, &lease).unwrap();
            target
                .record_tool_settlement_with_context(
                    nested.turn,
                    crate::ToolSettlement {
                        call_id: "delegate".into(),
                        name: "delegate".into(),
                        outcome: crate::ToolOutcome::Unknown,
                    },
                    None,
                )
                .unwrap();
            target
                .record_assistant(
                    nested.turn,
                    Message {
                        role: Role::Assistant,
                        content: vec![Content::Text("background transferred".into())],
                        provider_replay: None,
                    },
                    Usage::unknown(),
                    false,
                )
                .unwrap();
        }
        let path = target.path().to_path_buf();
        let target_history = target.view().unwrap().entries;
        assert!(
            target.recovery_state().unwrap().is_none(),
            "lack of a recovery binding is not proof of settlement"
        );
        let mut next = spec(if case == "self" {
            DelegationTarget::resume(parent.path()).unwrap()
        } else {
            DelegationTarget::resume(&path).unwrap()
        });
        match case {
            "identity" => {
                next.target = DelegationTarget::Resume {
                    path: path.clone(),
                    session: uuid::Uuid::new_v4(),
                }
            }
            "workspace" => next.cwd = Some(root.canonicalize().unwrap()),
            "missing" => {
                next.target = DelegationTarget::Resume {
                    path: root.join("missing.sqlite"),
                    session: target.id(),
                }
            }
            _ => {}
        }
        drop(target);
        let before = parent.view().unwrap().entries;
        assert!(
            matches!(
                parent.admit_delegation(site, next, DelegationOwnership::Foreground),
                Err(SessionError::DelegationTargetUnavailable(_))
            ),
            "{case}"
        );
        assert_eq!(
            parent.view().unwrap().entries,
            before,
            "{case}: no admission"
        );
        assert_eq!(
            Session::inspect(&path).unwrap().entries,
            target_history,
            "{case}: no repair or replay"
        );
        assert!(!root.join("missing.sqlite").exists());
        drop(parent);
        fs::remove_dir_all(root).unwrap();
    }
}
