use super::*;

fn model(name: &str) -> ModelRef {
    ModelRef {
        provider: "test".into(),
        model: name.into(),
    }
}
fn answer(text: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![Content::Text(text.into())],
        provider_replay: Some(ion_ai::ProviderReplay::new(
            "test",
            "fixture",
            serde_json::json!({"signature": text}),
        )),
    }
}
fn fixture() -> (PathBuf, PathBuf, Session) {
    let root = std::env::temp_dir().join(format!("ion-navigation-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let path = root.join("session.sqlite");
    let session = Session::create(&path, Some(&root)).unwrap();
    (root, path, session)
}
fn finish(session: &Session, input: &str, output: &str) -> u64 {
    let (turn, _) = session.begin_turn(input.into(), model("one")).unwrap();
    session
        .record_assistant(turn, answer(output), Usage::unknown(), false)
        .unwrap();
    turn
}
fn select(session: &Session, head: u64) -> ContextPosition {
    session
        .prepare_navigation(session.context_position().unwrap(), head)
        .unwrap()
        .commit()
        .unwrap()
}

#[test]
fn status_tracks_renaming_reasoning_selection_and_reopen() {
    let (root, path, session) = fixture();
    assert_eq!(session.status().unwrap().entries, 0);
    session.set_name(Some("  retained name  ")).unwrap();
    {
        let idle = session.operations.try_lock().unwrap();
        session
            .record_selection(&idle, model("one"), Some(Reasoning::High))
            .unwrap();
    }
    finish(&session, "original", "answer");
    let current = session.status().unwrap();
    assert_eq!(current.cwd, Some(fs::canonicalize(&root).unwrap()));
    assert_eq!(current.name.as_deref(), Some("retained name"));
    assert_eq!(current.reasoning, Reasoning::High);
    assert_eq!(current.entries, session.entry_count().unwrap());
    select(&session, 0);
    let selected = session.status().unwrap();
    assert_eq!(selected.entries, current.entries + 1);
    assert_eq!(selected.reasoning, Reasoning::ProviderDefault);
    assert_eq!(selected.name, current.name);
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.status().unwrap(), selected);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn alternatives_survive_selection_reopen_copies_and_new_global_turn_ids() {
    let (root, path, session) = fixture();
    let key = RequestKey::new("navigation".into(), "original".into()).unwrap();
    let input = Message::user_input("first".into(), []);
    assert!(matches!(
        session
            .admit_turn(input.clone(), model("one"), Some(key.clone()), None)
            .unwrap(),
        TurnAdmission::New { turn: 1, .. }
    ));
    session
        .record_assistant(1, answer("original"), Usage::unknown(), false)
        .unwrap();
    let original = session.view().unwrap();
    let original_head = original.context_position().head_entry;
    let original_answer = original
        .context_points()
        .into_iter()
        .find(|point| !point.restore_input)
        .unwrap();
    let position = select(&session, 0);
    assert_eq!(position.session, original.id);
    assert!(session.messages().unwrap().is_empty());
    assert!(
        matches!(
            session
                .admit_turn(input, model("one"), Some(key), None)
                .unwrap(),
            TurnAdmission::Existing(_)
        ),
        "navigation cannot forget an accepted external delivery"
    );
    assert_eq!(session.context_position().unwrap(), position);
    assert_eq!(
        Session::inspect(&path).unwrap().context_position(),
        position
    );
    drop(session);
    let session = Session::open(&path).unwrap();
    assert_eq!(
        session.context_position().unwrap(),
        position,
        "selection persists without a new prompt"
    );
    assert!(session.context_messages().unwrap().is_empty());
    assert_eq!(finish(&session, "alternative", "alternate result"), 2);
    let alternate_head = session.context_position().unwrap().head_entry;
    let alternate_entries = session.view().unwrap().entries;
    assert_eq!(
        &alternate_entries[..original.entries.len()],
        &original.entries
    );
    select(&session, original_head);
    assert_eq!(session.messages().unwrap().len(), 2);
    assert!(
        serde_json::to_string(&session.context_messages().unwrap())
            .unwrap()
            .contains("original")
    );
    assert!(
        !serde_json::to_string(&session.context_messages().unwrap())
            .unwrap()
            .contains("alternate result")
    );
    assert!(
        session
            .context_messages_for(&model("one"))
            .unwrap()
            .iter()
            .all(|message| message.provider_replay.is_none())
    );
    let view = session.view().unwrap();
    assert_eq!(view.entries[..alternate_entries.len()], alternate_entries);
    assert!(
        view.context_points()
            .iter()
            .any(|point| !point.selected_path && point.message == answer("alternate result"))
    );
    let transcript = crate::TranscriptProjection::from_session(&view);
    let rendered = format!("{transcript:?}");
    assert!(rendered.contains("original") && !rendered.contains("alternate result"));
    let clone = session.clone_to(root.join("clone.sqlite")).unwrap();
    assert_ne!(clone.id(), session.id());
    assert_eq!(
        clone.context_messages().unwrap(),
        session.context_messages().unwrap()
    );
    assert_eq!(clone.view().unwrap().context_path, view.context_path);
    drop(clone);
    let fork = session
        .fork_to(root.join("fork.sqlite"), ForkPoint::AfterTurn(2))
        .unwrap();
    assert!(
        serde_json::to_string(&fork.context_messages().unwrap())
            .unwrap()
            .contains("alternate result")
    );
    drop(fork);
    select(&session, alternate_head);
    assert_eq!(finish(&session, "continue alternate", "third"), 3);
    let alternate = session.view().unwrap();
    let projected = crate::TranscriptProjection::from_session(&alternate);
    let expected_sources = alternate
        .entries
        .iter()
        .enumerate()
        .filter_map(|(index, fact)| {
            let entry = index as u64 + 1;
            (alternate.context_path.contains(&entry)
                && matches!(
                    fact,
                    SessionEntry::TurnStarted { .. } | SessionEntry::Assistant { .. }
                ))
            .then_some(crate::TranscriptSource { entry, part: 0 })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        projected
            .items
            .iter()
            .map(|item| item.source().unwrap())
            .collect::<Vec<_>>(),
        expected_sources,
        "selected context never renumbers global source coordinates"
    );
    select(&session, original_answer.head_entry);
    let before = session.view().unwrap();
    assert_eq!(before.messages.len(), 2);
    assert_eq!(
        Session::inspect_metadata(&path).unwrap().turns,
        3,
        "catalog counts global executions"
    );
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.view().unwrap().messages, before.messages);
    assert_eq!(reopened.view().unwrap().context_path, before.context_path);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stale_previews_exclusive_preparation_and_failed_commits_do_not_publish() {
    let (root, path, session) = fixture();
    finish(&session, "first", "answer");
    let stale = session.context_position().unwrap();
    session.select_model(model("two")).unwrap();
    assert!(matches!(
        session.prepare_navigation(stale, 0),
        Err(SessionError::StaleContextPosition)
    ));
    let before = session.context_position().unwrap();
    let prepared = session.prepare_navigation(before, 0).unwrap();
    assert!(matches!(
        session.prepare_navigation(before, 0),
        Err(SessionError::OperationActive)
    ));
    drop(prepared);
    assert_eq!(session.context_position().unwrap(), before);
    session.store.lock().unwrap().connection.execute_batch("CREATE TRIGGER reject_navigation BEFORE INSERT ON entries BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
    assert!(
        session
            .prepare_navigation(before, 0)
            .unwrap()
            .commit()
            .is_err()
    );
    assert_eq!(session.context_position().unwrap(), before);
    assert_eq!(session.model_preference().unwrap(), Some(model("two")));
    assert_eq!(Session::inspect(&path).unwrap().context_position(), before);
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute_batch("DROP TRIGGER reject_navigation")
        .unwrap();
    let mut committed = session.prepare_navigation(before, 0).unwrap();
    committed.commit().unwrap();
    assert!(
        matches!(session.reserve_idle(), Err(SessionError::OperationActive)),
        "publication retains the engine gate after durable commit"
    );
    drop(committed);
    session.ensure_idle().unwrap();
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn complete_tool_groups_context_metadata_and_compaction_follow_the_selected_path() {
    let (root, path, session) = fixture();
    finish(&session, "first", "answer");
    let old_head = session.context_position().unwrap().head_entry;
    let execution = ModelExecution {
        route: ion_ai::ModelRoute::direct(model("one"), ion_ai::ModelRouteReason::Auxiliary),
        returned_model: None,
    };
    session
        .record_compaction(
            old_head,
            "selected prefix summary".into(),
            execution,
            Usage::unknown(),
        )
        .unwrap();
    let compacted_head = session.context_position().unwrap().head_entry;
    let (turn, _) = session
        .begin_turn("use a tool".into(), model("two"))
        .unwrap();
    session
        .admit_request(
            turn,
            model("two"),
            ModelContextSnapshot {
                instructions: "two instructions".into(),
                tools: Vec::new(),
            },
        )
        .unwrap();
    let tool_call = Message {
        role: Role::Assistant,
        content: vec![Content::ToolCall(ToolCall {
            id: "call".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
            raw_arguments: None,
        })],
        provider_replay: None,
    };
    session
        .record_assistant(turn, tool_call, Usage::unknown(), false)
        .unwrap();
    let unsettled = session.entry_count().unwrap();
    session
        .record_tool_result(
            turn,
            ToolResult {
                call_id: "call".into(),
                name: "read".into(),
                result: serde_json::json!({"observed":true}),
                images: Vec::new(),
                is_error: false,
            },
        )
        .unwrap();
    let settled = session.entry_count().unwrap();
    session
        .end_turn(turn, TurnEndReason::Completed, &BTreeSet::new())
        .unwrap();
    let points = session.view().unwrap().context_points();
    assert!(!points.iter().any(|point| point.entry == unsettled));
    assert!(points.iter().any(|point| point.entry == settled
        && point.head_entry == settled
        && !point.restore_input));
    assert!(matches!(
        session.prepare_navigation(session.context_position().unwrap(), unsettled),
        Err(SessionError::InvalidContextPoint)
    ));
    select(&session, settled);
    assert_eq!(session.model_preference().unwrap(), Some(model("two")));
    assert_eq!(
        session.model_context().unwrap().unwrap().instructions,
        "two instructions"
    );
    assert!(matches!(
        session.context_messages().unwrap().last().unwrap().role,
        Role::Tool
    ));
    select(&session, compacted_head);
    assert_eq!(session.model_preference().unwrap(), Some(model("one")));
    assert_eq!(session.context_messages().unwrap().len(), 1);
    assert!(session.model_context().unwrap().is_none());
    select(&session, old_head);
    assert!(session.view().unwrap().compacted_through.is_none());
    assert_eq!(session.context_messages().unwrap().len(), 2);
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.context_messages().unwrap().len(), 2);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupted_effects_close_atomically_before_selection_without_replay() {
    let (root, path, session) = fixture();
    session
        .admit_user_shell("touch unknown-file".into(), false)
        .unwrap();
    let before = session.view().unwrap();
    session.store.lock().unwrap().connection.execute_batch("CREATE TRIGGER reject_navigation BEFORE INSERT ON entries WHEN json_extract(NEW.body,'$.kind') = 'context_selected' BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
    assert!(
        session
            .prepare_navigation(before.context_position(), 0)
            .unwrap()
            .commit()
            .is_err()
    );
    assert_eq!(
        session.view().unwrap().entries,
        before.entries,
        "unknown settlement cannot publish alone"
    );
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute_batch("DROP TRIGGER reject_navigation")
        .unwrap();
    select(&session, 0);
    let view = session.view().unwrap();
    assert!(matches!(
        view.entries[1],
        SessionEntry::UserShellSettled {
            outcome: UserShellOutcome::Unknown,
            ..
        }
    ));
    assert!(view.messages.is_empty());
    assert!(!root.join("unknown-file").exists());
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(reopened.view().unwrap().unfinished_user_shell.is_none());
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn turn_summaries_follow_restored_reasoning_without_renumbering_turns() {
    let (root, _, session) = fixture();
    let idle = session.reserve_idle().unwrap();
    session
        .record_selection(&idle, model("one"), Some(Reasoning::High))
        .unwrap();
    drop(idle);
    finish(&session, "high", "first");
    select(&session, 0);
    finish(&session, "default", "second");
    let turns = session.view().unwrap().turns();
    assert_eq!(turns[0].reasoning, Reasoning::High);
    assert_eq!(turns[1].reasoning, Reasoning::ProviderDefault);
    assert_eq!(turns[1].turn, 2);
    let fork = session
        .fork_to(root.join("fork.sqlite"), ForkPoint::AfterTurn(2))
        .unwrap();
    assert_eq!(fork.reasoning().unwrap(), turns[1].reasoning);
    drop(fork);
    let (third, _) = session
        .begin_turn("interrupted".into(), model("two"))
        .unwrap();
    let idle = session.reserve_idle().unwrap();
    session
        .record_selection(&idle, model("two"), Some(Reasoning::Off))
        .unwrap();
    drop(idle);
    session
        .end_turn(third, TurnEndReason::Interrupted, &BTreeSet::new())
        .unwrap();
    assert_eq!(
        session
            .fork_preferences(ForkPoint::BeforeTurn(third))
            .unwrap(),
        (model("two"), Reasoning::ProviderDefault)
    );
    assert_eq!(
        session
            .fork_preferences(ForkPoint::AfterTurn(third))
            .unwrap(),
        (model("two"), Reasoning::Off)
    );
    let fork = session
        .fork_to(root.join("after.sqlite"), ForkPoint::AfterTurn(third))
        .unwrap();
    assert_eq!(fork.reasoning().unwrap(), Reasoning::Off);
    drop(fork);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn late_background_results_keep_the_initiating_path_and_live_ownership() {
    let (root, session, _, record) = super::delegation_tests::pending_for(
        false,
        crate::DelegationOwnership::Background {
            owner: uuid::Uuid::new_v4(),
        },
    );
    let lease = Arc::new(());
    session.transfer_delegation(&record, &lease).unwrap();
    session
        .record_tool_result(
            1,
            ToolResult {
                call_id: "call".into(),
                name: "delegate".into(),
                result: serde_json::Value::Null,
                images: Vec::new(),
                is_error: false,
            },
        )
        .unwrap();
    session
        .end_turn(1, TurnEndReason::Completed, &BTreeSet::new())
        .unwrap();
    let source_head = session.context_position().unwrap().head_entry;
    select(&session, 0);
    assert!(session.view().unwrap().delegations()[0].outcome.is_none());
    let mut prepared = session
        .prepare_navigation(session.context_position().unwrap(), source_head)
        .unwrap();
    session
        .settle_delegation(&record, crate::DelegationOutcome::Unknown)
        .unwrap();
    assert!(
        matches!(prepared.commit(), Err(SessionError::StaleContextPosition)),
        "late outcome invalidates prepared publication without moving selected context"
    );
    drop(prepared);
    let position = session.context_position().unwrap();
    assert_eq!(position.head_entry, 0);
    assert!(session.messages().unwrap().is_empty());
    assert_eq!(
        session.view().unwrap().delegations()[0].outcome,
        Some(crate::DelegationOutcome::Unknown)
    );
    select(&session, source_head);
    assert_eq!(session.messages().unwrap().len(), 3);
    let path = session.path().to_path_buf();
    drop(session);
    let reopened = Session::open(path).unwrap();
    assert_eq!(reopened.messages().unwrap().len(), 3);
    drop(reopened);
    drop(lease);
    fs::remove_dir_all(root).unwrap();
}
