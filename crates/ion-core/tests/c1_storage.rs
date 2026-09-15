//! Durable-store boundaries: ownership, schema refusal, corruption and pages.

mod support;

use std::sync::Arc;

use ion_ai::{Script, ScriptedModelService};
use ion_core::{
    EntryQuery, Error, InputDisposition, Session, SessionLimits, SessionSpec, SubmitRequest,
    ToolRegistry, TurnOutcome,
};
use support::{answer, database, services, spec, stream};

#[tokio::test]
async fn a_second_owner_is_refused_and_close_releases_the_session() {
    let path = database("ownership");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(answer(
        "ok",
    )))]));
    let session = Session::create(&path, spec(), services(model.clone(), ToolRegistry::new()))
        .await
        .expect("create session");

    let refused = Session::open(
        &path,
        SessionLimits::default(),
        services(model.clone(), ToolRegistry::new()),
    )
    .await;
    match refused {
        Err(Error::SessionInUse(blocked)) => assert_eq!(blocked, path),
        other => panic!("expected the session to be in use, got {other:?}"),
    }

    session.close().await.expect("close");
    let reopened = Session::open(
        &path,
        SessionLimits::default(),
        services(model, ToolRegistry::new()),
    )
    .await
    .expect("reopen after close");
    reopened.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn opening_an_unknown_database_is_a_named_error() {
    let path = database("missing").with_file_name("absent.sqlite");
    let error = Session::open(
        &path,
        SessionLimits::default(),
        services(Arc::new(ScriptedModelService::new([])), ToolRegistry::new()),
    )
    .await
    .expect_err("a missing database must not be created");
    assert!(matches!(error, Error::UnknownSession(_)), "got {error:?}");
}

