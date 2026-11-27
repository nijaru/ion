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
fn compacted_payloads_leave_the_working_set_but_remain_inspectable() {
    let (root, path, session) = fixture();
    for _ in 0..12 {
        finish(&session, "inspectable input", &"x".repeat(32 * 1024));
    }
    let original = session.view().unwrap();
    let through = session.entry_count().unwrap();
    session
        .record_compaction(
            through,
            "summary".into(),
            original.last_execution.clone().unwrap(),
            Usage::unknown(),
        )
        .unwrap();
    assert_eq!(session.view().unwrap().messages, original.messages);
    assert!(
        session.store().lock().unwrap().working.messages.is_empty(),
        "archived payloads remain resident after compaction"
    );
    assert!(
        session
            .store()
            .lock()
            .unwrap()
            .working
            .context_changes
            .is_empty()
    );
    let expected = session.context_messages().unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.context_messages().unwrap(), expected);
    assert_eq!(reopened.view().unwrap().messages, original.messages);
    assert!(
        reopened.store().lock().unwrap().working.messages.is_empty(),
        "reopen resurrected the compacted archive into the working set"
    );
    select(&reopened, through);
    let selected = original
        .messages
        .into_iter()
        .map(|mut message| {
            message.provider_replay = None;
            message
        })
        .collect::<Vec<_>>();
    assert_eq!(reopened.context_messages().unwrap(), selected);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn preference_replay_does_not_reread_selected_transcripts() {
    let (root, _, session) = fixture();
    let mut heads = Vec::new();
    for _ in 0..60 {
        finish(&session, "input", "answer");
        heads.push(session.entry_count().unwrap());
    }
    let original = session.view().unwrap().entries;
    let reads = |head: u64| {
        let mut entries = original[..head as usize].to_vec();
        for _ in 0..32 {
            entries.push(SessionEntry::ContextSelected { head_entry: head });
        }
        let reads = std::cell::Cell::new(0usize);
        let projection = project(
            entries.iter().map(Ok),
            ProjectionMode::Metadata,
            &|coordinate| {
                reads.set(reads.get() + 1);
                Ok(entries[(coordinate - 1) as usize].clone())
            },
        )
        .unwrap();
        assert_eq!(projection.state.last_model, Some(model("one")));
        project(
            entries.iter().map(Ok),
            ProjectionMode::Working,
            &|coordinate| {
                let entry = &entries[(coordinate - 1) as usize];
                assert!(
                    !matches!(entry, SessionEntry::TurnEnded { .. }),
                    "working context rereads metadata with no message payload"
                );
                Ok(entry.clone())
            },
        )
        .unwrap();
        reads.get()
    };
    assert_eq!(
        reads(heads[3]),
        reads(heads[59]),
        "restoring preferences rereads the entire selected transcript"
    );
    drop(session);
    fs::remove_dir_all(root).unwrap();
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
fn observed_turn_advances_the_cut_only_if_admission_follows_the_prefix() {
    let (root, _path, session) = fixture();
    let prefix = session.presentation(None).unwrap().checkpoint.unwrap();
    let (turn, _) = session.begin_turn("observed".into(), model("one")).unwrap();
    let admission = session.context_position().unwrap().head_entry;
    session
        .record_assistant(turn, answer("reply"), Usage::unknown(), false)
        .unwrap();
    let closure = session.context_position().unwrap().head_entry;
    let cut = session
        .observed_turn_checkpoint(prefix, admission, closure)
        .unwrap()
        .unwrap();
    let next = session.presentation(Some(cut)).unwrap();
    assert_eq!(next.after, Some(cut));
    assert!(next.transcript.items.is_empty());

    session
        .admit_user_shell("unknown preceding shell".into(), true)
        .unwrap();
    let (turn, _) = session
        .begin_turn("another observed turn".into(), model("one"))
        .unwrap();
    let admission = session.context_position().unwrap().head_entry;
    session
        .record_assistant(turn, answer("reply"), Usage::unknown(), false)
        .unwrap();
    let closure = session.context_position().unwrap().head_entry;
    assert!(
        session
            .observed_turn_checkpoint(cut, admission, closure)
            .unwrap()
            .is_none()
    );
    let repaired = session.presentation(Some(cut)).unwrap();
    assert!(matches!(
        repaired.transcript.items.first(),
        Some(crate::TranscriptItem::UserShell(_))
    ));
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn terminal_refresh_reuses_only_idle_cuts_and_catches_up_in_selected_order() {
    let (root, path, session) = fixture();
    finish(&session, "base", "base reply");
    let base = session.presentation(None).unwrap();
    let at_base = base.checkpoint.expect("settled turn closes its transcript");
    let admission = session
        .admit_user_shell("printf pending".into(), true)
        .unwrap();
    let pending = session.presentation(Some(at_base)).unwrap();
    assert_eq!(pending.after, Some(at_base));
    assert!(pending.checkpoint.is_none());
    assert!(
        matches!(&pending.transcript.items[..], [crate::TranscriptItem::UserShell(shell)] if shell.outcome.is_none())
    );
    session
        .record_user_shell(
            admission,
            crate::ToolOutput::success(serde_json::json!({"stdout":"done"})),
        )
        .unwrap();
    // A gap larger than an Alt-T page is a suffix to catch up, not a seed
    // from the latest page: every unseen commit must reach native history.
    for index in 0..64 {
        finish(&session, &format!("request {index}"), "reply");
    }
    let caught_up = session.presentation(Some(at_base)).unwrap();
    assert_eq!(caught_up.after, Some(at_base));
    assert_eq!(caught_up.transcript.items.len(), 129);
    assert!(
        matches!(&caught_up.transcript.items[0], crate::TranscriptItem::UserShell(shell) if shell.outcome.is_some())
    );
    let full = session.presentation(None).unwrap();
    assert_eq!(
        full.transcript.items,
        [base.transcript.items, caught_up.transcript.items].concat()
    );
    let resumed = session.presentation(caught_up.checkpoint).unwrap();
    assert!(resumed.after.is_some());
    assert!(resumed.transcript.items.is_empty());

    // Navigation can land inside a prior turn. Being idle now does not
    // certify the selected historical prefix as a projection boundary.
    select(&session, base.head_entry - 1);
    assert!(session.presentation(None).unwrap().checkpoint.is_none());
    select(&session, base.head_entry);
    let branch = session.presentation(caught_up.checkpoint).unwrap();
    assert!(branch.after.is_none());
    assert_eq!(branch.transcript.items.len(), 2);
    assert!(branch.checkpoint.is_some());
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(
        reopened.presentation(branch.checkpoint).unwrap().after,
        branch.checkpoint
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn editor_recall_bounds_recent_input_count_and_bytes_across_branches() {
    let (root, path, session) = fixture();
    for n in 0..100 {
        finish(&session, &format!("input {n:03}"), "answer");
    }
    // All these inputs become inactive; their text is still useful for recall.
    select(&session, 0);
    assert_eq!(
        session.presentation(None).unwrap().prompts,
        (4..100)
            .map(|n| format!("input {n:03}"))
            .collect::<Vec<_>>()
    );

    let wide = ['a', 'b', 'c', 'd'].map(|c| c.to_string().repeat(MAX_RECALL_PROMPT_BYTES));
    for input in &wide {
        finish(&session, input, "answer");
        select(&session, 0);
    }
    finish(&session, &"x".repeat(MAX_RECALL_PROMPT_BYTES + 1), "answer");
    select(&session, 0);
    finish(&session, " exact latest ", "answer");
    let snapshot = session.presentation(None).unwrap();
    let expected = [wide[1].as_str(), &wide[2], &wide[3], " exact latest "];
    assert_eq!(snapshot.prompts, expected);
    assert_eq!(session.recent_prompts().unwrap(), expected);
    assert_eq!(snapshot.transcript.items.len(), 2);
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(
        reopened.presentation(None).unwrap().prompts,
        snapshot.prompts
    );
    assert_eq!(
        reopened
            .view()
            .unwrap()
            .entries
            .iter()
            .filter(|entry| matches!(entry, SessionEntry::TurnStarted { .. }))
            .count(),
        106
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn terminal_snapshot_streams_selected_facts_but_keeps_global_recall_and_usage() {
    use crate::{TranscriptItem, TranscriptProjection};

    let (root, path, session) = fixture();
    let verify = |session: &Session, prompts: &[&str], selection| {
        let snapshot = session.presentation(None).unwrap();
        let view = session.view().unwrap();
        assert_eq!(
            snapshot.transcript,
            TranscriptProjection::from_session(&view)
        );
        assert_eq!(snapshot.id, view.id);
        assert_eq!(snapshot.selection, selection);
        assert_eq!(snapshot.head_entry, view.context_position().head_entry);
        assert_eq!(snapshot.revision, view.entries.len() as u64);
        assert_eq!(snapshot.name.as_deref(), Some("branches"));
        assert_eq!(snapshot.reasoning, view.reasoning);
        assert_eq!(snapshot.prompts, prompts);
        assert_eq!(snapshot.usage, view.usage().total());
        snapshot
    };

    session.set_name(Some("branches")).unwrap();
    finish(&session, "base", "root reply");
    let base = session.entry_count().unwrap();
    finish(&session, "inactive prompt", "inactive reply");
    let selection = select(&session, base).revision;
    let admission = session
        .admit_user_shell("printf selected".into(), true)
        .unwrap();
    let pending = verify(&session, &["base", "inactive prompt"], selection);
    assert!(pending.unfinished);
    assert!(
        matches!(pending.transcript.items.last(), Some(TranscriptItem::UserShell(shell))
        if shell.source.entry == admission && shell.outcome.is_none())
    );
    session
        .record_user_shell(
            admission,
            crate::ToolOutput::success(serde_json::json!({"stdout": "selected"})),
        )
        .unwrap();
    finish(&session, "selected prompt", "selected reply");
    let prompts = ["base", "inactive prompt", "selected prompt"];
    let selected = verify(&session, &prompts, selection);
    assert!(!selected.unfinished);
    assert_eq!(selected.usage.observations, 3);
    assert_eq!(selected.transcript.items.len(), 5);
    assert!(
        matches!(&selected.transcript.items[2], TranscriptItem::UserShell(shell)
        if shell.source.entry == admission && shell.outcome.is_some())
    );
    drop(session);
    let reopened = Session::open(&path).unwrap();
    verify(&reopened, &prompts, selection);
    let cleared = select(&reopened, 0).revision;
    assert!(
        verify(&reopened, &prompts, cleared)
            .transcript
            .items
            .is_empty()
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn transcript_pages_remain_on_their_committed_cut_until_selection_changes() {
    use crate::TranscriptItem;
    let (root, path, session) = fixture();
    finish(&session, "ancestor", "answer");
    let ancestor = session.context_position().unwrap().head_entry;
    finish(&session, "abandoned", "old branch");
    select(&session, ancestor);
    for _ in 0..60 {
        finish(&session, "selected", "answer");
    }
    // A single record cannot consume the ordinary page budget, but it must
    // also not prevent moving backwards past it.
    finish(&session, "large", &"a".repeat(270 * 1024));
    let admission = session.admit_user_shell("shell".into(), true).unwrap();
    let first = session.transcript_page(None).unwrap().unwrap();
    let cursor = first.older.unwrap();
    assert!(first.projection.items.len() <= 96);
    let oversized = first
        .projection
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::Oversized { source } => Some(*source),
            _ => None,
        })
        .expect("large record has a page placeholder");
    let evidence = session
        .transcript_evidence(first.cursor, oversized)
        .unwrap()
        .unwrap();
    assert!(
        matches!(&evidence.items[..], [TranscriptItem::Assistant(message)]
        if matches!(&message.parts[..], [crate::TranscriptPart::Text(text)] if text.len() == 270 * 1024))
    );
    assert!(
        matches!(first.projection.items.last(), Some(TranscriptItem::UserShell(shell)) if shell.outcome.is_none())
    );
    session
        .record_user_shell(
            admission,
            crate::ToolOutput::success(serde_json::json!({"stdout":"settled"})),
        )
        .unwrap();
    let same_cut = session
        .transcript_page(Some(first.cursor))
        .unwrap()
        .unwrap();
    assert_eq!(same_cut.projection, first.projection);
    let shell = session
        .transcript_evidence(
            first.cursor,
            first.projection.items.last().unwrap().source().unwrap(),
        )
        .unwrap()
        .unwrap();
    assert!(
        matches!(&shell.items[..], [TranscriptItem::UserShell(shell)] if shell.outcome.is_none())
    );
    let latest = session.transcript_page(None).unwrap().unwrap();
    assert!(
        matches!(latest.projection.items.last(), Some(TranscriptItem::UserShell(shell)) if shell.outcome.is_some())
    );
    let older = session.transcript_page(Some(cursor)).unwrap().unwrap();
    assert!(older.older.is_none());
    let before = older.projection.items.last().unwrap().source().unwrap();
    let after = first.projection.items.first().unwrap().source().unwrap();
    assert!(before.entry < after.entry);
    assert!(older.projection.items.iter().all(|item| !matches!(item, TranscriptItem::User(message) if message.parts.iter().any(|part| matches!(part, crate::TranscriptPart::Text(text) if text == "abandoned")))));
    drop(session);
    let session = Session::open(&path).unwrap();
    assert_eq!(
        session
            .transcript_page(Some(first.cursor))
            .unwrap()
            .unwrap()
            .projection,
        first.projection
    );
    select(&session, ancestor);
    assert!(session.transcript_page(Some(cursor)).unwrap().is_none());
    assert!(
        session
            .transcript_evidence(first.cursor, oversized)
            .unwrap()
            .is_none()
    );
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn transcript_pages_attach_results_to_repeated_call_occurrences() {
    use crate::TranscriptItem;
    let (root, path, session) = fixture();
    let (turn, _) = session.begin_turn("tools".into(), model("one")).unwrap();
    for value in ["first result", "second result"] {
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::ToolCall(ToolCall {
                        id: "reused".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({}),
                        raw_arguments: None,
                    })],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        session
            .record_tool_result(
                turn,
                ToolResult {
                    call_id: "reused".into(),
                    name: "read".into(),
                    result: serde_json::json!({"value":value}),
                    images: Vec::new(),
                    is_error: false,
                },
            )
            .unwrap();
    }
    session
        .end_turn(turn, TurnEndReason::Completed, &BTreeSet::new())
        .unwrap();
    drop(session);
    let session = Session::open(&path).unwrap();
    let page = session.transcript_page(None).unwrap().unwrap();
    let results = page
        .projection
        .items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::ActivityGroup(group) => {
                Some(&group.activities[0].result.as_ref().unwrap().value["value"])
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results, ["first result", "second result"]);
    assert_eq!(
        page.tool_starts.values().copied().collect::<Vec<_>>(),
        [1, 2]
    );
    let mut listed = Vec::new();
    session
        .visit_committed_tools(|number, activity| {
            listed.push((
                number.get(),
                activity.result.as_ref().unwrap().value["value"].clone(),
            ));
        })
        .unwrap();
    assert_eq!(
        listed,
        [
            (2, serde_json::json!("second result")),
            (1, serde_json::json!("first result"))
        ]
    );
    let second = session
        .tool_evidence(None, ToolEvidenceTarget::Latest)
        .unwrap()
        .unwrap();
    assert_eq!((second.number.get(), second.total), (2, 2));
    assert_eq!(
        second.activity.result.unwrap().value["value"],
        "second result"
    );
    let first = session
        .tool_evidence(
            Some(second.cursor),
            ToolEvidenceTarget::Number(std::num::NonZeroUsize::MIN),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        first.activity.result.as_ref().unwrap().value["value"],
        "first result"
    );
    assert_ne!(first.activity.identity, second.activity.identity);
    finish(&session, "unrelated input", "unrelated prose");
    let unrelated = session.context_position().unwrap().head_entry - 1;
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE entries SET body=?1 WHERE seq=?2",
            params![
                br#"{"kind":"unreadable_fixture","data":{}}"#.as_slice(),
                i64::try_from(unrelated).unwrap(),
            ],
        )
        .unwrap();
    assert!(
        session.presentation(None).is_err(),
        "fixture must fail a full display read"
    );
    let mut reread = Vec::new();
    session
        .visit_committed_tools(|number, activity| {
            reread.push((
                number.get(),
                activity.result.as_ref().unwrap().value["value"].clone(),
            ));
        })
        .unwrap();
    assert_eq!(
        reread, listed,
        "tool listing must not decode unrelated prose"
    );
    drop(connection);
    select(&session, 0);
    session
        .visit_committed_tools(|_, _| panic!("inactive branch tool entered current listing"))
        .unwrap();
    assert!(
        session
            .tool_evidence(Some(second.cursor), ToolEvidenceTarget::Latest)
            .unwrap()
            .is_none()
    );
    assert!(
        session
            .tool_evidence(None, ToolEvidenceTarget::Latest)
            .unwrap()
            .is_none()
    );
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn focused_tool_read_does_not_decode_sibling_outcomes() {
    let (root, path, session) = fixture();
    let (turn, _) = session.begin_turn("tools".into(), model("one")).unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: ["first", "second"]
                    .into_iter()
                    .map(|id| {
                        Content::ToolCall(ToolCall {
                            id: id.into(),
                            name: "read".into(),
                            arguments: serde_json::json!({}),
                            raw_arguments: None,
                        })
                    })
                    .collect(),
                provider_replay: None,
            },
            Usage::unknown(),
            true,
        )
        .unwrap();
    let pending = session
        .tool_evidence(None, ToolEvidenceTarget::Latest)
        .unwrap()
        .unwrap();
    assert!(pending.activity.result.is_none());
    for id in ["first", "second"] {
        session
            .record_tool_result(
                turn,
                ToolResult {
                    call_id: id.into(),
                    name: "read".into(),
                    result: serde_json::json!({"value":id}),
                    images: Vec::new(),
                    is_error: false,
                },
            )
            .unwrap();
    }
    let pinned = session
        .tool_evidence(Some(pending.cursor), ToolEvidenceTarget::Latest)
        .unwrap()
        .unwrap();
    assert!(
        pinned.activity.result.is_none(),
        "cursor must not gain later observations"
    );
    let first = session
        .tool_evidence(
            None,
            ToolEvidenceTarget::Number(std::num::NonZeroUsize::MIN),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        first.activity.result.as_ref().unwrap().value["value"],
        "first"
    );
    let sibling = session.entry_count().unwrap();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute(
            "UPDATE entries SET body=?1 WHERE seq=?2",
            params![
                br#"{"kind":"unreadable_fixture","data":{}}"#.as_slice(),
                i64::try_from(sibling).unwrap()
            ],
        )
        .unwrap();
    let crate::TranscriptActivityIdentity::Tool(first_occurrence) = first.activity.identity else {
        panic!("top-level tool")
    };
    let focused = session
        .tool_evidence(None, ToolEvidenceTarget::Occurrence(first_occurrence))
        .unwrap()
        .unwrap();
    assert_eq!(focused.activity, first.activity);
    assert!(
        session
            .tool_evidence(None, ToolEvidenceTarget::Latest)
            .is_err()
    );
    drop(connection);
    drop(session);
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
    assert!(session.view().unwrap().messages.is_empty());
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
    assert_eq!(session.view().unwrap().messages.len(), 2);
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
    session.store().lock().unwrap().connection.execute_batch("CREATE TRIGGER reject_navigation BEFORE INSERT ON entries BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
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
        .store()
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
    session.store().lock().unwrap().connection.execute_batch("CREATE TRIGGER reject_navigation BEFORE INSERT ON entries WHEN json_extract(NEW.body,'$.kind') = 'context_selected' BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
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
        .store()
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
    assert!(session.view().unwrap().messages.is_empty());
    assert_eq!(
        session.view().unwrap().delegations()[0].outcome,
        Some(crate::DelegationOutcome::Unknown)
    );
    select(&session, source_head);
    assert_eq!(session.view().unwrap().messages.len(), 3);
    let path = session.file_path().unwrap().to_path_buf();
    drop(session);
    let reopened = Session::open(path).unwrap();
    assert_eq!(reopened.view().unwrap().messages.len(), 3);
    drop(reopened);
    drop(lease);
    fs::remove_dir_all(root).unwrap();
}
