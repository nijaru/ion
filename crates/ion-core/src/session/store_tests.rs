use super::*;
use std::io::Write;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ion-store-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    fn path(&self) -> PathBuf {
        self.0.join("session.sqlite")
    }

    fn session(&self) -> Session {
        Session::create(self.path(), None).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn seed_append_and_reopen_use_only_the_acquired_connection() {
    let header = Header {
        version: FORMAT_VERSION,
        id: uuid::Uuid::new_v4(),
        provider_session_id: uuid::Uuid::now_v7(),
        cwd: None,
        name: None,
    };
    let initial = [SessionEntry::ModelSelected {
        model: model("fixture"),
    }];
    let connection = Connection::open_in_memory().unwrap();
    configure_connection(&connection).unwrap();
    let mut working = store::StoreSeed::prepare(&initial)
        .unwrap()
        .initialize(connection, &header, |_| Ok(()))
        .unwrap();
    let key = RequestKey::new("connection".into(), "once".into()).unwrap();
    let input = Message::user_input("accepted once".into(), []);
    working
        .append(&[SessionEntry::TurnStarted {
            turn: 1,
            input: input.clone(),
            model: model("fixture"),
            completion: Vec::new(),
            request: Some(key.clone()),
            recovery: None,
        }])
        .unwrap();

    // Discard the live projection but keep the database. No file or writer
    // lease exists, and no synthetic repair may be appended on reconstruction.
    let (mut reopened, retained) = Store::reopen(working.connection).unwrap();
    assert_eq!(retained.identity(), header.identity());
    assert_eq!(reopened.state.sequence, 2);
    assert_eq!(reopened.state.active, Some(1));
    assert_eq!(reopened.messages[0].message, input);
    let accepted = submission::request_receipt(&reopened.connection, header.id, &key, Some(&input))
        .unwrap()
        .unwrap();
    assert_eq!(accepted.turn, 1);
    assert!(accepted.end.is_none());

    reopened
        .append(&[SessionEntry::TurnEnded {
            turn: 1,
            reason: TurnEndReason::Interrupted,
        }])
        .unwrap();
    let (final_store, _) = Store::reopen(reopened.connection).unwrap();
    assert_eq!(final_store.state.sequence, 3);
    assert_eq!(final_store.state.active, None);
    let settled =
        submission::request_receipt(&final_store.connection, header.id, &key, Some(&input))
            .unwrap()
            .unwrap();
    assert_eq!(settled.turn, accepted.turn);
    assert_eq!(settled.end, Some(TurnEndReason::Interrupted));
}

#[tokio::test]
async fn pinned_view_does_not_block_commits_or_mix_journal_versions() {
    let fixture = Fixture::new();
    let session = Arc::new(fixture.session());
    let writer = session.io().unwrap();
    writer
        .run(|session| {
            session.set_name(Some("before"))?;
            session.select_model(model("before"))
        })
        .await
        .unwrap();

    let connection = Connection::open_with_flags(
        session.file_path().unwrap(),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (snapshot, view, sequence) = session.pin_view(&connection).unwrap();
    // Keep the durable snapshot open while independently committing both a
    // header update and an append through the normal writer lane.
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        writer.run(|session| {
            session.set_name(Some("after"))?;
            session.select_model(model("after"))
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let entries = read_entries(&snapshot).unwrap();
    assert_eq!(
        read_header(&snapshot).unwrap().name.as_deref(),
        Some("before")
    );
    assert_eq!(view.name.as_deref(), Some("before"));
    assert_eq!(view.last_model, Some(model("before")));
    assert_eq!(entries.len() as u64, sequence);
    assert!(
        matches!(entries.as_slice(), [SessionEntry::ModelSelected { model: selected }] if *selected == model("before"))
    );
    drop(snapshot);

    let latest = session.reader().unwrap().view().await.unwrap();
    assert_eq!(latest.name.as_deref(), Some("after"));
    assert_eq!(latest.last_model, Some(model("after")));
    assert_eq!(latest.entries.len(), entries.len() + 1);
    session.shutdown().await.unwrap();
}

#[test]
fn reopen_checkpoint_pins_header_scan_and_context_point_reads() {
    let fixture = Fixture::new();
    let session = fixture.session();
    session.set_name(Some("before")).unwrap();
    let TurnAdmission::New {
        entry: input_entry, ..
    } = session
        .admit_turn(
            Message::user_input("original input".into(), []),
            model("fixture"),
            None,
            None,
        )
        .unwrap()
    else {
        panic!("expected new admission")
    };
    let connection =
        Connection::open_with_flags(fixture.path(), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let checkpoint = ReadCheckpoint::pin(&connection).unwrap();

    // Test-only rewrite of a committed physical row: merely checking the tail
    // couldn't detect a point read accidentally escaping the pinned snapshot.
    let mut other = Connection::open(fixture.path()).unwrap();
    let write = other.transaction().unwrap();
    let mut header = read_header(&write).unwrap();
    header.name = Some("after".into());
    write
        .execute(
            "UPDATE session SET header=?1 WHERE id=1",
            params![serde_json::to_vec(&header).unwrap()],
        )
        .unwrap();
    let mut entry = read_entry(&write, input_entry).unwrap();
    let SessionEntry::TurnStarted { input, .. } = &mut entry else {
        panic!("expected accepted input")
    };
    *input = Message::user_input("rewritten input".into(), []);
    write
        .execute(
            "UPDATE entries SET body=?1 WHERE seq=?2",
            params![
                encode_entry(&entry).unwrap(),
                i64::try_from(input_entry).unwrap()
            ],
        )
        .unwrap();
    write.commit().unwrap();

    for (cut, label, prompt) in [
        (checkpoint, "before", "original input"),
        (
            ReadCheckpoint::pin(&other).unwrap(),
            "after",
            "rewritten input",
        ),
    ] {
        assert_eq!(cut.header.name.as_deref(), Some(label));
        assert_eq!(
            cut.project(ProjectionMode::Metadata)
                .unwrap()
                .latest_prompt
                .as_deref(),
            Some(prompt)
        );
        let working = cut.project(ProjectionMode::Working).unwrap();
        assert_eq!(
            working.messages[0].message,
            Message::user_input(prompt.into(), [])
        );
        assert_eq!(working.state.active, Some(1));
    }
}

fn model(name: &str) -> ModelRef {
    ModelRef {
        provider: "fixture".into(),
        model: name.into(),
    }
}

fn sql(session: &Session, sql: &str) {
    // Fault injection and evidence inspection deliberately bypass the gate.
    session
        .store()
        .lock()
        .unwrap()
        .connection
        .execute_batch(sql)
        .unwrap();
}

fn assert_faulted<T>(result: Result<T, SessionError>) {
    let Err(error) = result else {
        panic!("faulted Store admitted an operation");
    };
    assert!(!error.is_commit_uncertain());
    assert!(matches!(error, SessionError::StoreFaulted));
}

#[test]
fn stale_append_refuses_to_write_or_publish_working_state() {
    let fixture = Fixture::new();
    let session = fixture.session();
    session.select_model(model("before")).unwrap();
    let unexpected = SessionEntry::ModelSelected {
        model: model("other writer"),
    };
    // Deliberately bypass the cooperative file lease to simulate a stale writer.
    let external = Connection::open(fixture.path()).unwrap();
    external
        .execute(
            "INSERT INTO entries(body) VALUES (?1)",
            params![encode_entry(&unexpected).unwrap()],
        )
        .unwrap();

    let refusal = session.select_model(model("must not publish")).unwrap_err();
    assert!(matches!(
        refusal,
        SessionError::JournalFrontierChanged {
            expected_tail: 1,
            actual: Some(2)
        }
    ));
    assert!(!refusal.is_commit_uncertain());
    assert_faulted(session.entry_count());
    assert_faulted(session.select_model(model("must not write again")));
    assert_eq!(session.store().lock().unwrap().state.sequence, 1);
    let saved = Session::inspect(fixture.path()).unwrap();
    assert_eq!(saved.entries.len(), 2);
    assert_eq!(saved.last_model, Some(model("other writer")));
}

#[test]
fn matching_tail_does_not_authorize_writes_to_another_journal() {
    for rename in [false, true] {
        let fixture = Fixture::new();
        let session = fixture.session();
        session.select_model(model("before")).unwrap();

        let external = Connection::open(fixture.path()).unwrap();
        let mut header = read_header(&external).unwrap();
        if rename {
            header.id = uuid::Uuid::new_v4();
        } else {
            // A copied journal can retain the display Session ID but belong to
            // a different provider lifetime. The tail in both cases is one.
            header.provider_session_id = uuid::Uuid::new_v4();
        }
        header.name = Some("other owner".into());
        external
            .execute(
                "UPDATE session SET header=?1 WHERE id=1",
                params![serde_json::to_vec(&header).unwrap()],
            )
            .unwrap();

        let refusal = if rename {
            session.set_name(Some("wrong name")).unwrap_err()
        } else {
            session.select_model(model("wrong model")).unwrap_err()
        };
        assert!(matches!(refusal, SessionError::JournalIdentityChanged));
        assert!(!refusal.is_commit_uncertain());
        assert_faulted(session.entry_count());
        assert_faulted(session.select_model(model("do not retry")));
        assert_eq!(read_header(&external).unwrap().name, header.name);
        let saved = Session::inspect(fixture.path()).unwrap();
        assert_eq!(saved.entries.len(), 1);
        assert_eq!(saved.last_model, Some(model("before")));
        drop(session);

        let next = Session::open(fixture.path()).unwrap();
        next.select_model(model("reconstructed")).unwrap();
        assert_eq!(next.entry_count().unwrap(), 2);
    }
}

fn artifact_error(error: std::io::Error) -> SessionError {
    *error
        .into_inner()
        .unwrap()
        .downcast::<SessionError>()
        .unwrap()
}

fn assert_fenced(session: &Arc<Session>, reference: &ArtifactRef) {
    assert_faulted(session.entry_count());
    assert_faulted(session.view());
    assert_faulted(session.status());
    assert_faulted(session.context_messages());
    assert_faulted(session.context_has_images());
    assert_faulted(session.model_context());
    assert_faulted(session.model_preference());
    assert_faulted(session.reasoning());
    assert_faulted(session.context_position());
    assert_faulted(session.recovery_state());
    assert_faulted(session.compaction_plan(None, 0, 1024, false, u64::MAX));
    assert_faulted(session.request_basis(&model("before")));
    assert_faulted(session.read_artifact(reference, 0, 1));
    assert_faulted(session.set_name(Some("must not write")));
    assert_faulted(session.select_model(model("must not write")));
    assert_faulted(session.begin_turn("must not accept".into(), model("after")));
    assert_faulted(session.clone_to(session.file_path().unwrap().with_file_name("clone.sqlite")));
    assert_faulted(session.fork_to(
        session.file_path().unwrap().with_file_name("fork.sqlite"),
        ForkPoint::BeforeTurn(1),
    ));
    let key = RequestKey::new("fixture".into(), "delivery".into()).unwrap();
    assert_faulted(session.request_receipt(&key));
    assert!(matches!(
        session.prepare_code_inspection(
            r#"{"kind":"parents"}"#,
            crate::ToolOccurrence {
                assistant_entry: 1,
                ordinal: 0
            },
        ),
        Err(code_inspection::InspectionError::Session(
            SessionError::StoreFaulted
        ))
    ));
    let error = session
        .artifact_sink()
        .capture("refused", b"no write")
        .unwrap_err();
    assert_faulted::<()>(Err(artifact_error(error)));
    assert!(
        !session
            .file_path()
            .unwrap()
            .with_file_name("clone.sqlite")
            .exists()
    );
    assert!(
        !session
            .file_path()
            .unwrap()
            .with_file_name("fork.sqlite")
            .exists()
    );
}

// Deferred foreign keys fail at real COMMIT, after every statement succeeded.
// This tests the phase boundary, not a simulated fsync/durability failure.
fn fail_commit(session: &Session, trigger: &str) {
    sql(session, &format!(
        "CREATE TABLE commit_parent (id INTEGER PRIMARY KEY);
         CREATE TABLE commit_child (parent INTEGER REFERENCES commit_parent(id) DEFERRABLE INITIALLY DEFERRED);
         CREATE TRIGGER fail_commit {trigger}
         BEGIN INSERT INTO commit_child(parent) VALUES (999); END;"
    ));
}

#[test]
fn statement_failure_rolls_back_partial_batch_preserves_projection_and_allows_retry() {
    let fixture = Fixture::new();
    let session = fixture.session();
    session.begin_turn("first".into(), model("before")).unwrap();
    let before = session.view().unwrap();
    let position = session.context_position().unwrap();
    sql(
        &session,
        "CREATE TRIGGER refuse_input BEFORE INSERT ON entries WHEN json_extract(NEW.body, '$.kind') = 'turn_started' BEGIN SELECT RAISE(ABORT, 'refused input'); END;",
    );

    // Recovery's TurnEnded is inserted before the new TurnStarted fails.
    let error = session
        .begin_turn("second".into(), model("after"))
        .unwrap_err();
    assert!(!error.is_commit_uncertain());
    assert!(
        matches!(error, SessionError::Sqlite(rusqlite::Error::SqliteFailure(_, Some(ref message))) if message == "refused input")
    );
    assert_eq!(session.view().unwrap().entries, before.entries);
    assert_eq!(session.context_messages().unwrap(), before.messages);
    assert_eq!(session.context_position().unwrap(), position);
    assert_eq!(session.entry_count().unwrap(), 1);
    assert_eq!(session.view().unwrap().unfinished_turn, Some(1));

    sql(&session, "DROP TRIGGER refuse_input");
    assert_eq!(
        session.begin_turn("second".into(), model("after")).unwrap(),
        (2, 0)
    );
    drop(session);
    let reopened = Session::open(fixture.path()).unwrap();
    assert_eq!(reopened.entry_count().unwrap(), 3);
    assert_eq!(reopened.view().unwrap().unfinished_turn, Some(2));
}

#[test]
fn validation_and_begin_failures_do_not_fault_store() {
    let fixture = Fixture::new();
    let session = fixture.session();
    let error = session
        .store()
        .lock()
        .unwrap()
        .append(&[SessionEntry::TurnEnded {
            turn: 1,
            reason: TurnEndReason::Completed,
        }])
        .unwrap_err();
    assert!(matches!(error, SessionError::InvalidHistory));
    assert!(!error.is_commit_uncertain());
    sql(&session, "BEGIN IMMEDIATE");
    let error = session.select_model(model("before")).unwrap_err();
    assert!(matches!(error, SessionError::Sqlite(_)));
    assert!(!error.is_commit_uncertain());
    assert_eq!(session.entry_count().unwrap(), 0);
    sql(&session, "ROLLBACK");
    session.select_model(model("after")).unwrap();
    assert_eq!(session.model_preference().unwrap(), Some(model("after")));
}

#[test]
fn commit_phase_failure_never_publishes_and_fences_until_reopen() {
    let fixture = Fixture::new();
    let session = Arc::new(fixture.session());
    session.select_model(model("before")).unwrap();
    let reference = session
        .artifact_sink()
        .capture("retained", b"evidence")
        .unwrap();
    let mut pending = session.artifact_sink().start("pending").unwrap();
    pending.write_all(b"must not flush").unwrap();
    let before = session.view().unwrap();
    fail_commit(&session, "AFTER INSERT ON entries");
    let error = session.select_model(model("after")).unwrap_err();
    assert!(error.is_commit_uncertain());
    assert!(
        matches!(error, SessionError::CommitUncertain { error, rollback_error: None } if matches!(*error, SessionError::Sqlite(_)))
    );
    {
        let store = session.store().lock().unwrap();
        assert_eq!(store.state.sequence, 1);
        assert_eq!(store.state.last_model, Some(model("before")));
        assert_eq!(read_entries(&store.connection).unwrap(), before.entries);
        assert_faulted(store.ensure_healthy());
    }
    assert_fenced(&session, &reference);
    let error = pending.finish(true).unwrap_err();
    assert_faulted::<()>(Err(artifact_error(error)));
    assert_eq!(
        Session::inspect(fixture.path()).unwrap().entries,
        before.entries
    );
    sql(&session, "DROP TRIGGER fail_commit");
    // Removing the fixture cannot heal this writer; only reopen reconstructs it.
    assert_faulted(session.entry_count());
    drop(session);
    let reopened = Session::open(fixture.path()).unwrap();
    assert_eq!(reopened.view().unwrap().entries, before.entries);
    reopened.select_model(model("after")).unwrap();
    assert_eq!(reopened.model_preference().unwrap(), Some(model("after")));
}

#[test]
fn unsuccessful_explicit_rollback_is_uncertain_even_after_sqlite_auto_rollback() {
    let fixture = Fixture::new();
    let session = fixture.session();
    sql(
        &session,
        "CREATE TRIGGER refuse_input BEFORE INSERT ON entries BEGIN SELECT RAISE(ROLLBACK, 'automatic rollback'); END;",
    );
    let error = session.select_model(model("after")).unwrap_err();
    assert!(error.is_commit_uncertain());
    assert!(
        matches!(error, SessionError::CommitUncertain { error, rollback_error: Some(_) } if matches!(*error, SessionError::Sqlite(rusqlite::Error::SqliteFailure(_, Some(ref message))) if message == "automatic rollback"))
    );
    assert_faulted(session.entry_count());
    sql(&session, "DROP TRIGGER refuse_input");
    drop(session);
    let reopened = Session::open(fixture.path()).unwrap();
    assert_eq!(reopened.entry_count().unwrap(), 0);
    reopened.select_model(model("after")).unwrap();
}

#[test]
fn header_chunk_and_receipt_commit_failures_share_classification_and_fencing() {
    for phase in ["header", "chunk", "receipt"] {
        let fixture = Fixture::new();
        let session = Arc::new(fixture.session());
        let trigger = match phase {
            "header" => "AFTER UPDATE ON session",
            "chunk" => "AFTER INSERT ON artifact_chunks",
            "receipt" => "AFTER INSERT ON artifacts WHEN NEW.receipt IS NOT NULL",
            _ => unreachable!(),
        };
        fail_commit(&session, trigger);
        let error = match phase {
            "header" => session.set_name(Some("uncertain")).unwrap_err(),
            "chunk" => SessionError::from(
                session
                    .artifact_sink()
                    .capture("chunk", b"data")
                    .unwrap_err(),
            ),
            "receipt" => {
                SessionError::from(session.artifact_sink().capture("empty", b"").unwrap_err())
            }
            _ => unreachable!(),
        };
        assert!(error.is_commit_uncertain(), "{phase}: {error}");
        assert_faulted(session.status());
        assert_faulted(session.set_name(Some("refused")));
        sql(&session, "DROP TRIGGER fail_commit");
        drop(session);
        let reopened = Arc::new(Session::open(fixture.path()).unwrap());
        assert_eq!(reopened.status().unwrap().name, None);
        reopened
            .artifact_sink()
            .capture("healthy", b"data")
            .unwrap();
        reopened.set_name(Some("healthy")).unwrap();
    }
}

#[tokio::test]
async fn async_artifact_errors_preserve_uncertainty_through_storage_and_io_wrappers() {
    for chunk in [true, false] {
        let fixture = Fixture::new();
        let session = Arc::new(fixture.session());
        fail_commit(
            &session,
            if chunk {
                "AFTER INSERT ON artifact_chunks"
            } else {
                "AFTER INSERT ON artifacts WHEN NEW.receipt IS NOT NULL"
            },
        );
        let mut writer = session.artifact_sink().start("async").unwrap();
        let error = if chunk {
            let error = writer.write_bytes(&vec![0; 256 * 1024]).await.unwrap_err();
            drop(writer);
            error
        } else {
            writer.finish_async(true).await.unwrap_err()
        };
        assert!(SessionError::from(error).is_commit_uncertain());
        assert_faulted(session.entry_count());
        session.shutdown().await.unwrap();
    }
}
