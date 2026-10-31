use super::*;

#[test]
fn background_refusal_has_no_session_fault() {
    let error: Box<dyn std::error::Error + Send + Sync> =
        Box::new(BackgroundStartError::Refused("owner at capacity".into()));
    assert!(matches!(
        error.downcast_ref::<BackgroundStartError>(),
        Some(BackgroundStartError::Refused(reason)) if reason == "owner at capacity"
    ));
    assert!(error.source().is_none());
}

#[test]
fn background_infrastructure_preserves_the_typed_session_cause() {
    let error: Box<dyn std::error::Error + Send + Sync> = Box::new(
        BackgroundStartError::Infrastructure(crate::SessionError::InvalidHistory),
    );
    assert!(matches!(
        error.downcast_ref::<BackgroundStartError>(),
        Some(BackgroundStartError::Infrastructure(_))
    ));
    assert!(matches!(
        error
            .source()
            .and_then(|cause| cause.downcast_ref::<crate::SessionError>()),
        Some(crate::SessionError::InvalidHistory)
    ));
}

#[tokio::test]
async fn delegation_requires_live_authority_and_a_valid_context() {
    let root = std::env::temp_dir().join(format!("ion-delegation-port-{}", uuid::Uuid::new_v4()));
    let session = Arc::new(Session::create(root.join("parent.sqlite"), None).unwrap());
    let model = ModelRef {
        provider: "fixture".into(),
        model: "fixture".into(),
    };
    session
        .begin_turn("delegate".into(), model.clone())
        .unwrap();
    let call = ion_ai::ToolCall {
        id: "call".into(),
        name: "delegate".into(),
        arguments: serde_json::json!({"session": session.id(), "turn": 1, "assistant_entry": 2, "ordinal": 0}),
        raw_arguments: None,
    };
    session
        .record_assistant(
            1,
            Message {
                role: ion_ai::Role::Assistant,
                content: vec![ion_ai::Content::ToolCall(call.clone())],
                provider_replay: None,
            },
            ion_ai::Usage::unknown(),
            false,
        )
        .unwrap();
    let before = session.view().unwrap().entries;
    let task = |context| {
        DelegationTask::new(
            DelegationSpec {
                target: context,
                cwd: None,
                model: model.clone(),
                reasoning: Reasoning::ProviderDefault,
                execution_timeout_ms: None,
                authority: AuthorityScope::new("child/v1").unwrap(),
                input: Message::user_input("child".into(), []),
            },
            |_, _, _| panic!("cold child work ran without live dispatch authority"),
        )
        .unwrap()
    };
    let untracked = crate::ToolInvocation::untracked(&call, CancellationToken::new());
    assert!(
        untracked
            .delegate(task(DelegationTarget::Fresh {
                directory: root.join("children")
            }))
            .await
            .is_err()
    );
    let live = CancellationToken::new();
    let (invocation, gateway) = crate::ToolInvocation::admitted(
        &call,
        live.clone(),
        session.id(),
        1,
        session.tool_occurrence(1, "call").unwrap(),
        None,
        ion_ai::Reasoning::ProviderDefault,
    );
    let rejected = gateway
        .run(
            &session,
            &live,
            invocation.delegate(task(DelegationTarget::Fork {
                directory: root.join("children"),
                point: crate::ForkPoint::AfterTurn(1),
            })),
        )
        .await
        .unwrap();
    assert!(
        rejected.is_err(),
        "an unsettled Turn is not a valid fork boundary"
    );
    assert!(
        !live.is_cancelled(),
        "invalid context choice is a refusal, not an infrastructure fault"
    );
    assert!(
        invocation
            .delegate(task(DelegationTarget::Fresh {
                directory: root.join("children")
            }))
            .await
            .is_err(),
        "a surviving historical invocation cannot revive its closed port"
    );
    let stop = CancellationToken::new();
    let (invocation, gateway) = crate::ToolInvocation::admitted(
        &call,
        stop.clone(),
        session.id(),
        1,
        session.tool_occurrence(1, "call").unwrap(),
        None,
        ion_ai::Reasoning::ProviderDefault,
    );
    stop.cancel();
    assert!(
        gateway
            .run(
                &session,
                &stop,
                invocation.delegate(task(DelegationTarget::Fresh {
                    directory: root.join("children")
                }))
            )
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(session.view().unwrap().entries, before);
    assert!(!root.join("children").exists());
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}
