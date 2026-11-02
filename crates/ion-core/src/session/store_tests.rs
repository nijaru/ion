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
    assert_faulted(session.clone_to(session.path().with_file_name("clone.sqlite")));
    assert_faulted(session.fork_to(
        session.path().with_file_name("fork.sqlite"),
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
    assert!(!session.path().with_file_name("clone.sqlite").exists());
    assert!(!session.path().with_file_name("fork.sqlite").exists());
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
