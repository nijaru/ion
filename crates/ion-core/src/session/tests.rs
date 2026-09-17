//! Session-level invariants that need durable pre-state or a storage fault.
//!
//! These live inside the crate because they are about the transaction
//! boundaries themselves: one arms a failing commit, the other starts a
//! process from a store that stopped between a provider answer and its
//! settlement. Neither is reachable from a client through the public API.

use std::path::PathBuf;
use std::sync::Arc;

use ion_ai::{
    Content, GenerationControls, Message, ModelRef, ModelStreamEvent, Reasoning,
    ResponseTermination, Role, Script, ScriptedModelService, ToolCall, ToolChoice, Usage,
};

use super::{Services, Session};
use crate::SessionId;
use crate::attempt::AttemptState;
use crate::config::{ContextPolicy, ConversationConfig, RunLimits};
use crate::input::{InputBody, InputMode, InputSender};
use crate::invocation::InvocationState;
use crate::limits::SessionLimits;
use crate::store::sqlite::InjectFault;
use crate::store::sqlite::conversation::CreateSession;
use crate::store::sqlite::input::{AdmitInput, Admitted};
use crate::store::sqlite::turn::{
    AdmittedCall, AttemptStart, BeginStep, CommitDispatch, CommitInvocationDispatch,
    CommitResponse, PrepareAttempt, SettleAttempt, StepStart,
};
use crate::store::{Db, SqliteStore};
use crate::tool::{ScriptedTool, ToolOutcome, ToolRegistry};
use crate::turn::TurnPhase;
use crate::{ConversationId, InvocationId, TurnId};

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
    let mut session = Session::open(
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
async fn a_retryable_failure_after_a_reused_attempt_still_retries() {
    let path = database("retry-reused-attempt");
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
        // The process stops between the attempt record and its dispatch intent:
        // exactly the window a later retry has to survive.
        db.run(PrepareAttempt { step })
            .await
            .expect("prepare attempt");
        db.close().await;
    }

    // The first provider call fails retryably, so the drive retries. Reusing the
    // attempt it just dispatched would dispatch one attempt twice.
    let model = Arc::new(ScriptedModelService::new([
        Script::OpenError(ion_ai::ProviderError {
            kind: ion_ai::ProviderErrorKind::Transport,
            message: "connection reset".to_owned(),
        }),
        Script::Stream(vec![ModelStreamEvent::Completed(answer("retried answer"))]),
    ]));
    let mut session = Session::open(
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
        Some(turn)
    );
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), handle.wait(turn))
        .await
        .expect("the retry must not stall the turn")
        .expect("wait");
    assert!(
        outcome.is_completed(),
        "the retry must produce a new request: {outcome:?}"
    );
    assert_eq!(model.requests().len(), 2, "one failure and one retry");

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
    let mut session = Session::open(
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

/// Seed a turn whose provider answer requested one tool call.
///
/// The caller chooses whether dispatch intent is committed, which is exactly
/// the window a crash can land in and the one no client can create.
async fn seed_admitted_call(
    db: &Db,
    implementation: &str,
    dispatch: bool,
) -> (ConversationId, TurnId, InvocationId) {
    let info = db
        .run(CreateSession {
            session_id: SessionId::new(),
            config: config(),
        })
        .await
        .expect("create session");
    let revision = info.last_commit.expect("create commit");
    let conversation = info.root;
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
    let turn = match admitted {
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
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "write".to_owned(),
        arguments: serde_json::json!({"path": "notes.txt"}),
    };
    db.run(CommitResponse {
        attempt,
        generation: 0,
        response: ion_ai::ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(call.clone())],
                provider_replay: None,
            },
            usage: Usage::known(1, 1),
            termination: ResponseTermination::Completed,
        },
    })
    .await
    .expect("record the response");
    let settlement = db
        .run(SettleAttempt {
            attempt,
            calls: vec![AdmittedCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                implementation: implementation.to_owned(),
                repeat_safe: false,
            }],
            limits: limits(),
        })
        .await
        .expect("settle the attempt");
    let invocation = match settlement {
        crate::store::sqlite::turn::Settlement::Tools { invocations, .. } => invocations[0],
        other => panic!("expected admitted calls, got {other:?}"),
    };
    if dispatch {
        // The process that owned the action is about to be lost: the dispatch
        // intent is durable and nothing has reported an outcome.
        db.run(CommitInvocationDispatch {
            invocation,
            generation: 0,
        })
        .await
        .expect("commit dispatch intent");
    }
    (conversation, turn, invocation)
}

