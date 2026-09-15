//! Session-level invariants that need durable pre-state or a storage fault.
//!
//! These live inside the crate because they are about the transaction
//! boundaries themselves: one arms a failing commit, the other starts a
//! process from a store that stopped between a provider answer and its
//! settlement. Neither is reachable from a client through the public API.

use std::path::PathBuf;
use std::sync::Arc;

use ion_ai::{
    Content, GenerationControls, Message, ModelRef, Reasoning, ResponseTermination, Role,
    ScriptedModelService, ToolChoice, Usage,
};

use super::{Services, Session};
use crate::SessionId;
use crate::attempt::AttemptState;
use crate::config::{ContextPolicy, ConversationConfig, RunLimits};
use crate::input::{InputBody, InputMode, InputSender};
use crate::limits::SessionLimits;
use crate::store::sqlite::InjectFault;
use crate::store::sqlite::conversation::CreateSession;
use crate::store::sqlite::input::{AdmitInput, Admitted};
use crate::store::sqlite::turn::{
    AttemptStart, BeginStep, CommitDispatch, CommitResponse, PrepareAttempt, StepStart,
};
use crate::store::{Db, SqliteStore};
use crate::tool::ToolRegistry;

fn limits() -> SessionLimits {
    SessionLimits {
        command_capacity: 16,
        ..SessionLimits::default()
    }
}

fn config() -> ConversationConfig {
    ConversationConfig {
        model: ModelRef {
            provider: "scripted".to_owned(),
            model: "test-model".to_owned(),
        },
        instructions: "be careful".to_owned(),
        controls: GenerationControls {
            max_output_tokens: 1024,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: false,
        },
        project_context: Vec::new(),
        tool_names: Vec::new(),
        context: ContextPolicy {
            max_request_bytes: 64 * 1024,
            max_input_tokens: 1_000,
        },
        limits: RunLimits {
            max_model_steps: 4,
            max_attempts_per_step: 2,
            max_cost_microusd: None,
            deadline_ms: 30_000,
            max_response_bytes: 64 * 1024,
            max_tool_output_bytes: 4 * 1024,
        },
    }
}

fn database(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ion-c1-unit-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("session.sqlite")
}

fn answer(text: &str) -> ion_ai::ModelResponse {
    ion_ai::ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::Text(text.to_owned())],
            provider_replay: None,
        },
        usage: Usage::known(1, 1),
        termination: ResponseTermination::Completed,
    }
}

