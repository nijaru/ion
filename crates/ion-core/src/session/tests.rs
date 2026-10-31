use super::*;
use ion_ai::{ProviderReplay, ToolCall};

fn test_execution() -> ModelExecution {
    ModelExecution {
        route: ion_ai::ModelRoute::direct(
            ModelRef {
                provider: "test".into(),
                model: "test".into(),
            },
            ion_ai::ModelRouteReason::Auxiliary,
        ),
        returned_model: None,
    }
}

#[tokio::test]
async fn user_shell_operation_rejects_overlap_and_commits_before_release() {
    let (root, path) = fixture();
    let session = Arc::new(Session::create(&path, Some(&root)).unwrap());
    let (admitted, admission) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let operation = session.run_user_shell(
        "echo done".into(),
        false,
        CancellationToken::new(),
        move |command, _| async move {
            assert_eq!(command, "echo done");
            admitted.send(()).unwrap();
            released.await.unwrap();
            crate::ToolOutput {
                value: serde_json::json!({"stdout":"done","exit_code":0}),
                images: Vec::new(),
                is_error: false,
            }
        },
    );
    admission.await.unwrap();
    let rejected = session.run_user_shell(
        "must not run".into(),
        false,
        CancellationToken::new(),
        |_, _| async { panic!("occupied owner dispatched a shell") },
    );
    assert!(matches!(rejected.await, Err(SessionError::OperationActive)));
    assert_eq!(session.view().unwrap().entries.len(), 1);
    assert!(session.operations.try_lock().is_err());
    assert!(matches!(
        session.select_model(test_execution().route.logical),
        Err(SessionError::OperationActive)
    ));
    assert!(
        matches!(session.view().unwrap().entries.last(), Some(SessionEntry::UserShellAdmitted { command, exclude_from_context: false }) if command == "echo done")
    );
    release.send(()).unwrap();
    assert_eq!(operation.await.unwrap().value["stdout"], "done");
    assert!(session.operations.try_lock().is_ok());
    assert!(matches!(
        session.view().unwrap().entries.last(),
        Some(SessionEntry::UserShellSettled {
            outcome: UserShellOutcome::Observed { .. },
            ..
        })
    ));
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn shell_admission_recovers_before_authority_and_rejects_failed_recovery() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let (turn, _) = session
        .begin_turn("interrupted".into(), test_execution().route.logical)
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "unsettled".into(),
                    name: "write".into(),
                    arguments: serde_json::json!({"path":"must-not-replay","content":"old"}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    drop(session);
    let session = Session::open(&path).unwrap();
    let before = session.view().unwrap();
    assert_eq!(before.unfinished_turn, Some(turn));
    session.store.lock().unwrap().connection.execute_batch(
        "CREATE TRIGGER reject_recovery BEFORE INSERT ON entries BEGIN SELECT RAISE(ABORT, 'recovery unavailable'); END;"
    ).unwrap();
    assert!(matches!(
        session.admit_user_shell("printf new".into(), false),
        Err(SessionError::Sqlite(_))
    ));
    assert_eq!(session.view().unwrap().entries, before.entries);
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute_batch("DROP TRIGGER reject_recovery;")
        .unwrap();

    let admission = session
        .admit_user_shell("printf new".into(), false)
        .unwrap();
    let recovered = session.view().unwrap();
    assert_eq!(
        recovered.unfinished_turn, None,
        "recovery must precede permission to run an effect"
    );
    assert!(matches!(
        recovered.entries.get(recovered.entries.len() - 2),
        Some(SessionEntry::TurnEnded {
            reason: TurnEndReason::Interrupted,
            ..
        })
    ));
    assert!(
        matches!(&recovered.entries[recovered.entries.len() - 3], SessionEntry::ToolResult { result, .. } if result.call_id == "unsettled" && matches!(result.outcome, crate::ToolOutcome::Unknown))
    );
    let projection = crate::TranscriptProjection::from_session(&recovered);
    let activity = projection
        .items
        .iter()
        .find_map(|item| match item {
            crate::TranscriptItem::ActivityGroup(group) => group.activities.first(),
            _ => None,
        })
        .unwrap();
    assert_eq!(activity.state, crate::ActivityState::Unknown);
    let encoded =
        serde_json::to_value(&recovered.entries[recovered.entries.len() - 3]).unwrap();
    assert_eq!(encoded["data"]["result"]["outcome"]["state"], "unknown");
    assert!(encoded["data"]["result"]["outcome"].get("output").is_none());
    session
        .record_user_shell(
            admission,
            serde_json::json!({"stdout":"new","exit_code":0}),
            false,
        )
        .unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(
        matches!(reopened.view().unwrap().entries.last(), Some(SessionEntry::UserShellSettled { outcome: UserShellOutcome::Observed { output, .. }, .. }) if output["stdout"] == "new")
    );
    for copy in [
        reopened.clone_to(root.join("clone.sqlite")).unwrap(),
        reopened
            .fork_to(root.join("fork.sqlite"), ForkPoint::AfterTurn(turn))
            .unwrap(),
    ] {
        let projected = crate::TranscriptProjection::from_session(&copy.view().unwrap());
        let activity = projected
            .items
            .iter()
            .find_map(|item| match item {
                crate::TranscriptItem::ActivityGroup(group) => group.activities.first(),
                _ => None,
            })
            .unwrap();
        assert_eq!(activity.state, crate::ActivityState::Unknown);
    }
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn shell_admission_and_result_faults_leave_only_committed_authority() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    session.store.lock().unwrap().connection.execute_batch(
        "CREATE TRIGGER reject_shell BEFORE INSERT ON entries BEGIN SELECT RAISE(ABORT, 'storage unavailable'); END;"
    ).unwrap();
    assert!(matches!(
        session.admit_user_shell("touch effect".into(), false),
        Err(SessionError::Sqlite(_))
    ));
    assert!(session.view().unwrap().entries.is_empty());
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute_batch("DROP TRIGGER reject_shell;")
        .unwrap();
    let admission = session
        .admit_user_shell("touch effect".into(), false)
        .unwrap();
    assert!(matches!(
        session.view().unwrap().entries.last(),
        Some(SessionEntry::UserShellAdmitted { command, .. }) if command == "touch effect"
    ));
    assert_eq!(
        session.view().unwrap().unfinished_user_shell,
        Some(UnobservedUserShell { admission_entry: 1 })
    );
    // Simulate the started host's effect, then fail only result publication.
    fs::write(root.join("effect"), "observed").unwrap();
    session.store.lock().unwrap().connection.execute_batch(
        "CREATE TRIGGER reject_result BEFORE INSERT ON entries BEGIN SELECT RAISE(ABORT, 'result unavailable'); END;"
    ).unwrap();
    assert!(matches!(
        session.record_user_shell(
            admission,
            serde_json::json!({"stdout":"observed", "exit_code":0}),
            false
        ),
        Err(SessionError::Sqlite(_))
    ));
    let before = session.view().unwrap();
    assert_eq!(before.entries.len(), 1);
    assert_eq!(before.user_shell_outcome(1), &UserShellOutcome::Unknown);
    assert!(
        serde_json::to_string(&before.display_messages())
            .unwrap()
            .contains("external effect unknown")
    );
    assert!(
        !serde_json::to_string(&before.display_messages())
            .unwrap()
            .contains("exit code")
    );
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.view().unwrap().entries, before.entries);
    assert_eq!(Session::inspect(&path).unwrap().entries, before.entries);
    assert_eq!(reopened.context_messages().unwrap(), before.messages);
    assert_eq!(fs::read_to_string(root.join("effect")).unwrap(), "observed");
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupted_shell_recovery_is_atomic_private_and_never_replayed() {
    for excluded in [false, true] {
        let (root, path) = fixture();
        let session = Session::create(&path, Some(&root)).unwrap();
        session
            .admit_user_shell("interrupted-command".into(), excluded)
            .unwrap();
        drop(session);
        let session = Session::open(&path).unwrap();
        let before = session.view().unwrap();
        assert_eq!(
            before.unfinished_user_shell,
            Some(UnobservedUserShell { admission_entry: 1 })
        );
        assert_eq!(before.unfinished_turn, None);
        assert_eq!(before.display_messages().len(), 1);
        assert_eq!(before.messages.len(), usize::from(!excluded));
        let transcript = crate::TranscriptProjection::from_session(&before);
        assert_eq!(transcript.sealed_prefix(), 0);
        assert_eq!(
            transcript.items[0].source(),
            Some(crate::TranscriptSource { entry: 1, part: 0 })
        );
        assert!(
            matches!(&transcript.items[..], [crate::TranscriptItem::UserShell(shell)]
            if shell.command == "interrupted-command" && shell.outcome.is_none() && shell.exclude_from_context == excluded)
        );
        let clone = session.clone_to(root.join("clone.sqlite")).unwrap();
        assert_eq!(clone.view().unwrap().entries, before.entries);
        assert_eq!(clone.context_messages().unwrap(), before.messages);
        drop(clone);
        // Fail the NEW admission after recovery would have been inserted:
        // both shell and coding admissions must roll back recovery too.
        session.store.lock().unwrap().connection.execute_batch(
            "CREATE TRIGGER reject_admission BEFORE INSERT ON entries WHEN json_extract(CAST(NEW.body AS TEXT), '$.kind') IN ('turn_started', 'user_shell_admitted') BEGIN SELECT RAISE(ABORT, 'admission unavailable'); END;"
        ).unwrap();
        assert!(
            session
                .admit_user_shell("new-command".into(), false)
                .is_err()
        );
        assert!(
            session
                .begin_turn("new input".into(), test_execution().route.logical)
                .is_err()
        );
        assert_eq!(session.view().unwrap().entries, before.entries);
        session
            .store
            .lock()
            .unwrap()
            .connection
            .execute_batch("DROP TRIGGER reject_admission;")
            .unwrap();
        // Exercise both explicit recovery entry points without synthetic Turns.
        if excluded {
            let admission = session
                .admit_user_shell("new-command".into(), false)
                .unwrap();
            session
                .record_user_shell(admission, serde_json::json!({"stdout":"new"}), false)
                .unwrap();
        } else {
            let (turn, _) = session
                .begin_turn("new input".into(), test_execution().route.logical)
                .unwrap();
            session
                .end_turn(turn, TurnEndReason::Cancelled, &BTreeSet::new())
                .unwrap();
        }
        let recovered = session.view().unwrap();
        assert_eq!(recovered.unfinished_user_shell, None);
        let transcript = crate::TranscriptProjection::from_session(&recovered);
        assert!(transcript.items[0].is_sealed());
        assert!(
            matches!(&transcript.items[0], crate::TranscriptItem::UserShell(shell) if shell.outcome == Some(UserShellOutcome::Unknown))
        );
        assert_eq!(
            transcript.items[0].source(),
            Some(crate::TranscriptSource { entry: 1, part: 0 })
        );
        assert!(matches!(
            &recovered.entries[1],
            SessionEntry::UserShellSettled {
                admission_entry: 1,
                outcome: UserShellOutcome::Unknown
            }
        ));
        assert_eq!(recovered.entries.iter().filter(|entry| matches!(entry, SessionEntry::UserShellAdmitted { command, .. } if command == "interrupted-command")).count(), 1);
        let context = serde_json::to_string(&session.context_messages().unwrap()).unwrap();
        assert_eq!(context.contains("interrupted-command"), !excluded);
        assert_eq!(context.contains("external effect unknown"), !excluded);
        // Settled prefix cuts cannot bisect shell intent and outcome.
        let plan = session
            .compaction_plan(None, 0, usize::MAX, true, u64::MAX)
            .unwrap()
            .unwrap();
        assert!(plan.through_entry >= 2);
        assert_eq!(
            serde_json::to_string(&plan.messages)
                .unwrap()
                .contains("interrupted-command"),
            !excluded
        );
        assert!(
            session
                .record_compaction(1, "invalid cut".into(), test_execution(), Usage::unknown())
                .is_err()
        );
        session
            .record_compaction(
                plan.through_entry,
                "settled history".into(),
                test_execution(),
                Usage::unknown(),
            )
            .unwrap();
        if !excluded {
            let fork = session
                .fork_to(root.join("fork.sqlite"), ForkPoint::BeforeTurn(1))
                .unwrap();
            assert_eq!(fork.view().unwrap().entries[..2], recovered.entries[..2]);
            assert!(
                serde_json::to_string(&fork.context_messages().unwrap())
                    .unwrap()
                    .contains("external effect unknown")
            );
            drop(fork);
        }
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(
            reopened.view().unwrap().user_shell_outcome(1),
            &UserShellOutcome::Unknown
        );
        assert!(
            !serde_json::to_string(&reopened.context_messages().unwrap())
                .unwrap()
                .contains("interrupted-command")
        );
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn passive_metadata_is_bounded_validated_and_does_not_recover_history() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    session.set_name(Some("Metadata")).unwrap();
    session
        .admit_user_shell(format!("{}\nSECOND_LINE", "🦀".repeat(130)), true)
        .unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    session
        .begin_turn("unfinished prompt".into(), model.clone())
        .unwrap();
    let before = session.view().unwrap().entries;
    let metadata = Session::inspect_metadata(&path).unwrap();
    assert_eq!(metadata.cwd, Some(root.canonicalize().unwrap()));
    assert_eq!(metadata.name.as_deref(), Some("Metadata"));
    assert_eq!(metadata.last_model, Some(model));
    assert_eq!(metadata.turns, 1);
    assert_eq!(
        metadata.first_input,
        Some(SessionInputPreview::Shell {
            command: "🦀".repeat(120),
            excluded: true,
        })
    );
    assert_eq!(session.view().unwrap().entries, before);
    drop(session);
    let connection = Connection::open(&path).unwrap();
    // Inject corruption below normal JSON-index validation, not a valid append.
    connection
        .execute_batch(
            "DROP INDEX request_keys; DROP INDEX turn_ends; DROP INDEX delegation_runs; DROP INDEX delegation_created_children; DROP INDEX child_bindings;",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO entries(body) VALUES (?1)",
            params![b"invalid JSON".as_slice()],
        )
        .unwrap();
    let damaged: Vec<(i64, Vec<u8>)> = connection
        .prepare("SELECT seq,body FROM entries ORDER BY seq")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for result in [
        Session::open(&path).map(|_| ()),
        Session::inspect_metadata(&path).map(|_| ()),
        Session::inspect(&path).map(|_| ()),
    ] {
        assert!(matches!(result, Err(SessionError::Json(_))), "{result:?}");
    }
    let after: Vec<(i64, Vec<u8>)> = connection
        .prepare("SELECT seq,body FROM entries ORDER BY seq")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        after, damaged,
        "failed validation rewrote committed history"
    );
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn prompt_metadata_uses_only_the_first_line_bounded_by_unicode_characters() {
    for (prompt, expected) in [
        ("短🦀é\nSECOND_LINE".into(), "短🦀é".into()),
        (
            format!("{}\nSECOND_LINE", "🦀é界".repeat(40)),
            "🦀é界".repeat(40),
        ),
        (
            format!("{}Z\nSECOND_LINE", "🦀é界".repeat(40)),
            "🦀é界".repeat(40),
        ),
    ] {
        let (root, path) = fixture();
        let session = Session::create(&path, Some(&root)).unwrap();
        let model = test_execution().route.logical;
        let (turn, _) = session.begin_turn(prompt, model.clone()).unwrap();
        session
            .end_turn(turn, TurnEndReason::Cancelled, &BTreeSet::new())
            .unwrap();
        session.begin_turn("latest prompt".into(), model).unwrap();
        let before = session.view().unwrap().entries;
        let metadata = Session::inspect_metadata(&path).unwrap();
        assert_eq!(
            metadata.first_input,
            Some(SessionInputPreview::Prompt {
                text: Some(expected)
            })
        );
        assert_eq!(session.view().unwrap().entries, before);
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(Session::inspect_metadata(&path).unwrap(), metadata);
        assert_eq!(reopened.view().unwrap().entries, before);
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn metadata_projection_retains_only_pending_shared_shell_slot() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let admission = session.admit_user_shell("shared".into(), false).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "selected".into(),
    };
    let mut entries = session.view().unwrap().entries;
    // Preferences may intervene without releasing the slot that settlement
    // must replace. Metadata must neither reject this prefix nor recover it.
    entries.push(SessionEntry::ModelSelected {
        model: model.clone(),
    });
    let pending = project(entries.iter().map(Ok), ProjectionMode::Metadata, &|entry| {
        Ok(entries[(entry - 1) as usize].clone())
    })
    .unwrap();
    assert_eq!(pending.messages.len(), 1);
    assert_eq!(pending.state.last_model, Some(model));
    assert!(pending.state.pending_shell.is_some());
    session
        .record_user_shell(
            admission,
            serde_json::json!({"stdout": "x".repeat(65536)}),
            false,
        )
        .unwrap();
    let settlement = session.view().unwrap().entries.pop().unwrap();
    entries.push(settlement);
    let settled = project(entries.iter().map(Ok), ProjectionMode::Metadata, &|entry| {
        Ok(entries[(entry - 1) as usize].clone())
    })
    .unwrap();
    assert!(settled.messages.is_empty());
    assert!(settled.state.pending_shell.is_none());
    assert_eq!(
        settled.first_input,
        Session::inspect_metadata(&path).unwrap().first_input
    );
    // Discarding messages must not admit a second settlement.
    entries.push(entries.last().unwrap().clone());
    for mode in [ProjectionMode::Full, ProjectionMode::Metadata] {
        assert!(matches!(
            project(entries.iter().map(Ok), mode, &|entry| Ok(entries
                [(entry - 1) as usize]
                .clone())),
            Err(SessionError::InvalidHistory)
        ));
    }
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn opening_invalid_database_does_not_initialize_session_schema() {
    let (root, path) = fixture();
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("CREATE TABLE unrelated (value INTEGER);")
        .unwrap();
    drop(connection);

    assert!(Session::open(&path).is_err());

    let connection =
        Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let table_exists = |name: &str| {
        connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
                params![name],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_some()
    };
    assert!(table_exists("unrelated"));
    assert!(!table_exists("session"));
    assert!(!table_exists("entries"));
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reopen_rejects_old_format_or_missing_affinity_without_backfill() {
    let (root, path) = fixture();
    drop(Session::create(&path, Some(&root)).unwrap());
    let connection = Connection::open(&path).unwrap();
    for version in [6, 17, FORMAT_VERSION] {
        let encoded = serde_json::to_vec(&serde_json::json!({
            "version": version,
            "cwd": root,
            "name": null,
        }))
        .unwrap();
        connection
            .execute(
                "UPDATE session SET header = ?1 WHERE id=1",
                params![encoded],
            )
            .unwrap();
        for result in [
            Session::open(&path).map(|_| ()),
            Session::inspect(&path).map(|_| ()),
            Session::inspect_metadata(&path).map(|_| ()),
        ] {
            if version != FORMAT_VERSION {
                assert!(
                    matches!(result, Err(SessionError::UnsupportedFormat(found)) if found == version)
                );
            } else {
                assert!(result.is_err());
            }
        }
        let stored: Vec<u8> = connection
            .query_row("SELECT header FROM session WHERE id=1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(stored, encoded);
    }
    drop(connection);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn shared_shell_context_preserves_host_metadata_after_reopen() {
    let (root, path) = fixture();
    let output = serde_json::json!({
        "stdout": "\u{fffd}", "stdout_lossy": true, "stdout_complete": true,
        "stdout_truncated": false, "stdout_omitted_bytes": 0,
        "stdout_full_path": "/capture/raw", "stdout_full_error": null,
        "stderr_complete": false, "stderr_omitted_bytes": null,
        "stderr_full_error": "read failed", "exit_code": 0,
        "additional_host_data": {"nested": [true, "preserve exactly"]}
    });
    let expected = serde_json::to_value(UserShellOutcome::Observed {
        output: output.clone(),
        is_error: false,
    })
    .unwrap();
    let check = |session: &Session| {
        let messages = session.context_messages().unwrap();
        let Content::Text(text) = &messages[0].content[0] else {
            panic!("shell context must be text");
        };
        let payload = text.rsplit_once('\n').unwrap().1;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(payload).unwrap(),
            expected
        );
    };
    let session = Session::create(&path, Some(&root)).unwrap();
    let admission = session
        .admit_user_shell("binary output".into(), false)
        .unwrap();
    session.record_user_shell(admission, output, false).unwrap();
    check(&session);
    drop(session);
    let reopened = Session::open(&path).unwrap();
    check(&reopened);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn user_shell_context_choice_survives_reopen_and_compaction() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let admission = session.admit_user_shell("pwd".into(), false).unwrap();
    session
        .record_user_shell(admission, serde_json::json!({"stdout":"visible"}), false)
        .unwrap();
    let admission = session.admit_user_shell("secret".into(), true).unwrap();
    session
        .record_user_shell(admission, serde_json::json!({"stdout":"private"}), false)
        .unwrap();
    let view = session.view().unwrap();
    assert_eq!(view.display_messages().len(), 2);
    assert_eq!(view.messages.len(), 1);
    assert!(
        serde_json::to_string(&session.context_messages().unwrap())
            .unwrap()
            .contains("visible")
    );
    assert!(
        !serde_json::to_string(&session.context_messages().unwrap())
            .unwrap()
            .contains("private")
    );
    assert!(matches!(
        session.admit_user_shell("\0".into(), false),
        Err(SessionError::InvalidHistory)
    ));
    assert_eq!(session.view().unwrap().entries.len(), 4);
    drop(session);
    let reopened = Session::open(&path).unwrap();
    let view = reopened.view().unwrap();
    assert_eq!(view.display_messages().len(), 2);
    assert_eq!(view.messages.len(), 1);
    assert!(
        !serde_json::to_string(&reopened.context_messages().unwrap())
            .unwrap()
            .contains("private")
    );
    reopened
        .record_compaction(
            reopened.entry_count().unwrap(),
            "visible command was run".into(),
            test_execution(),
            Usage::unknown(),
        )
        .unwrap();
    assert!(
        !serde_json::to_string(&reopened.context_messages().unwrap())
            .unwrap()
            .contains("private")
    );
    // A shell result after a compacted prefix still resolves by absolute
    // admission sequence, not its position in the retained suffix.
    let admission = reopened
        .admit_user_shell("after-summary".into(), false)
        .unwrap();
    reopened
        .record_user_shell(
            admission,
            serde_json::json!({"stdout":"suffix-observation"}),
            false,
        )
        .unwrap();
    let context = serde_json::to_string(&reopened.context_messages().unwrap()).unwrap();
    assert!(context.contains("suffix-observation"));
    assert!(!context.contains("external effect unknown"));
    assert!(!context.contains("private"));
    let plan = reopened
        .compaction_plan(None, 0, usize::MAX, true, u64::MAX)
        .unwrap()
        .unwrap();
    assert!(
        serde_json::to_string(&plan.messages)
            .unwrap()
            .contains("suffix-observation")
    );
    drop(reopened);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(
        serde_json::to_string(&reopened.context_messages().unwrap()).unwrap(),
        context
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn selected_point_forks_preserve_valid_prefix_and_source_history() {
    let (root, path) = fixture();
    let source = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "model".into(),
    };
    for prompt in ["first", "second"] {
        let (turn, _) = source.begin_turn(prompt.into(), model.clone()).unwrap();
        source
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text(format!("answer {turn}"))],
                    provider_replay: None,
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
        if turn == 1 {
            source
                .record_compaction(
                    source.entry_count().unwrap(),
                    "summary".into(),
                    test_execution(),
                    Usage::unknown(),
                )
                .unwrap();
        }
    }
    let (active, _) = source.begin_turn("third".into(), model.clone()).unwrap();
    assert!(matches!(
        source.fork_to(root.join("invalid.sqlite"), ForkPoint::AfterTurn(active)),
        Err(SessionError::InvalidForkPoint)
    ));
    let before = source
        .fork_to(root.join("before.sqlite"), ForkPoint::BeforeTurn(2))
        .unwrap();
    let after = source
        .fork_to(root.join("after.sqlite"), ForkPoint::AfterTurn(2))
        .unwrap();
    assert_eq!(source.view().unwrap().unfinished_turn, Some(3));
    assert_eq!(before.view().unwrap().last_model, Some(model.clone()));
    assert_eq!(before.context_messages().unwrap().len(), 1); // summary only
    assert_eq!(
        after.view().unwrap().last_end.as_ref().map(|item| item.0),
        Some(2)
    );
    assert_eq!(after.context_messages().unwrap().len(), 3); // summary, second input/answer
    assert!(
        !after
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry, SessionEntry::TurnStarted { turn: 3, .. }))
    );
    let source_id = source.provider_session_id();
    let before_id = before.provider_session_id();
    let after_id = after.provider_session_id();
    assert_ne!(source_id, before_id);
    assert_ne!(source_id, after_id);
    assert_ne!(before_id, after_id);
    drop((source, before, after));
    assert_eq!(
        Session::open(&path).unwrap().provider_session_id(),
        source_id
    );
    assert_eq!(
        Session::open(root.join("before.sqlite"))
            .unwrap()
            .provider_session_id(),
        before_id
    );
    assert_eq!(
        Session::open(root.join("after.sqlite"))
            .unwrap()
            .provider_session_id(),
        after_id
    );
    fs::remove_dir_all(root).unwrap();
}

fn fixture() -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("ion-session-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    (root.clone(), root.join("session.sqlite"))
}

#[test]
fn raw_input_limit_can_be_persisted_after_json_encoding() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let mut prompt = "line\n".repeat(8 * 1024 * 1024 / 5);
    prompt.push_str("abc");
    assert_eq!(prompt.len(), 8 * 1024 * 1024);
    session
        .begin_turn(
            prompt.clone(),
            ModelRef {
                provider: "test".into(),
                model: "test".into(),
            },
        )
        .unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(matches!(
        &reopened.view().unwrap().entries[0],
        SessionEntry::TurnStarted { input, .. }
            if input.content == vec![Content::Text(prompt)]
    ));
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn image_input_survives_reopen_and_malformed_image_cannot_be_constructed() {
    let image: ion_ai::ImageContent = serde_json::from_value(serde_json::json!({
        "mime_type":"image/png",
        "data":"iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
    }))
    .unwrap();
    for text in [Some("inspect"), None] {
        let (root, path) = fixture();
        let session = Session::create(&path, Some(&root)).unwrap();
        let mut content = Vec::new();
        if let Some(text) = text {
            content.push(Content::Text(text.into()));
        }
        content.push(Content::Image(image.clone()));
        let input = Message {
            role: Role::User,
            content,
            provider_replay: None,
        };
        let model = ModelRef {
            provider: "test".into(),
            model: "vision".into(),
        };
        let (turn, _) = session
            .begin_turn_message(input.clone(), model.clone())
            .unwrap();
        session
            .end_turn(turn, TurnEndReason::Cancelled, &BTreeSet::new())
            .unwrap();
        session.begin_turn("latest prompt".into(), model).unwrap();
        let facts = session.view().unwrap().entries;
        let metadata = Session::inspect_metadata(&path).unwrap();
        assert_eq!(
            metadata.first_input,
            Some(SessionInputPreview::Prompt {
                text: text.map(str::to_owned)
            })
        );
        assert_eq!(session.view().unwrap().entries, facts);
        let before = session.entry_count().unwrap();
        assert!(
            serde_json::from_value::<ion_ai::ImageContent>(serde_json::json!({
                "mime_type":"image/png", "data":"broken"
            }))
            .is_err()
        );
        assert_eq!(session.entry_count().unwrap(), before);
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.context_messages().unwrap()[0], input);
        assert_eq!(Session::inspect_metadata(&path).unwrap(), metadata);
        assert_eq!(reopened.view().unwrap().entries, facts);
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn interrupted_call_closes_atomically_with_next_input() {
    let (root, path) = fixture();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let session = Session::create(&path, Some(&root)).unwrap();
    let (turn, _) = session.begin_turn("first".into(), model.clone()).unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "call".into(),
                    name: "write".into(),
                    arguments: serde_json::json!({}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    drop(session);
    let before = Session::inspect(&path).unwrap();
    assert_eq!(before.unfinished_turn, Some(turn));
    assert_eq!(before.entries.len(), 2);
    let reopened = Session::open(&path).unwrap();
    let (next, interrupted) = reopened.begin_turn("second".into(), model).unwrap();
    assert_eq!((next, interrupted), (turn + 1, 1));
    let entries = reopened.view().unwrap().entries;
    assert!(matches!(entries[2], SessionEntry::ToolResult { .. }));
    assert!(matches!(
        entries[3],
        SessionEntry::TurnEnded {
            reason: TurnEndReason::Interrupted,
            ..
        }
    ));
    assert!(matches!(entries[4], SessionEntry::TurnStarted { .. }));
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn session_aliases_share_writer_identity_and_copies_normalize_paths() {
    use std::os::unix::fs::symlink;

    let (root, path) = fixture();
    let directory_alias = root.join("alias");
    symlink(&root, &directory_alias).unwrap();
    let file_alias = root.join("alias.sqlite");
    let session = Session::create(&path, Some(&root)).unwrap();
    symlink(&path, &file_alias).unwrap();
    for alias in [&file_alias, &directory_alias.join("session.sqlite")] {
        assert!(matches!(
            Session::open(alias),
            Err(SessionError::AlreadyOpen)
        ));
    }
    drop(session);
    let session = Session::open(&file_alias).unwrap();
    assert_eq!(session.path(), path.canonicalize().unwrap());
    assert!(matches!(
        Session::open(&path),
        Err(SessionError::AlreadyOpen)
    ));
    let copied = session
        .clone_to(directory_alias.join("copy.sqlite"))
        .unwrap();
    assert_eq!(
        copied.path(),
        root.join("copy.sqlite").canonicalize().unwrap()
    );
    assert!(matches!(
        Session::open(root.join("copy.sqlite")),
        Err(SessionError::AlreadyOpen)
    ));
    drop(copied);
    drop(session);
    drop(Session::open(&path).unwrap());
    fs::remove_file(directory_alias).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hard_linked_databases_are_rejected_before_sqlite_access() {
    let (root, path) = fixture();
    drop(Session::create(&path, Some(&root)).unwrap());
    let alias = root.join("hard.sqlite");
    fs::hard_link(&path, &alias).unwrap();
    for name in [&path, &alias] {
        assert!(matches!(
            Session::open(name),
            Err(SessionError::HardLinkedDatabase)
        ));
        assert!(matches!(
            Session::inspect(name),
            Err(SessionError::HardLinkedDatabase)
        ));
    }
    fs::remove_file(alias).unwrap();
    drop(Session::open(&path).unwrap());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn distinct_database_extensions_do_not_share_a_writer_lock() {
    let (root, path) = fixture();
    let first = Session::create(&path, Some(&root)).unwrap();
    let second = Session::create(path.with_extension("db"), Some(&root)).unwrap();
    drop(second);
    drop(first);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_copy_retains_lock_identity_for_waiting_writers() {
    let (root, path) = fixture();
    let source = Session::create(&path, Some(&root)).unwrap();
    let destination = root.join("copy.sqlite");
    // A contender can open the lock inode before actually requesting its lease.
    let waiting = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("copy.sqlite.lock"))
        .unwrap();
    assert!(
        source
            .copy_entries_to(
                &destination,
                vec![SessionEntry::TurnEnded {
                    turn: 1,
                    reason: TurnEndReason::Completed,
                }]
            )
            .is_err()
    );
    assert!(!destination.exists());
    let writer = Session::create(&destination, Some(&root)).unwrap();
    assert_eq!(
        flock(&waiting, FlockOperation::NonBlockingLockExclusive),
        Err(rustix::io::Errno::WOULDBLOCK)
    );
    drop(writer);
    flock(&waiting, FlockOperation::NonBlockingLockExclusive).unwrap();
    flock(&waiting, FlockOperation::Unlock).unwrap();
    drop(waiting);
    drop(source);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn releasing_a_session_unlocks_an_inherited_file_description() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    assert!(matches!(
        Session::open(&path),
        Err(SessionError::AlreadyOpen)
    ));
    let inherited = session._lock.0.try_clone().unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    drop(reopened);
    drop(inherited);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn interrupted_results_preserve_assistant_call_order() {
    let (root, path) = fixture();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let session = Session::create(&path, Some(&root)).unwrap();
    let (turn, _) = session.begin_turn("first".into(), model.clone()).unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: ["z-call", "a-call"]
                    .into_iter()
                    .map(|id| {
                        Content::ToolCall(ToolCall {
                            id: id.into(),
                            name: "read".into(),
                            arguments: serde_json::json!({"path":"missing"}),
                            raw_arguments: None,
                        })
                    })
                    .collect(),
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.begin_turn("second".into(), model).unwrap().1, 2);
    let ids = reopened
        .view()
        .unwrap()
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::ToolResult { result, .. } => Some(result.call_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, ["z-call", "a-call"]);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn final_answer_and_end_share_a_commit() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (turn, _) = session.begin_turn("hello".into(), model).unwrap();
    assert!(
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("done".into())],
                    provider_replay: None
                },
                Usage::known(123, 45),
                false,
            )
            .unwrap()
    );
    let view = session.view().unwrap();
    assert_eq!(view.last_end, Some((turn, TurnEndReason::Completed)));
    assert_eq!(view.last_usage, Some(Usage::known(123, 45)));
    assert_eq!(view.entries.len(), 3);
    drop(session);
    assert_eq!(
        Session::inspect(&path).unwrap().last_usage,
        Some(Usage::known(123, 45))
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejected_batch_does_not_publish_projection_or_settled_cut() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let (turn, _) = session
        .begin_turn(
            "first".into(),
            ModelRef {
                provider: "test".into(),
                model: "test".into(),
            },
        )
        .unwrap();
    let answer = Message {
        role: Role::Assistant,
        content: vec![Content::Text("done".into())],
        provider_replay: None,
    };
    {
        let mut store = session.store.lock().unwrap();
        let before = store.settled.clone();
        assert!(matches!(
            append(
                &mut store,
                &[
                    SessionEntry::Assistant {
                        turn,
                        message: answer.clone(),
                        tool_activities: Vec::new(),
                        execution: ModelExecution {
                            route: ion_ai::ModelRoute::direct(
                                ModelRef {
                                    provider: "test".into(),
                                    model: "test".into(),
                                },
                                ion_ai::ModelRouteReason::UserRequest,
                            ),
                            returned_model: None,
                        },
                        usage: Usage::unknown(),
                        termination: ResponseTermination::Completed,
                    },
                    SessionEntry::Steering {
                        turn,
                        input: Message {
                            role: Role::User,
                            content: vec![Content::Text(String::new())],
                            provider_replay: None,
                        },
                    },
                ],
            ),
            Err(SessionError::InvalidHistory)
        ));
        assert_eq!(store.settled, before);
        assert_eq!(store.messages.len(), 1);
    }
    session
        .record_assistant(turn, answer, Usage::unknown(), false)
        .unwrap();
    assert_eq!(session.view().unwrap().entries.len(), 3);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn name_is_visible_after_reopen_without_changing_history() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    session.set_name(Some("  Investigate parser  ")).unwrap();
    assert_eq!(
        session.view().unwrap().name.as_deref(),
        Some("Investigate parser")
    );
    assert!(session.view().unwrap().entries.is_empty());
    drop(session);
    assert_eq!(
        Session::inspect(&path).unwrap().name.as_deref(),
        Some("Investigate parser")
    );
    let reopened = Session::open(&path).unwrap();
    reopened.set_name(None).unwrap();
    assert!(reopened.view().unwrap().name.is_none());
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn reasoning_preference_is_atomic_exclusive_and_prefix_owned() {
    let root = std::env::temp_dir().join(format!("ion-reasoning-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("session.sqlite");
    let session = Session::create(&path, Some(&root)).unwrap();
    let choose = |session: &Session, reasoning| -> Result<(), SessionError> {
        let guard = session.operations.try_lock().map_err(SessionError::from)?;
        session.record_selection(&guard, test_execution().route.logical, Some(reasoning))
    };
    choose(&session, Reasoning::High).unwrap();
    let before = session.view().unwrap().entries;
    choose(&session, Reasoning::High).unwrap();
    assert_eq!(session.view().unwrap().entries, before);
    assert!(choose(&session, Reasoning::BudgetTokens(0)).is_err());
    assert_eq!(session.view().unwrap().entries, before);
    {
        let _operation = session.operations.try_lock().unwrap();
        assert!(matches!(
            choose(&session, Reasoning::Off),
            Err(SessionError::OperationActive)
        ));
        assert!(matches!(
            session.select_model(test_execution().route.logical),
            Err(SessionError::OperationActive)
        ));
    }
    let operation = session.operations.try_lock().unwrap();
    let (turn, _) = session
        .begin_turn("first".into(), test_execution().route.logical)
        .unwrap();
    assert!(matches!(
        choose(&session, Reasoning::Off),
        Err(SessionError::OperationActive)
    ));
    session
        .end_turn(turn, TurnEndReason::Cancelled, &BTreeSet::new())
        .unwrap();
    drop(operation);
    choose(&session, Reasoning::Off).unwrap();
    for (name, point) in [
        ("before", ForkPoint::BeforeTurn(turn)),
        ("after", ForkPoint::AfterTurn(turn)),
    ] {
        let fork = session
            .fork_to(root.join(format!("{name}.sqlite")), point)
            .unwrap();
        assert_eq!(fork.reasoning().unwrap(), Reasoning::High);
    }
    let clone = session.clone_to(root.join("clone.sqlite")).unwrap();
    assert_eq!(clone.reasoning().unwrap(), Reasoning::Off);
    drop(clone);
    let before = session.view().unwrap().entries;
    session.store.lock().unwrap().connection.execute_batch("CREATE TRIGGER refuse_setting BEFORE INSERT ON entries WHEN json_extract(CAST(NEW.body AS TEXT), '$.kind') = 'reasoning_selected' BEGIN SELECT RAISE(FAIL, 'injected preference failure'); END").unwrap();
    {
        let guard = session.operations.try_lock().unwrap();
        assert!(
            session
                .record_selection(
                    &guard,
                    ModelRef {
                        provider: "test".into(),
                        model: "other".into()
                    },
                    Some(Reasoning::Low)
                )
                .is_err()
        );
    }
    assert_eq!(session.reasoning().unwrap(), Reasoning::Off);
    assert_eq!(session.view().unwrap().entries, before);
    drop(session);
    let reopened = Session::open(path).unwrap();
    assert_eq!(reopened.reasoning().unwrap(), Reasoning::Off);
    assert_eq!(
        reopened.view().unwrap().turns()[0].reasoning,
        Reasoning::High
    );
    drop(reopened);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn idle_preferences_do_not_rewrite_or_recover_an_unfinished_turn() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let original = test_execution().route.logical;
    let operation = session.operations.try_lock().unwrap();
    session
        .record_selection(&operation, original.clone(), Some(Reasoning::High))
        .unwrap();
    let (old_turn, _) = session
        .begin_turn("interrupted".into(), original.clone())
        .unwrap();
    drop(operation);
    drop(session);

    let session = Session::open(&path).unwrap();
    let replacement = ModelRef {
        model: "replacement".into(),
        ..original.clone()
    };
    let operation = session.operations.try_lock().unwrap();
    session
        .record_selection(&operation, replacement.clone(), Some(Reasoning::Off))
        .unwrap();
    drop(operation);
    let selected = session.view().unwrap();
    assert_eq!(selected.unfinished_turn, Some(old_turn));
    assert_eq!(selected.turns()[0].model, original);
    assert_eq!(selected.turns()[0].reasoning, Reasoning::High);
    assert!(selected.turns()[0].end.is_none());
    drop(session);

    let session = Session::open(&path).unwrap();
    assert_eq!(
        session.view().unwrap().last_model,
        Some(replacement.clone())
    );
    assert_eq!(session.reasoning().unwrap(), Reasoning::Off);
    let operation = session.operations.try_lock().unwrap();
    let (new_turn, _) = session
        .begin_turn("new input".into(), replacement.clone())
        .unwrap();
    let turns = session.view().unwrap().turns();
    assert_eq!(turns[0].end, Some(TurnEndReason::Interrupted));
    assert_eq!(turns[0].model, original);
    assert_eq!(turns[0].reasoning, Reasoning::High);
    assert_eq!(turns[1].model, replacement);
    assert_eq!(turns[1].reasoning, Reasoning::Off);
    session
        .end_turn(new_turn, TurnEndReason::Cancelled, &BTreeSet::new())
        .unwrap();
    drop(operation);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn idle_model_choice_survives_reopen() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "new".into(),
    };
    session.select_model(model.clone()).unwrap();
    drop(session);
    assert_eq!(Session::inspect(&path).unwrap().last_model, Some(model));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn compaction_changes_only_the_model_projection_and_survives_reopen() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let assert_input_budget = || {
        for instructions in [
            None,
            Some("Keep \"雪\" and paths\\literal\nwithout guessing"),
        ] {
            let plan = session
                .compaction_plan(instructions, 0, usize::MAX, true, u64::MAX)
                .unwrap()
                .unwrap();
            let input = crate::summary::input(instructions, plan.messages, true).unwrap();
            let bytes = serde_json::to_vec(&input.content).unwrap().len();
            assert_eq!(
                session
                    .compaction_plan(instructions, 0, bytes, true, u64::MAX)
                    .unwrap()
                    .unwrap()
                    .through_entry,
                plan.through_entry
            );
            assert!(
                session
                    .compaction_plan(instructions, 0, bytes - 1, true, u64::MAX)
                    .unwrap()
                    .is_none(),
                "the sole settled prefix must include framing exactly once, including a prior summary"
            );
        }
    };
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (first, _) = session
        .begin_turn("first task".into(), model.clone())
        .unwrap();
    session
        .record_assistant(
            first,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("first result".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let plan = session
        .compaction_plan(None, 0, usize::MAX, true, u64::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(plan.through_entry, 3);
    assert_eq!(plan.messages.len(), 2);
    assert_input_budget();
    assert!(
        session
            .record_compaction(1, "invalid".into(), test_execution(), Usage::unknown())
            .is_err()
    );
    session
        .record_compaction(
            plan.through_entry,
            "first task done".into(),
            test_execution(),
            Usage::unknown(),
        )
        .unwrap();
    let (second, _) = session.begin_turn("next task".into(), model).unwrap();
    session
        .record_assistant(
            second,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("next result".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    assert_eq!(session.messages().unwrap().len(), 4);
    assert_eq!(session.context_messages().unwrap().len(), 3);
    assert_input_budget();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.context_messages().unwrap().len(), 3);
    assert_eq!(reopened.view().unwrap().compacted_through, Some(3));
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn human_thinking_stays_in_facts_not_model_context_across_history_operations() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let (turn, _) = session
        .begin_turn("question".into(), test_execution().route.logical)
        .unwrap();
    let message = Message {
        role: Role::Assistant,
        content: vec![
            Content::Thinking("human thought".into()),
            Content::Text("answer".into()),
        ],
        provider_replay: Some(ProviderReplay::new(
            "test",
            "test-replay",
            serde_json::json!({"signature":"opaque"}),
        )),
    };
    session
        .record_assistant(turn, message.clone(), Usage::unknown(), false)
        .unwrap();
    assert_eq!(session.view().unwrap().display_messages()[1], message);
    let facts = session.view().unwrap().entries;
    assert!(facts.iter().any(|entry| matches!(entry, SessionEntry::Assistant { message: saved, .. } if saved == &message)));
    assert_eq!(
        session.messages().unwrap()[1].content,
        vec![Content::Text("answer".into())]
    );
    assert_eq!(
        session.messages().unwrap()[1].provider_replay,
        message.provider_replay
    );
    let clone = session
        .clone_to(root.join("clone-thinking.sqlite"))
        .unwrap();
    let fork = session
        .fork_to(
            root.join("fork-thinking.sqlite"),
            ForkPoint::AfterTurn(turn),
        )
        .unwrap();
    for copy in [&clone, &fork] {
        assert_eq!(copy.view().unwrap().display_messages()[1], message);
        assert_eq!(copy.view().unwrap().entries, facts);
        assert_eq!(copy.messages().unwrap(), session.messages().unwrap());
    }
    session
        .record_compaction(3, "summary".into(), test_execution(), Usage::unknown())
        .unwrap();
    assert!(
        session
            .context_messages()
            .unwrap()
            .iter()
            .flat_map(|message| &message.content)
            .all(|part| !matches!(part, Content::Thinking(_)))
    );
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.view().unwrap().display_messages()[1], message);
    assert!(reopened.view().unwrap().entries.starts_with(&facts));
    assert_eq!(
        reopened.messages().unwrap()[1].provider_replay,
        message.provider_replay
    );
    assert_eq!(
        reopened.messages().unwrap()[1].content,
        vec![Content::Text("answer".into())]
    );
    drop((reopened, clone, fork));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn model_switch_does_not_revive_earlier_opaque_replay() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let gemini = ModelRef {
        provider: "openrouter".into(),
        model: "google/gemini".into(),
    };
    let other = ModelRef {
        provider: "openrouter".into(),
        model: "other-model".into(),
    };
    let (first, _) = session.begin_turn("first".into(), gemini.clone()).unwrap();
    session
        .record_assistant(
            first,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("first answer".into())],
                provider_replay: Some(ProviderReplay::new(
                    "openrouter",
                    "openrouter_reasoning_details",
                    serde_json::json!([{"type":"reasoning.encrypted","data":"opaque"}]),
                )),
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    session.select_model(other.clone()).unwrap();
    let (second, _) = session.begin_turn("second".into(), other.clone()).unwrap();
    let second_context = session.context_messages_for(&other).unwrap();
    assert!(second_context[1].provider_replay.is_none());
    assert_eq!(session.context_messages().unwrap(), second_context);
    assert_eq!(
        second_context[1].content,
        session.messages().unwrap()[1].content
    );
    session
        .record_assistant(
            second,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("second answer".into())],
                provider_replay: Some(ProviderReplay::new(
                    "openrouter",
                    "openrouter_plain_reasoning",
                    serde_json::json!("reasoning"),
                )),
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    session
        .record_compaction(
            3,
            "first turn done".into(),
            test_execution(),
            Usage::unknown(),
        )
        .unwrap();
    let (third, _) = session.begin_turn("third".into(), gemini.clone()).unwrap();
    let third_context = session.context_messages_for(&gemini).unwrap();
    assert!(
        third_context
            .iter()
            .all(|message| message.provider_replay.is_none())
    );
    session
        .record_assistant(
            third,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "latest".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path":"file.txt"}),
                    raw_arguments: None,
                })],
                provider_replay: Some(ProviderReplay::new(
                    "openrouter",
                    "openrouter_reasoning_details",
                    serde_json::json!([{"type":"reasoning.text","text":"now"}]),
                )),
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let current = session.context_messages_for(&gemini).unwrap();
    assert!(
        current[..current.len() - 1]
            .iter()
            .all(|message| message.provider_replay.is_none())
    );
    assert!(current.last().unwrap().provider_replay.is_some());
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.context_messages_for(&gemini).unwrap(), current);
    assert_eq!(
        reopened
            .messages()
            .unwrap()
            .iter()
            .filter(|message| message.provider_replay.is_some())
            .count(),
        3
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn effective_model_route_changes_do_not_revive_old_replay_under_one_logical_model() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let logical = ModelRef {
        provider: "virtual".into(),
        model: "coding".into(),
    };
    let physical_a = ModelRef {
        provider: "anthropic".into(),
        model: "claude-a".into(),
    };
    let physical_b = ModelRef {
        provider: "openrouter".into(),
        model: "provider/model-b".into(),
    };
    let execution = |effective: ModelRef, returned_model: &str| ModelExecution {
        route: ion_ai::ModelRoute {
            logical: logical.clone(),
            effective,
            reason: ion_ai::ModelRouteReason::UserRequest,
        },
        returned_model: Some(returned_model.into()),
    };

    let (first, _) = session.begin_turn("first".into(), logical.clone()).unwrap();
    let context = ModelContextSnapshot {
        instructions: "routing fixture".into(),
        tools: Vec::new(),
    };
    session
        .admit_request(first, physical_a.clone(), context.clone())
        .unwrap();
    assert!(
        !session
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry, SessionEntry::EffectiveModelChanged { .. }))
    );
    session
        .record_assistant_with_activities(
            first,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("first answer".into())],
                provider_replay: Some(ProviderReplay::new(
                    "anthropic",
                    "anthropic_content_blocks",
                    serde_json::json!({"opaque":"a"}),
                )),
            },
            Vec::new(),
            execution(physical_a.clone(), "claude-a-20261004"),
            Usage::known(10, 2),
            false,
        )
        .unwrap();

    let (second, _) = session
        .begin_turn("second".into(), logical.clone())
        .unwrap();
    assert!(
        session
            .admit_request(second, physical_b.clone(), context.clone())
            .unwrap()
    );
    assert!(
        session
            .context_messages_for(&physical_b)
            .unwrap()
            .iter()
            .all(|message| message.provider_replay.is_none())
    );
    session
        .record_assistant_with_activities(
            second,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("second answer".into())],
                provider_replay: Some(ProviderReplay::new(
                    "openrouter",
                    "openrouter_plain_reasoning",
                    serde_json::json!("b"),
                )),
            },
            Vec::new(),
            execution(physical_b.clone(), "provider/model-b-20261004"),
            Usage::known(12, 3),
            false,
        )
        .unwrap();

    let (third, _) = session.begin_turn("third".into(), logical.clone()).unwrap();
    assert!(
        session
            .admit_request(third, physical_a.clone(), context)
            .unwrap()
    );
    let third_context = session.context_messages_for(&physical_a).unwrap();
    assert!(
        third_context
            .iter()
            .all(|message| message.provider_replay.is_none())
    );
    session
        .record_assistant_with_activities(
            third,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("third answer".into())],
                provider_replay: None,
            },
            Vec::new(),
            execution(physical_a.clone(), "claude-a-20261004"),
            Usage::known(14, 4),
            false,
        )
        .unwrap();

    let view = session.view().unwrap();
    assert_eq!(view.last_model, Some(logical.clone()));
    assert_eq!(view.last_effective_model, Some(physical_a.clone()));
    assert_eq!(
        view.last_execution
            .as_ref()
            .map(|execution| &execution.route.effective),
        Some(&physical_a)
    );
    assert_eq!(
        view.messages
            .iter()
            .filter(|message| message.provider_replay.is_some())
            .count(),
        2
    );
    drop(session);

    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.view().unwrap().last_model, Some(logical));
    assert_eq!(
        reopened.view().unwrap().last_effective_model,
        Some(physical_a.clone())
    );
    assert!(
        reopened
            .context_messages_for(&physical_a)
            .unwrap()
            .iter()
            .all(|message| message.provider_replay.is_none())
    );
    assert_eq!(
        reopened
            .messages()
            .unwrap()
            .iter()
            .filter(|message| message.provider_replay.is_some())
            .count(),
        2
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn provider_rebase_is_durable_and_keeps_raw_assistant_history() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "anthropic".into(),
        model: "claude-test".into(),
    };
    let (first, _) = session.begin_turn("first".into(), model.clone()).unwrap();
    session
        .record_assistant(
            first,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("first answer".into())],
                provider_replay: Some(
                    ProviderReplay::new(
                        "anthropic",
                        "anthropic_content_blocks",
                        serde_json::json!({"blocks":[{"type":"thinking",
                            "thinking":"","signature":"signed"},{"type":"text",
                            "text":"first answer"}],"prefix_sha256":"a".repeat(64)}),
                    )
                    .with_prefix_binding(true),
                ),
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let (second, _) = session.begin_turn("second".into(), model.clone()).unwrap();
    assert!(
        session.context_messages_for(&model).unwrap()[1]
            .provider_replay
            .is_some()
    );
    session.rebase_provider_replay(second).unwrap();
    assert!(
        session.context_messages_for(&model).unwrap()[1]
            .provider_replay
            .is_none()
    );
    assert_eq!(
        session.messages().unwrap()[1]
            .provider_replay
            .as_ref()
            .unwrap()
            .kind,
        "anthropic_content_blocks"
    );
    session
        .record_assistant(
            second,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("second answer".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    assert!(matches!(
        session.rebase_provider_replay(second),
        Err(SessionError::InvalidHistory)
    ));
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(
        reopened.context_messages_for(&model).unwrap()[1]
            .provider_replay
            .is_none()
    );
    assert!(reopened.messages().unwrap()[1].provider_replay.is_some());
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn parent_result_cannot_settle_a_pending_child() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (turn, _) = session.begin_turn("compose".into(), model).unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "parent".into(),
                    name: "code_mode".into(),
                    arguments: serde_json::json!({"code":"return true"}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let parent = session.tool_occurrence(turn, "parent").unwrap();
    session
        .record_child_intent(
            turn,
            crate::ChildIntent {
                parent,
                child: 0,
                call: ToolCall {
                    id: "child".into(),
                    name: "write".into(),
                    arguments: serde_json::json!({}),
                    raw_arguments: None,
                },
                definition: ion_ai::ToolSpec {
                    name: "write".into(),
                    description: String::new(),
                    input_schema: serde_json::json!({"type":"object"}),
                },
                activity: crate::ToolActivity::external("write"),
            },
        )
        .unwrap();
    let result = ToolResult {
        call_id: "parent".into(),
        name: "code_mode".into(),
        result: serde_json::json!({"result":true}),
        images: Vec::new(),
        is_error: false,
    };
    let before = session.view().unwrap().entries;
    assert!(matches!(
        session.record_tool_result(turn, result.clone()),
        Err(SessionError::InvalidHistory)
    ));
    assert_eq!(session.view().unwrap().entries, before);
    session
        .record_child_outcome(
            turn,
            parent,
            0,
            crate::ChildOutcome::Observed {
                output: crate::tool_result::ToolOutput {
                    value: serde_json::json!({"written":true}),
                    images: Vec::new(),
                    is_error: false,
                },
            },
        )
        .unwrap();
    session.record_tool_result(turn, result).unwrap();
    session
        .end_turn(turn, TurnEndReason::Completed, &BTreeSet::new())
        .unwrap();
    drop(session);
    Session::open(&path).unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn code_parent_discovery_pages_coordinates_and_excludes_unsettled_calls() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let (turn, _) = session
        .begin_turn(
            "compose".into(),
            ModelRef {
                provider: "test".into(),
                model: "test".into(),
            },
        )
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: (0..12)
                    .map(|ordinal| {
                        Content::ToolCall(ToolCall {
                            id: format!("parent{ordinal}"),
                            name: "code_mode".into(),
                            arguments: serde_json::json!({"code":"PRIVATE_CODE"}),
                            raw_arguments: None,
                        })
                    })
                    .collect(),
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let mut parents = Vec::new();
    for ordinal in 0..12 {
        let id = format!("parent{ordinal}");
        let parent = session.tool_occurrence(turn, &id).unwrap();
        parents.push(parent);
        session
            .record_child_intent(
                turn,
                crate::ChildIntent {
                    parent,
                    child: 0,
                    call: ToolCall {
                        id: format!("child{ordinal}"),
                        name: "PRIVATE_TOOL".into(),
                        arguments: serde_json::json!({"private":"PRIVATE_ARGUMENT"}),
                        raw_arguments: None,
                    },
                    definition: ToolSpec {
                        name: "PRIVATE_TOOL".into(),
                        description: String::new(),
                        input_schema: serde_json::json!({"type":"object"}),
                    },
                    activity: crate::ToolActivity::external("PRIVATE_TOOL"),
                },
            )
            .unwrap();
        session
            .record_child_outcome(turn, parent, 0, crate::ChildOutcome::Unknown)
            .unwrap();
        if ordinal < 11 {
            session
                .record_tool_result(
                    turn,
                    ToolResult {
                        call_id: id,
                        name: "code_mode".into(),
                        result: serde_json::json!(null),
                        images: Vec::new(),
                        is_error: true,
                    },
                )
                .unwrap();
        }
    }
    let before = session.view().unwrap().entries;
    let first = session.inspect_code(r#"{"kind":"parents"}"#).unwrap();
    let expected = parents[1..11]
        .iter()
        .rev()
        .map(|parent| serde_json::json!({"parent":parent,"child_count":1}))
        .collect::<Vec<_>>();
    assert_eq!(first["parents"], serde_json::json!(expected));
    assert_eq!(first["next_before"], serde_json::json!(parents[1]));
    assert!(!first.to_string().contains("PRIVATE"));
    let last = session
        .inspect_code(
            &serde_json::json!({
                "kind":"parents","before":first["next_before"]
            })
            .to_string(),
        )
        .unwrap();
    assert_eq!(
        last,
        serde_json::json!({
            "parents":[{"parent":parents[0],"child_count":1}],"next_before":null
        })
    );
    let empty = session
        .inspect_code(
            &serde_json::json!({
                "kind":"parents","before":parents[0]
            })
            .to_string(),
        )
        .unwrap();
    assert_eq!(empty, serde_json::json!({"parents":[],"next_before":null}));
    for invalid in [
        serde_json::json!({"kind":"parents","parent":parents[0]}),
        serde_json::json!({"kind":"parents","before":{"assistant_entry":-1,"ordinal":0}}),
        serde_json::json!({"kind":"parents","before":{"assistant_entry":1}}),
    ] {
        assert!(
            session.inspect_code(&invalid.to_string()).is_err(),
            "{invalid}"
        );
    }
    assert_eq!(session.view().unwrap().entries, before);
    session
        .end_turn(turn, TurnEndReason::Cancelled, &BTreeSet::new())
        .unwrap();
    let settled = session.inspect_code(r#"{"kind":"parents"}"#).unwrap();
    assert_eq!(
        settled["parents"][0]["parent"],
        serde_json::json!(parents[11])
    );
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(
        reopened.inspect_code(r#"{"kind":"parents"}"#).unwrap(),
        settled
    );
    let copy = reopened.clone_to(root.join("copy.sqlite")).unwrap();
    assert_eq!(copy.inspect_code(r#"{"kind":"parents"}"#).unwrap(), settled);
    drop(copy);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn code_inspection_preserves_states_exact_values_and_bounded_metadata() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let (turn, _) = session
        .begin_turn(
            "compose".into(),
            ModelRef {
                provider: "test".into(),
                model: "test".into(),
            },
        )
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "parent".into(),
                    name: "code_mode".into(),
                    arguments: serde_json::json!({"code":"return true"}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let parent = session.tool_occurrence(turn, "parent").unwrap();
    let list = serde_json::json!({"kind":"children","parent":parent});
    assert!(session.inspect_code(&list.to_string()).is_err());
    for child in 0..12 {
        session
            .record_child_intent(
                turn,
                crate::ChildIntent {
                    parent,
                    child,
                    call: ToolCall {
                        id: format!("child{child}"),
                        name: "test".into(),
                        arguments: serde_json::json!({}),
                        raw_arguments: None,
                    },
                    definition: ToolSpec {
                        name: "test".into(),
                        description: String::new(),
                        input_schema: serde_json::json!({"type":"object"}),
                    },
                    activity: crate::ToolActivity::external("test"),
                },
            )
            .unwrap();
        let outcome = match child {
            1 => crate::ChildOutcome::Unknown,
            2 => crate::ChildOutcome::NotDispatched {
                reason: "STOPPED_".repeat(200_000),
            },
            _ => crate::ChildOutcome::Observed {
                output: crate::tool_result::ToolOutput {
                    value: match child {
                        0 => serde_json::json!(u64::MAX),
                        3 => serde_json::Value::Null,
                        4 => serde_json::json!("😀"),
                        6 => serde_json::json!({"x".repeat(1024*1024+1):"large key","small":1}),
                        _ => serde_json::json!({"a/b~":child,"private":"PRIVATE_FACT"}),
                    },
                    images: Vec::new(),
                    is_error: child == 3,
                },
            },
        };
        session
            .record_child_outcome(turn, parent, child, outcome)
            .unwrap();
    }
    session
        .record_tool_result(
            turn,
            ToolResult {
                call_id: "parent".into(),
                name: "code_mode".into(),
                result: serde_json::json!({"error":"stopped"}),
                images: Vec::new(),
                is_error: true,
            },
        )
        .unwrap();
    session
        .end_turn(turn, TurnEndReason::Completed, &BTreeSet::new())
        .unwrap();
    let before = session.view().unwrap().entries;
    let first = session.inspect_code(&list.to_string()).unwrap();
    assert_eq!(first["children"].as_array().unwrap().len(), 10);
    assert_eq!(first["next_after"], 9);
    assert_eq!(first["children"][1]["state"], "unknown");
    assert_eq!(first["children"][2]["state"], "not_dispatched");
    assert_eq!(first["children"][2]["reason_truncated"], true);
    assert!(first["children"][2]["reason"].as_str().unwrap().len() <= 1024);
    assert!(!first.to_string().contains("PRIVATE_FACT"));
    assert!(first.to_string().len() < 32768);
    assert_eq!(first["children"][6]["keys_truncated"], true);
    let small = session.inspect_code(&serde_json::json!({"kind":"output","parent":parent,"child":6,"pointer":"/small","limit":4}).to_string()).unwrap();
    assert_eq!(small["json"], "1");
    assert!(small.to_string().len() < 1024);
    let second = session
        .inspect_code(
            &serde_json::json!({"kind":"children","parent":parent,"after":9}).to_string(),
        )
        .unwrap();
    assert_eq!(second["children"].as_array().unwrap().len(), 2);
    assert_eq!(second["next_after"], serde_json::Value::Null);
    for (child, expected) in [(0, u64::MAX.to_string()), (3, "null".into())] {
        let page = session
            .inspect_code(
                &serde_json::json!({"kind":"output","parent":parent,"child":child}).to_string(),
            )
            .unwrap();
        assert_eq!(page["json"], expected);
    }
    for child in [1, 2] {
        let result = session
            .inspect_code(
                &serde_json::json!({"kind":"output","parent":parent,"child":child}).to_string(),
            )
            .unwrap();
        assert!(result.get("json").is_none());
        assert!(result.get("is_error").is_none());
    }
    let selected = session
        .inspect_code(
            &serde_json::json!({"kind":"output","parent":parent,"child":5,"pointer":"/a~1b~0"})
                .to_string(),
        )
        .unwrap();
    assert_eq!(selected["json"], "5");
    assert_eq!(selected["root_value_type"], "object");
    assert!(selected.get("pointer").is_none());
    for invalid in [
        serde_json::json!({"kind":"output","parent":parent,"child":4,"offset":2}),
        serde_json::json!({"kind":"output","parent":parent,"child":0,"limit":65537}),
        serde_json::json!({"kind":"output","parent":parent,"child":5,"pointer":"/images"}),
        serde_json::json!({"kind":"children","parent":parent,"entry":1}),
        serde_json::json!({"kind":"children","parent":{"assistant_entry":1,"ordinal":0}}),
    ] {
        assert!(
            session.inspect_code(&invalid.to_string()).is_err(),
            "{invalid}"
        );
    }
    assert_eq!(session.view().unwrap().entries, before);
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.inspect_code(&list.to_string()).unwrap(), first);
    let copy = reopened.clone_to(root.join("copy.sqlite")).unwrap();
    assert_eq!(copy.inspect_code(&list.to_string()).unwrap(), first);
    drop(copy);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn compaction_suffix_budget_does_not_charge_opaque_replay() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    for text in ["old", "recent"] {
        let (turn, _) = session.begin_turn(text.into(), model.clone()).unwrap();
        session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text(text.into())],
                    provider_replay: Some(ion_ai::ProviderReplay::new(
                        "test",
                        "opaque",
                        serde_json::json!("x".repeat(4_096)),
                    )),
                },
                Usage::unknown(),
                false,
            )
            .unwrap();
    }
    let plan = session
        .compaction_plan(None, 1_000, usize::MAX, true, u64::MAX)
        .unwrap()
        .unwrap();
    let summarized = plan.messages.len();
    session
        .record_compaction(
            plan.through_entry,
            "old summary".into(),
            test_execution(),
            Usage::unknown(),
        )
        .unwrap();
    let context = session.context_messages().unwrap();
    drop(session);
    fs::remove_dir_all(root).unwrap();
    assert_eq!(
        summarized, 2,
        "the recent turn fits the neutral suffix budget"
    );
    assert_eq!(context.len(), 3);
    assert_eq!(context[2].content, vec![Content::Text("recent".into())]);
    assert_eq!(
        context[2].provider_replay.as_ref().unwrap().data,
        serde_json::json!("x".repeat(4_096)),
        "sizing must not discard retained continuation data"
    );
}

#[test]
fn compaction_never_cuts_between_parallel_tool_results() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (turn, _) = session.begin_turn("inspect".into(), model).unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: ["one", "two"]
                    .into_iter()
                    .map(|id| {
                        Content::ToolCall(ToolCall {
                            id: id.into(),
                            name: "read".into(),
                            arguments: serde_json::json!({"path": id}),
                            raw_arguments: None,
                        })
                    })
                    .collect(),
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
                call_id: "one".into(),
                name: "read".into(),
                result: serde_json::json!({"content":"a"}),
                images: Vec::new(),
                is_error: false,
            },
        )
        .unwrap();
    assert!(
        session
            .compaction_plan(None, 0, usize::MAX, true, u64::MAX)
            .unwrap()
            .is_none()
    );
    session
        .record_tool_result(
            turn,
            ToolResult {
                call_id: "two".into(),
                name: "read".into(),
                result: serde_json::json!({"content":"b".repeat(8000)}),
                images: Vec::new(),
                is_error: false,
            },
        )
        .unwrap();
    assert_eq!(
        session
            .compaction_plan(None, 0, usize::MAX, true, u64::MAX)
            .unwrap()
            .unwrap()
            .through_entry,
        4
    );
    assert_eq!(
        session
            .compaction_plan(None, 1000, usize::MAX, true, u64::MAX)
            .unwrap()
            .unwrap()
            .through_entry,
        4
    );
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn repeated_compaction_after_cancelled_tool_batch_has_no_new_prefix() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (turn, _) = session.begin_turn("inspect".into(), model).unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "call".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path":"file"}),
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
                call_id: "call".into(),
                name: "read".into(),
                result: serde_json::json!({"content":"observed"}),
                images: Vec::new(),
                is_error: false,
            },
        )
        .unwrap();
    session
        .end_turn(turn, TurnEndReason::Cancelled, &BTreeSet::new())
        .unwrap();
    let plan = session
        .compaction_plan(None, 1, usize::MAX, false, u64::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(plan.messages.len(), 3);
    session
        .record_compaction(
            plan.through_entry,
            "saved observation".into(),
            test_execution(),
            Usage::unknown(),
        )
        .unwrap();
    assert!(
        session
            .compaction_plan(None, 1, usize::MAX, false, u64::MAX)
            .unwrap()
            .is_none()
    );
    let context = session.context_messages().unwrap();
    assert_eq!(context.len(), 1);
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.context_messages().unwrap(), context);
    assert!(
        reopened
            .compaction_plan(None, 1, usize::MAX, false, u64::MAX)
            .unwrap()
            .is_none()
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn compaction_keeps_oversized_recent_tool_batch_when_older_cut_exists() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (old_turn, _) = session
        .begin_turn("old task".into(), model.clone())
        .unwrap();
    session
        .record_assistant(
            old_turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("old answer".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let (current_turn, _) = session.begin_turn("read big file".into(), model).unwrap();
    session
        .record_assistant(
            current_turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "call-1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path":"big.txt"}),
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
            current_turn,
            ToolResult {
                call_id: "call-1".into(),
                name: "read".into(),
                result: serde_json::json!({"content":"x".repeat(8000)}),
                images: Vec::new(),
                is_error: false,
            },
        )
        .unwrap();
    let plan = session
        .compaction_plan(None, 1000, usize::MAX, true, u64::MAX)
        .unwrap()
        .unwrap();
    assert_eq!(plan.through_entry, 3);
    assert_eq!(plan.messages.len(), 2);
    session
        .record_compaction(
            plan.through_entry,
            "old task done".into(),
            test_execution(),
            Usage::unknown(),
        )
        .unwrap();
    let context = session.context_messages().unwrap();
    assert_eq!(context.len(), 4);
    assert!(matches!(context[2].content[0], Content::ToolCall(_)));
    assert!(matches!(context[3].content[0], Content::ToolResult(_)));
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.context_messages().unwrap(), context);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn completed_text_response_is_a_compaction_boundary_during_steering() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (turn, _) = session.begin_turn("first request".into(), model).unwrap();
    assert!(
        !session
            .record_assistant(
                turn,
                Message {
                    role: Role::Assistant,
                    content: vec![Content::Text("first response".into())],
                    provider_replay: None,
                },
                Usage::unknown(),
                true,
            )
            .unwrap()
    );
    session
        .record_steerings(
            turn,
            vec![Message {
                role: Role::User,
                content: vec![Content::Text("follow-up steering".into())],
                provider_replay: None,
            }],
        )
        .unwrap();
    assert_eq!(
        session
            .compaction_plan(None, 0, usize::MAX, true, u64::MAX)
            .unwrap()
            .unwrap()
            .through_entry,
        2
    );
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(
        reopened
            .compaction_plan(None, 0, usize::MAX, true, u64::MAX)
            .unwrap()
            .unwrap()
            .through_entry,
        2
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_tool_result_survives_reopen() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (turn, _) = session
        .begin_turn("read missing file".into(), model)
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ToolCall {
                    id: "call".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path":"missing"}),
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
                call_id: "call".into(),
                name: "read".into(),
                result: serde_json::json!({"error":"file missing"}),
                images: Vec::new(),
                is_error: true,
            },
        )
        .unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert!(matches!(
        &reopened.messages().unwrap()[2].content[0],
        Content::ToolResult(ToolResult { is_error: true, .. })
    ));
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn model_context_changes_are_durable_and_elide_repeats() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let (turn, _) = session.begin_turn("hello".into(), model.clone()).unwrap();
    let first = ModelContextSnapshot {
        instructions: "first instructions".into(),
        tools: vec![ToolSpec {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type":"object"}),
        }],
    };
    assert!(
        session
            .admit_request(turn, model.clone(), first.clone())
            .unwrap()
    );
    assert!(!session.admit_request(turn, model.clone(), first).unwrap());

    let changed = ModelContextSnapshot {
        instructions: "changed instructions".into(),
        tools: vec![ToolSpec {
            name: "read".into(),
            description: "Read a file".into(),
            input_schema: serde_json::json!({"type":"object"}),
        }],
    };
    assert!(session.admit_request(turn, model, changed.clone()).unwrap());
    let view = session.view().unwrap();
    assert_eq!(view.last_context, Some(changed.clone()));
    assert_eq!(
        view.entries
            .iter()
            .filter(|entry| matches!(entry, SessionEntry::ModelContextChanged { .. }))
            .count(),
        2
    );
    drop(session);

    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.view().unwrap().last_context, Some(changed));
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn request_basis_keeps_context_and_replay_at_one_committed_boundary() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let context = |instructions: &str| ModelContextSnapshot {
        instructions: instructions.into(),
        tools: Vec::new(),
    };
    let (turn, _) = session.begin_turn("first".into(), model.clone()).unwrap();
    session
        .admit_request(turn, model.clone(), context("A"))
        .unwrap();
    let captured = session.request_basis(&model).unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("continue".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            true,
        )
        .unwrap();
    session
        .record_steerings(
            turn,
            vec![Message {
                role: Role::User,
                content: vec![Content::Text("later".into())],
                provider_replay: None,
            }],
        )
        .unwrap();
    session
        .admit_request(turn, model.clone(), context("B"))
        .unwrap();
    let current = session.request_basis(&model).unwrap();
    assert_eq!(captured.turn, Some(turn));
    assert_eq!(captured.messages.len(), 1);
    assert_eq!(captured.previous, Some(context("A")));
    assert_eq!(captured.reasoning, Reasoning::ProviderDefault);
    assert!(!captured.assistant_seen);
    assert!(!captured.prefix_bound);
    assert_eq!(captured.timeline(&context("C")), None);
    assert_eq!(current.messages.len(), 3);
    assert_eq!(current.previous, Some(context("B")));
    assert!(current.assistant_seen);
    assert!(!current.prefix_bound);
    assert_eq!(
        current.timeline(&context("B")).unwrap().changes,
        vec![ModelContextChange {
            after_message: 3,
            context: model_context_state(&context("B"))
        },]
    );
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn context_timeline_tracks_committed_message_boundaries() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let context = |instructions: &str| ModelContextSnapshot {
        instructions: instructions.into(),
        tools: Vec::new(),
    };
    let (first, _) = session.begin_turn("first".into(), model.clone()).unwrap();
    session
        .admit_request(first, model.clone(), context("A"))
        .unwrap();
    session
        .record_assistant(
            first,
            Message {
                role: Role::Assistant,
                content: ["one", "two"]
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
    for id in ["one", "two"] {
        session
            .record_tool_result_with_context(
                first,
                ToolResult {
                    call_id: id.into(),
                    name: "read".into(),
                    result: serde_json::json!({"ok":true}),
                    images: Vec::new(),
                    is_error: false,
                },
                crate::ToolResultProjection::Observed,
                (id == "two").then(|| context("B")),
            )
            .unwrap();
    }
    session
        .record_steerings(
            first,
            vec![Message {
                role: Role::User,
                content: vec![Content::Text("steer".into())],
                provider_replay: None,
            }],
        )
        .unwrap();
    session
        .record_assistant(
            first,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("done".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    for excluded in [false, true] {
        let admission = session
            .admit_user_shell("echo observed".into(), excluded)
            .unwrap();
        session
            .record_user_shell(
                admission,
                serde_json::json!({"stdout":"observed","exit_code":0}),
                false,
            )
            .unwrap();
    }
    let (second, _) = session.begin_turn("second".into(), model.clone()).unwrap();
    // user + assistant + two results + steering + answer + shared shell +
    // user = eight. The excluded shell adds no neutral transcript message.
    let expected = |later: &[&str]| ModelContextTimeline {
        initial: model_context_state(&context("A")),
        changes: std::iter::once(ModelContextChange {
            after_message: 4,
            context: model_context_state(&context("B")),
        })
        .chain(later.iter().map(|name| ModelContextChange {
            after_message: 8,
            context: model_context_state(&context(name)),
        }))
        .collect(),
    };
    assert_eq!(
        session.context_timeline_for(&model, &context("C")).unwrap(),
        Some(expected(&["C"]))
    );
    session.store.lock().unwrap().connection.execute_batch(
        "CREATE TRIGGER reject_context BEFORE INSERT ON entries WHEN json_extract(CAST(NEW.body AS TEXT), '$.kind') = 'model_context_changed' BEGIN SELECT RAISE(ABORT, 'storage unavailable'); END;"
    ).unwrap();
    assert!(
        session
            .admit_request(second, model.clone(), context("C"))
            .is_err()
    );
    assert_eq!(
        session.context_timeline_for(&model, &context("B")).unwrap(),
        Some(expected(&[]))
    );
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute_batch("DROP TRIGGER reject_context")
        .unwrap();
    for name in ["C", "D"] {
        session
            .admit_request(second, model.clone(), context(name))
            .unwrap();
    }
    assert_eq!(
        session.context_timeline_for(&model, &context("D")).unwrap(),
        Some(expected(&["C", "D"]))
    );
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(
        reopened
            .context_timeline_for(&model, &context("D"))
            .unwrap(),
        Some(expected(&["C", "D"]))
    );
    reopened.rebase_provider_replay(second).unwrap();
    assert_eq!(
        reopened
            .context_timeline_for(&model, &context("D"))
            .unwrap(),
        None
    );
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn clone_and_fork_follow_model_context_history() {
    let (root, path) = fixture();
    let session = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    let context = |instructions: &str| ModelContextSnapshot {
        instructions: instructions.into(),
        tools: vec![ToolSpec {
            name: "read".into(),
            description: "Read".into(),
            input_schema: serde_json::json!({"type":"object"}),
        }],
    };

    let (first, _) = session.begin_turn("first".into(), model.clone()).unwrap();
    session
        .admit_request(first, model.clone(), context("first context"))
        .unwrap();
    session
        .record_assistant(
            first,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("first done".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();

    let (second, _) = session.begin_turn("second".into(), model.clone()).unwrap();
    session
        .admit_request(second, model.clone(), context("second context"))
        .unwrap();
    session
        .record_assistant(
            second,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("second done".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();

    let clone = session.clone_to(root.join("clone-context.sqlite")).unwrap();
    assert_eq!(
        clone.model_context().unwrap().unwrap().instructions,
        "second context"
    );

    let before = session
        .fork_to(
            root.join("before-context.sqlite"),
            ForkPoint::BeforeTurn(second),
        )
        .unwrap();
    assert_eq!(
        before.model_context().unwrap().unwrap().instructions,
        "first context"
    );

    let after = session
        .fork_to(
            root.join("after-context.sqlite"),
            ForkPoint::AfterTurn(second),
        )
        .unwrap();
    assert_eq!(
        after.model_context().unwrap().unwrap().instructions,
        "second context"
    );

    for (copy, latest, changes) in [
        (&before, "first context", Vec::new()),
        (
            &clone,
            "second context",
            vec![ModelContextChange {
                after_message: 3,
                context: model_context_state(&context("second context")),
            }],
        ),
        (
            &after,
            "second context",
            vec![ModelContextChange {
                after_message: 3,
                context: model_context_state(&context("second context")),
            }],
        ),
    ] {
        assert_eq!(
            copy.context_timeline_for(&model, &context(latest)).unwrap(),
            Some(ModelContextTimeline {
                initial: model_context_state(&context("first context")),
                changes,
            })
        );
    }
    drop(after);
    drop(before);
    drop(clone);
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn cloned_session_preserves_context_but_diverges_independently() {
    let (root, path) = fixture();
    let clone_path = root.join("clone.sqlite");
    let source = Session::create(&path, Some(&root)).unwrap();
    let model = ModelRef {
        provider: "test".into(),
        model: "test".into(),
    };
    source.set_name(Some("Source")).unwrap();
    let (first, _) = source.begin_turn("first".into(), model.clone()).unwrap();
    source
        .record_assistant(
            first,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("done".into())],
                provider_replay: None,
            },
            Usage::known(10, 2),
            false,
        )
        .unwrap();
    source
        .record_compaction(
            3,
            "first is done".into(),
            test_execution(),
            Usage::unknown(),
        )
        .unwrap();
    let (second, _) = source.begin_turn("second".into(), model.clone()).unwrap();
    let original = source.view().unwrap();
    let cloned = source.clone_to(&clone_path).unwrap();
    let source_id = source.provider_session_id();
    let clone_id = cloned.provider_session_id();
    assert_ne!(source_id, clone_id);
    assert_eq!(cloned.view().unwrap().entries, original.entries);
    assert_eq!(
        cloned.context_messages().unwrap(),
        source.context_messages().unwrap()
    );
    assert_eq!(cloned.view().unwrap().unfinished_turn, Some(second));
    assert_eq!(cloned.view().unwrap().name, None);
    let (third, interrupted) = cloned.begin_turn("alternate".into(), model).unwrap();
    assert_eq!((third, interrupted), (second + 1, 0));
    assert_eq!(source.view().unwrap().entries, original.entries);
    drop(cloned);
    drop(source);
    assert_eq!(
        Session::open(&path).unwrap().provider_session_id(),
        source_id
    );
    assert_eq!(
        Session::open(&clone_path).unwrap().provider_session_id(),
        clone_id
    );
    assert_eq!(
        Session::inspect(&clone_path).unwrap().last_end,
        Some((second, TurnEndReason::Interrupted))
    );
    fs::remove_dir_all(root).unwrap();
}