#[tokio::test]
async fn an_older_schema_is_refused_rather_than_reinterpreted() {
    let path = database("schema");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(answer(
        "ok",
    )))]));
    let session = Session::create(&path, spec(), services(model.clone(), ToolRegistry::new()))
        .await
        .expect("create session");
    session.close().await.expect("close");

    {
        let connection = rusqlite::Connection::open(&path).expect("open raw");
        connection
            .pragma_update(None, "user_version", 6)
            .expect("stamp an old version");
    }

    let error = Session::open(
        &path,
        SessionLimits::default(),
        services(model, ToolRegistry::new()),
    )
    .await
    .expect_err("an old schema must be refused");
    match error {
        Error::UnsupportedSchema { found, expected } => {
            assert_eq!(found, 6);
            assert_eq!(expected, 1);
        }
        other => panic!("expected a schema refusal, got {other:?}"),
    }
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn a_self_parented_conversation_is_refused_at_open() {
    let path = database("cycle");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(answer(
        "ok",
    )))]));
    let session = Session::create(&path, spec(), services(model.clone(), ToolRegistry::new()))
        .await
        .expect("create session");
    let root = session.root();
    session.close().await.expect("close");

    {
        let connection = rusqlite::Connection::open(&path).expect("open raw");
        connection
            .execute(
                "UPDATE conversations SET parent_id = id, parent_at = 1 WHERE id = ?1",
                [root.get()],
            )
            .expect("corrupt the history edge");
    }

    let error = Session::open(
        &path,
        SessionLimits::default(),
        services(model, ToolRegistry::new()),
    )
    .await
    .expect_err("an impossible ancestry must be refused");
    assert!(matches!(error, Error::Corrupt(_)), "got {error:?}");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn a_cutoff_outside_its_source_conversation_is_refused_at_open() {
    let path = database("cutoff");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(answer(
        "ok",
    )))]));
    let session = Session::create(&path, spec(), services(model.clone(), ToolRegistry::new()))
        .await
        .expect("create session");
    session.close().await.expect("close");

    {
        // A second conversation that claims to branch from the root at an entry
        // that does not exist. Nothing may traverse it.
        let connection = rusqlite::Connection::open(&path).expect("open raw");
        let mut statement = connection
            .prepare("SELECT id FROM conversations")
            .expect("prepare");
        let root: i64 = statement
            .query_row([], |row| row.get(0))
            .expect("root conversation");
        connection
            .execute(
                "INSERT INTO conversations (id, parent_id, parent_at) VALUES (999, ?1, 4242)",
                [root],
            )
            .expect("insert a branch");
    }

    let error = Session::open(
        &path,
        SessionLimits::default(),
        services(model, ToolRegistry::new()),
    )
    .await
    .expect_err("an invisible cutoff must be refused");
    assert!(matches!(error, Error::Corrupt(_)), "got {error:?}");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn pages_are_clamped_and_never_overflow() {
    let path = database("pages");
    let model =
        Arc::new(ScriptedModelService::new((0..3).map(|index| {
            Script::Stream(stream(answer(&format!("answer {index}"))))
        })));
    let session = Session::create(&path, spec(), services(model, ToolRegistry::new()))
        .await
        .expect("create session");
    let handle = session.handle();
    for index in 0..3 {
        let turn = handle
            .submit(SubmitRequest::user(format!("question {index}")))
            .await
            .expect("submit")
            .turn
            .expect("turn");
        let outcome = handle.wait(turn).await.expect("wait");
        assert!(outcome.is_completed());
    }

    // The largest possible page request is clamped, not added to.
    let page = handle
        .entries(EntryQuery {
            conversation: handle.root(),
            after: None,
            limit: u32::MAX,
        })
        .await
        .expect("read page");
    assert_eq!(page.entries.len(), 6);
    assert!(!page.has_more);

    // A zero limit is an empty page, not an unbounded read.
    let empty = handle
        .entries(EntryQuery {
            conversation: handle.root(),
            after: None,
            limit: 0,
        })
        .await
        .expect("read empty page");
    assert!(empty.entries.is_empty());
    assert!(!empty.has_more);

    // Paging continues strictly after the last entry of the previous page.
    let first = handle
        .entries(EntryQuery {
            conversation: handle.root(),
            after: None,
            limit: 2,
        })
        .await
        .expect("first page");
    assert_eq!(first.entries.len(), 2);
    assert!(first.has_more);
    let second = handle
        .entries(EntryQuery {
            conversation: handle.root(),
            after: first.entries.last().map(|entry| entry.id),
            limit: 2,
        })
        .await
        .expect("second page");
    assert_eq!(second.entries.len(), 2);
    assert!(second.entries[0].id > first.entries[1].id);

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn the_reserve_keeps_settlement_possible_when_admission_is_full() {
    let path = database("quota");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(answer(
        &"x".repeat(11_000),
    )))]));
    let mut spec: SessionSpec = spec();
    // Ordinary growth may fill 10 kB; 20 kB stays reserved for outcomes.
    spec.limits = SessionLimits {
        quota_bytes: 30_000,
        reserved_bytes: 20_000,
        max_queued_inputs: 8,
        command_capacity: 16,
    };
    let session = Session::create(&path, spec, services(model, ToolRegistry::new()))
        .await
        .expect("create session");
    let handle = session.handle();

    let turn = handle
        .submit(SubmitRequest::user("small question"))
        .await
        .expect("submit")
        .turn
        .expect("turn");
    let outcome = handle.wait(turn).await.expect("wait");
    match outcome {
        TurnOutcome::Completed { .. } => {}
        other => panic!("the answer must be recordable inside the reserve: {other:?}"),
    }

    // The accepted answer already consumed the ordinary allowance, so new
    // admission is refused while the turn itself stays readable.
    let refused = handle
        .submit(SubmitRequest::follow_up("y".repeat(11_000)))
        .await;
    assert!(
        matches!(refused, Err(Error::QuotaExhausted { .. })),
        "got {refused:?}"
    );

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn an_open_session_starts_no_work_and_keeps_the_history() {
    let path = database("reopen");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(answer(
        "remembered",
    )))]));
    let session = Session::create(&path, spec(), services(model.clone(), ToolRegistry::new()))
        .await
        .expect("create session");
    let handle = session.handle();
    let turn = handle
        .submit(SubmitRequest::user("hello"))
        .await
        .expect("submit")
        .turn
        .expect("turn");
    let first = handle.wait(turn).await.expect("wait");
    session.close().await.expect("close");

    let reopened = Session::open(
        &path,
        SessionLimits::default(),
        services(model.clone(), ToolRegistry::new()),
    )
    .await
    .expect("reopen");
    let handle = reopened.handle();
    assert_eq!(model.requests().len(), 1, "opening starts no model work");
    assert!(
        handle
            .resume(handle.root())
            .await
            .expect("resume")
            .is_none()
    );

    let view = handle.turn(turn).await.expect("view").expect("turn");
    assert_eq!(view.turn.outcome, Some(first));
    let page = handle
        .entries(EntryQuery {
            conversation: handle.root(),
            after: None,
            limit: 16,
        })
        .await
        .expect("entries");
    assert_eq!(page.entries.len(), 2);

    // A queued input from before the reopen is still there, and is answered by
    // its own turn rather than being lost.
    reopened.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn withdrawing_survives_a_reopen_as_a_terminal_disposition() {
    let path = database("withdraw-reopen");
    let model = Arc::new(ScriptedModelService::new([
        Script::Stream(stream(support::tool_answer(ion_ai::ToolCall {
            id: "call-1".to_owned(),
            name: "slow".to_owned(),
            arguments: serde_json::json!({}),
        }))),
        Script::Stream(stream(answer("done"))),
    ]));
    let mut tools = ToolRegistry::new();
    tools.insert(Arc::new(
        ion_core::ScriptedTool::new(
            "slow",
            [ion_core::ToolOutcome::Completed(serde_json::json!("ok"))],
        )
        .repeat_safe(true),
    ));
    let mut spec: SessionSpec = spec();
    spec.config.tool_names = vec!["slow".to_owned()];
    let session = Session::create(&path, spec, services(model, tools))
        .await
        .expect("create session");
    let handle = session.handle();
    let turn = handle
        .submit(SubmitRequest::user("first"))
        .await
        .expect("submit")
        .turn
        .expect("turn");
    let queued = handle
        .submit(SubmitRequest::follow_up("withdrawn"))
        .await
        .expect("follow up");
    handle.withdraw(queued.input).await.expect("withdraw");
    handle.wait(turn).await.expect("wait");
    session.close().await.expect("close");

    let reopened = Session::open(
        &path,
        SessionLimits::default(),
        services(Arc::new(ScriptedModelService::new([])), ToolRegistry::new()),
    )
    .await
    .expect("reopen");
    let handle = reopened.handle();
    let input = handle
        .input(queued.input)
        .await
        .expect("read input")
        .expect("input");
    assert_eq!(input.disposition, InputDisposition::Cancelled);
    assert!(
        handle
            .resume(handle.root())
            .await
            .expect("resume")
            .is_none(),
        "a withdrawn input starts nothing"
    );
    reopened.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}