#[tokio::test]
async fn a_commit_failure_publishes_nothing_and_fences_the_session() {
    let path = database("fault");
    let store = SqliteStore::create(&path).expect("create store");
    let db = Db::start(store, limits()).expect("start database thread");
    let info = db
        .run(CreateSession {
            session_id: SessionId::new(),
            config: config(),
        })
        .await
        .expect("create session");
    let after_create = sqlite_i64(&path, "SELECT last_seq FROM session_meta WHERE id = 1");

    // The next committing transaction fails after doing its work but before
    // publishing, which is the only interesting kind of failure.
    db.run(InjectFault).await.expect("arm the fault");
    let admitted = db
        .run(AdmitInput {
            conversation: info.root,
            sender: InputSender::User,
            mode: InputMode::Submit,
            request_key: None,
            body: InputBody::Text("hello".to_owned()),
            limits: limits(),
            now_unix_ms: now(),
        })
        .await;
    assert!(admitted.is_err(), "the transaction must fail: {admitted:?}");
    assert!(
        db.is_fenced(),
        "an unclear storage outcome fences the session"
    );
    db.close().await;

    // Nothing from the failed transaction is visible, and the identity space
    // did not move: an aborted commit consumes nothing.
    assert_eq!(sqlite_i64(&path, "SELECT COUNT(*) FROM inputs"), 0);
    assert_eq!(sqlite_i64(&path, "SELECT COUNT(*) FROM turns"), 0);
    assert_eq!(sqlite_i64(&path, "SELECT COUNT(*) FROM entries"), 0);
    assert_eq!(
        sqlite_i64(&path, "SELECT last_seq FROM session_meta WHERE id = 1"),
        after_create
    );
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn response_ready_evidence_is_settled_without_another_provider_call() {
    let path = database("response-ready");
    let conversation;
    let turn;
    {
        let store = SqliteStore::create(&path).expect("create store");
        let db = Db::start(store, limits()).expect("start database thread");
        let info = db
            .run(CreateSession {
                session_id: SessionId::new(),
                config: config(),
            })
            .await
            .expect("create session");
        let revision = info.last_commit.expect("create commit");
        conversation = info.root;
        let admitted = db
            .run(AdmitInput {
                conversation,
                sender: InputSender::User,
                mode: InputMode::Submit,
                request_key: None,
                body: InputBody::Text("hello".to_owned()),
                limits: limits(),
                now_unix_ms: now(),
            })
            .await
            .expect("admit");
        turn = match admitted {
            Admitted::Started { turn, .. } => turn,
            other => panic!("expected a started turn, got {other:?}"),
        };
        let step = match db
            .run(BeginStep {
                turn,
                config_revision: revision,
                model: config().model,
                instructions: config().instructions,
                context: Vec::new(),
                controls: config().controls,
                tools: Vec::new(),
                max_request_bytes: config().context.max_request_bytes,
                limits: limits(),
            })
            .await
            .expect("begin step")
        {
            StepStart::Started(step) => step,
            StepStart::Limit { setting } => panic!("unexpected limit {setting}"),
        };
        let attempt = match db
            .run(PrepareAttempt { step })
            .await
            .expect("prepare attempt")
        {
            AttemptStart::Started(attempt) => attempt,
            AttemptStart::Limit { setting } => panic!("unexpected limit {setting}"),
        };
        db.run(CommitDispatch {
            attempt,
            generation: 0,
        })
        .await
        .expect("dispatch");
        // The provider answered and the answer is durable. The process stops
        // here: exactly the window that must not cost a second request.
        db.run(CommitResponse {
            attempt,
            generation: 0,
            response: answer("recovered answer"),
        })
        .await
        .expect("record the response");
        db.close().await;
    }

    // Any provider call from here would panic, because the service has no
    // script left; that is what makes the assertion below meaningful.
    let model = Arc::new(ScriptedModelService::new([]));
    let session = Session::open(
        &path,
        limits(),
        Services::new(
            Arc::clone(&model) as Arc<dyn ion_ai::ModelService>,
            Arc::new(ToolRegistry::new()),
        ),
    )
    .await
    .expect("reopen");
    let handle = session.handle();
    assert_eq!(
        handle.resume(conversation).await.expect("resume"),
        Some(turn),
        "the unfinished turn is resumed"
    );
    let outcome = handle.wait(turn).await.expect("wait");
    assert!(
        outcome.is_completed(),
        "the stored answer settles the turn: {outcome:?}"
    );
    assert!(
        model.requests().is_empty(),
        "a durable response is never requested twice"
    );

    let view = handle.turn(turn).await.expect("view").expect("turn");
    assert_eq!(view.attempts[0].state, AttemptState::Settled);
    let page = handle
        .entries(crate::EntryQuery {
            conversation,
            after: None,
            limit: 8,
        })
        .await
        .expect("entries");
    assert_eq!(page.entries.len(), 2, "input and recovered answer");

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn a_dispatched_attempt_without_a_response_is_never_silently_repeated() {
    let path = database("dispatched");
    let conversation;
    let turn;
    {
        let store = SqliteStore::create(&path).expect("create store");
        let db = Db::start(store, limits()).expect("start database thread");
        let info = db
            .run(CreateSession {
                session_id: SessionId::new(),
                config: config(),
            })
            .await
            .expect("create session");
        let revision = info.last_commit.expect("create commit");
        conversation = info.root;
        let admitted = db
            .run(AdmitInput {
                conversation,
                sender: InputSender::User,
                mode: InputMode::Submit,
                request_key: None,
                body: InputBody::Text("hello".to_owned()),
                limits: limits(),
                now_unix_ms: now(),
            })
            .await
            .expect("admit");
        turn = match admitted {
            Admitted::Started { turn, .. } => turn,
            other => panic!("expected a started turn, got {other:?}"),
        };
        let step = match db
            .run(BeginStep {
                turn,
                config_revision: revision,
                model: config().model,
                instructions: config().instructions,
                context: Vec::new(),
                controls: config().controls,
                tools: Vec::new(),
                max_request_bytes: config().context.max_request_bytes,
                limits: limits(),
            })
            .await
            .expect("begin step")
        {
            StepStart::Started(step) => step,
            StepStart::Limit { setting } => panic!("unexpected limit {setting}"),
        };
        let attempt = match db
            .run(PrepareAttempt { step })
            .await
            .expect("prepare attempt")
        {
            AttemptStart::Started(attempt) => attempt,
            AttemptStart::Limit { setting } => panic!("unexpected limit {setting}"),
        };
        db.run(CommitDispatch {
            attempt,
            generation: 0,
        })
        .await
        .expect("dispatch");
        db.close().await;
    }

    let model = Arc::new(ScriptedModelService::new([ion_ai::Script::Stream(vec![
        ion_ai::ModelStreamEvent::Completed(answer("after recovery")),
    ])]));
    let session = Session::open(
        &path,
        limits(),
        Services::new(
            Arc::clone(&model) as Arc<dyn ion_ai::ModelService>,
            Arc::new(ToolRegistry::new()),
        ),
    )
    .await
    .expect("reopen");
    let handle = session.handle();
    let resumed = handle.resume(conversation).await.expect("resume");
    assert_eq!(resumed, Some(turn));
    let outcome = handle.wait(turn).await.expect("wait");
    assert!(outcome.is_completed(), "got {outcome:?}");

    // One new physical attempt was made, and the unknown earlier one is
    // preserved as evidence rather than erased or reinterpreted.
    let view = handle.turn(turn).await.expect("view").expect("turn");
    let states: Vec<_> = view.attempts.iter().map(|attempt| attempt.state).collect();
    assert!(
        states.contains(&AttemptState::Indeterminate),
        "the unresolved attempt stays visible: {states:?}"
    );
    assert_eq!(model.requests().len(), 1, "exactly one retry");

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

fn sqlite_i64(path: &std::path::Path, sql: &str) -> i64 {
    let connection = rusqlite::Connection::open(path).expect("open raw");
    connection
        .query_row(sql, [], |row| row.get(0))
        .expect("query")
}

/// The seed helpers write real durable state, so they stamp a real clock: a
/// turn admitted at epoch zero is legitimately past its deadline.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}