/// Poll until the turn parks, or fail after a bounded wait.
async fn parked(handle: &crate::SessionHandle, turn: TurnId) -> crate::TurnView {
    for _ in 0..300 {
        if let Some(view) = handle.turn(turn).await.expect("view")
            && matches!(view.turn.phase, TurnPhase::Blocked { .. })
        {
            return view;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the turn never parked");
}

fn registry(tool: std::sync::Arc<ScriptedTool>) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.insert(tool);
    tools
}

#[tokio::test]
async fn a_dispatched_action_without_an_executor_parks_instead_of_repeating() {
    let path = database("dispatched-action");
    let (conversation, turn, invocation);
    {
        let store = SqliteStore::create(&path).expect("create store");
        let db = Db::start(store, limits()).expect("start database thread");
        (conversation, turn, invocation) = seed_admitted_call(&db, "write@scripted-1", true).await;
        db.close().await;
    }

    // Any provider call would panic: the model has no script left.
    let model = Arc::new(ScriptedModelService::new([]));
    let tool = std::sync::Arc::new(ScriptedTool::new(
        "write",
        [ToolOutcome::Completed(serde_json::json!({"ok": true}))],
    ));
    let mut session = Session::open(
        &path,
        limits(),
        Services::new(
            Arc::clone(&model) as Arc<dyn ion_ai::ModelService>,
            Arc::new(registry(tool.clone())),
        ),
    )
    .await
    .expect("reopen");
    let handle = session.handle();
    assert_eq!(
        handle.resume(conversation).await.expect("resume"),
        Some(turn)
    );

    // The action may have happened, so it is never repeated on the strength of
    // a missing executor: the exchange parks for a client decision.
    let blocked = parked(&handle, turn).await;
    assert_eq!(blocked.invocations[0].id, invocation);
    assert_eq!(blocked.invocations[0].state, InvocationState::Indeterminate);
    assert!(blocked.turn.outcome.is_none());
    assert!(
        tool.calls().is_empty(),
        "a dispatched action is not repeated"
    );
    assert!(model.requests().is_empty(), "no model step ran");

    // A parked action does not keep the session open: nothing is executing.
    assert!(session.close().await.expect("close").is_closed());
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn a_prepared_call_is_dispatched_after_a_restart() {
    let path = database("prepared-action");
    let (conversation, turn);
    {
        let store = SqliteStore::create(&path).expect("create store");
        let db = Db::start(store, limits()).expect("start database thread");
        let (seeded_conversation, seeded_turn, _) =
            seed_admitted_call(&db, "write@scripted-1", false).await;
        conversation = seeded_conversation;
        turn = seeded_turn;
        db.close().await;
    }

    // The recorded implementation is present, so the call was never dispatched
    // and may run under current policy.
    let tool = std::sync::Arc::new(ScriptedTool::new(
        "write",
        [ToolOutcome::Completed(serde_json::json!({"ok": true}))],
    ));
    let model = Arc::new(ScriptedModelService::new([Script::Stream(vec![
        ModelStreamEvent::Completed(answer("done")),
    ])]));
    let mut session = Session::open(
        &path,
        limits(),
        Services::new(
            Arc::clone(&model) as Arc<dyn ion_ai::ModelService>,
            Arc::new(registry(tool.clone())),
        ),
    )
    .await
    .expect("reopen");
    let handle = session.handle();
    assert_eq!(
        handle.resume(conversation).await.expect("resume"),
        Some(turn)
    );
    let outcome = handle.wait(turn).await.expect("wait");
    assert!(outcome.is_completed(), "got {outcome:?}");
    assert_eq!(tool.calls().len(), 1, "the prepared call runs once");
    assert_eq!(
        model.requests().len(),
        1,
        "one model step after the restart"
    );

    let view = handle.turn(turn).await.expect("view").expect("turn");
    assert!(matches!(
        view.invocations[0].state,
        InvocationState::Succeeded { .. }
    ));

    assert!(session.close().await.expect("close").is_closed());
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn a_changed_implementation_cannot_reinterpret_a_prepared_call() {
    let path = database("changed-implementation");
    let (conversation, turn);
    {
        let store = SqliteStore::create(&path).expect("create store");
        let db = Db::start(store, limits()).expect("start database thread");
        let (seeded_conversation, seeded_turn, _) =
            seed_admitted_call(&db, "write@revision-2", false).await;
        conversation = seeded_conversation;
        turn = seeded_turn;
        db.close().await;
    }

    // The registry has a tool with the same name and different implementation.
    // It is not allowed to run arguments that were prepared for another one.
    let tool = std::sync::Arc::new(ScriptedTool::new(
        "write",
        [ToolOutcome::Completed(serde_json::json!({"ok": true}))],
    ));
    let model = Arc::new(ScriptedModelService::new([
        Script::Stream(vec![ModelStreamEvent::Completed(answer("noted"))]),
        Script::Stream(vec![ModelStreamEvent::Completed(answer("finished"))]),
    ]));
    let mut session = Session::open(
        &path,
        limits(),
        Services::new(
            Arc::clone(&model) as Arc<dyn ion_ai::ModelService>,
            Arc::new(registry(tool.clone())),
        ),
    )
    .await
    .expect("reopen");
    let handle = session.handle();
    assert_eq!(
        handle.resume(conversation).await.expect("resume"),
        Some(turn)
    );
    let outcome = handle.wait(turn).await.expect("wait");
    assert!(outcome.is_completed(), "got {outcome:?}");
    assert!(
        tool.calls().is_empty(),
        "a replacement implementation never runs prepared arguments"
    );

    // The failure is a truthful fact the model can read, and it names the
    // implementation the arguments were prepared for.
    let view = handle.turn(turn).await.expect("view").expect("turn");
    match &view.invocations[0].state {
        InvocationState::Failed { message } => {
            assert!(message.contains("write@revision-2"), "got {message:?}");
        }
        other => panic!("expected a known failure, got {other:?}"),
    }
    let page = handle
        .entries(crate::EntryQuery {
            conversation,
            after: None,
            limit: 8,
        })
        .await
        .expect("entries");
    let recorded = serde_json::to_string(&page.entries).expect("encode");
    assert!(recorded.contains("write@revision-2"), "got {recorded}");

    assert!(session.close().await.expect("close").is_closed());
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
