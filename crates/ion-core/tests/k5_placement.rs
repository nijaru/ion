//! K5/R4: placement is independent of answer success.
//!
//! An accepted input is placed in the transcript when the session binds it to the
//! turn that will answer it, before any invocation runs. These tests cover what
//! that buys: a cancelled, failed or interrupted answer leaves exactly one placed
//! user entry, an explicit retry answers the same placed input instead of
//! admitting it twice, and an abandoned input keeps its placement.

use std::sync::Arc;

use ion_ai::{
    Content, Message, ModelRef, ModelStreamEvent, ResponseTermination, Role, Script,
    ScriptedModelService, Usage,
};
use ion_core::builtin::Builtins;
use ion_core::{
    CloseMode, ConversationId, DriveOutcome, InputBody, InputMode, InputPlacement, InputRequest,
    InputSender, RequestKey, Session, TaskDriver, TaskKindName, TaskOutcomeKind, TaskRegistry,
    TaskRequest, TaskStatus,
};
use serde_json::json;

mod support;

use support::TempDb;

fn kind(name: &str) -> TaskKindName {
    TaskKindName::new(name).expect("task kind")
}

fn model_ref() -> ModelRef {
    ModelRef {
        provider: "test".to_owned(),
        model: "scripted".to_owned(),
    }
}

fn completed(text: &str) -> ModelStreamEvent {
    ModelStreamEvent::Completed(ion_ai::ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::Text(text.to_owned())],
            provider_replay: None,
        },
        usage: Usage::known(10, 4),
        termination: ResponseTermination::Completed,
    })
}

fn incomplete(text: &str) -> ModelStreamEvent {
    ModelStreamEvent::TextDelta(text.to_owned())
}

fn driver(scripts: impl IntoIterator<Item = Script>) -> (TaskDriver, Arc<ScriptedModelService>) {
    let service = Arc::new(ScriptedModelService::new(scripts));
    let mut registry = TaskRegistry::new();
    Builtins {
        model: model_ref(),
        service: service.clone(),
        tools: Arc::new(ion_core::builtin::ToolCatalog::new()),
    }
    .register(&mut registry)
    .expect("register built-ins");
    (
        TaskDriver::new(Session::new().expect("session"), registry),
        service,
    )
}

fn registry(scripts: impl IntoIterator<Item = Script>) -> TaskRegistry {
    let service = Arc::new(ScriptedModelService::new(scripts));
    let mut registry = TaskRegistry::new();
    Builtins {
        model: model_ref(),
        service,
        tools: Arc::new(ion_core::builtin::ToolCatalog::new()),
    }
    .register(&mut registry)
    .expect("register built-ins");
    registry
}

fn durable(scripts: impl IntoIterator<Item = Script>, db: &std::path::Path) -> TaskDriver {
    TaskDriver::create(db, registry(scripts)).expect("create")
}

fn reopen(db: &std::path::Path, scripts: impl IntoIterator<Item = Script>) -> TaskDriver {
    TaskDriver::open(db, registry(scripts)).expect("reopen")
}

fn submission(target: ConversationId, key: &str, text: &str) -> InputRequest {
    InputRequest {
        target,
        sender: InputSender::User,
        mode: InputMode::Submit,
        request_key: Some(RequestKey::new(key).expect("request key")),
        body: InputBody::Text(text.to_owned()),
    }
}

fn turn_request(target: ConversationId) -> TaskRequest {
    TaskRequest {
        conversation_id: target,
        kind: kind(ion_core::builtin::GENERATION),
        schema_version: 1,
        input: json!({}),
        dependencies: Vec::new(),
    }
}

async fn transcript(driver: &TaskDriver, root: ConversationId) -> Vec<(String, String)> {
    driver
        .conversation_entries(root, None, 16)
        .await
        .expect("transcript")
        .entries
        .iter()
        .map(|entry| {
            (
                entry.kind.as_str().to_owned(),
                entry.data["text"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

fn placement(snapshot: &ion_core::SessionSnapshot, input: ion_core::InputId) -> InputPlacement {
    snapshot
        .inputs
        .iter()
        .find(|candidate| candidate.id == input)
        .expect("admitted input")
        .disposition
        .placement()
        .expect("a placed input")
}

#[tokio::test]
async fn cancelling_before_the_first_token_still_places_the_input() {
    let (driver, service) = driver([Script::Stream(vec![completed("never sent")])]);
    let root = driver.snapshot().await.root_conversation;
    let submitted = driver
        .submit_input(submission(root, "turn-1", "answer me"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("turn root");

    // The turn never runs: it is cancelled while still pending, and its abort
    // invocation settles it. The accepted input remains placed.
    let cancellation = driver.cancel_turn(turn).await.expect("cancel");
    assert_eq!(cancellation.cancelled, vec![turn]);
    // Cancellation drives the abort invocation itself; the client only observes
    // the outcome.
    let record = tokio::time::timeout(std::time::Duration::from_secs(10), driver.wait_task(turn))
        .await
        .expect("no stall")
        .expect("the aborted turn settles");
    let TaskStatus::Terminal(outcome) = record.status else {
        panic!("the aborted turn is terminal");
    };
    assert_eq!(outcome.kind, TaskOutcomeKind::Aborted);

    assert_eq!(
        transcript(&driver, root).await,
        vec![("user".to_owned(), "answer me".to_owned())],
        "the accepted message is history even though no answer was attempted"
    );
    let snapshot = driver.snapshot().await;
    assert_eq!(placement(&snapshot, submitted.input_id).turn, turn);
    assert!(
        service.requests().is_empty(),
        "cancellation before the first token must not call the provider"
    );
}

#[tokio::test]
async fn an_interrupted_attempt_places_the_input_once() {
    let db = TempDb::new("r4-interrupted");
    let driver = durable(
        [
            Script::Stream(vec![incomplete("partial")]),
            Script::Stream(vec![completed("recovered")]),
        ],
        db.path(),
    );
    let root = driver.snapshot().await.root_conversation;
    let submitted = driver
        .submit_input(submission(root, "turn-1", "hello"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("turn root");
    let DriveOutcome::Settled(settlement) = driver.drive_task(turn).await.expect("drive") else {
        panic!("an end-of-stream answer settles as a failure");
    };
    assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Failed);
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    // A handover must not place the same accepted input again.
    let reopened = reopen(db.path(), [Script::Stream(vec![completed("second")])]);
    let snapshot = reopened.snapshot().await;
    assert_eq!(placement(&snapshot, submitted.input_id).turn, turn);
    assert_eq!(
        snapshot
            .entries
            .iter()
            .filter(|entry| entry.kind.as_str() == "user")
            .count(),
        1,
        "placement is committed once, not once per process"
    );
    assert_eq!(
        snapshot
            .entries
            .iter()
            .filter(|entry| entry.kind.as_str() == "assistant")
            .count(),
        0,
        "the incomplete answer was never appended"
    );
}

#[tokio::test]
async fn a_failed_attempt_can_be_retried_after_reopen_without_a_second_entry() {
    let db = TempDb::new("r4-retry");
    let driver = durable(
        [
            Script::Stream(vec![incomplete("partial")]),
            Script::Stream(vec![completed("retried")]),
        ],
        db.path(),
    );
    let root = driver.snapshot().await.root_conversation;
    let submitted = driver
        .submit_input(submission(root, "turn-1", "hello"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("turn root");
    driver.drive_task(turn).await.expect("drive");
    let closed_by = driver.turn_closed_by(turn).await.expect("closure receipt");
    let placed = placement(&driver.snapshot().await, submitted.input_id);
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    // An explicit retry answers the same placed input: no second admission, no
    // second user message, and a genuinely new attempt.
    let reopened = reopen(db.path(), [Script::Stream(vec![completed("retried")])]);
    let retry = reopened
        .retry_input(submitted.input_id, turn, turn_request(root))
        .await
        .expect("retry");
    assert_ne!(retry, turn, "a retry is a new attempt");
    let snapshot = reopened.snapshot().await;
    assert_eq!(
        placement(&snapshot, submitted.input_id),
        InputPlacement {
            entry: placed.entry,
            turn: retry,
        },
        "the retry answers the same entry"
    );
    assert_eq!(
        reopened.turn_closed_by(turn).await,
        Some(closed_by),
        "the failed attempt's closure receipt is preserved"
    );

    let request = reopened
        .snapshot()
        .await
        .tasks
        .iter()
        .find(|task| task.id == retry)
        .expect("retry task")
        .id;
    let DriveOutcome::Settled(settlement) =
        reopened.drive_task(request).await.expect("drive retry")
    else {
        panic!("the retry settles");
    };
    assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Completed);
    assert_eq!(
        transcript(&reopened, root).await,
        vec![
            ("user".to_owned(), "hello".to_owned()),
            ("assistant".to_owned(), "retried".to_owned()),
        ],
        "exactly one user entry, one answer"
    );
    reopened.close(CloseMode::Graceful).await;
}

#[tokio::test]
async fn an_abandoned_input_keeps_its_placement_and_can_still_be_retried() {
    let (driver, _service) = driver([Script::Stream(vec![incomplete("partial")])]);
    let root = driver.snapshot().await.root_conversation;
    let submitted = driver
        .submit_input(submission(root, "turn-1", "hello"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("turn root");
    driver.drive_task(turn).await.expect("drive");
    let placed = placement(&driver.snapshot().await, submitted.input_id);

    driver
        .abandon_input(submitted.input_id, turn)
        .await
        .expect("abandon");
    let snapshot = driver.snapshot().await;
    let disposition = snapshot
        .inputs
        .iter()
        .find(|input| input.id == submitted.input_id)
        .expect("input")
        .disposition;
    assert_eq!(
        disposition,
        ion_core::InputDisposition::Abandoned(placed),
        "abandonment ends the answer intent, not the accepted message"
    );
    assert_eq!(
        transcript(&driver, root).await,
        vec![("user".to_owned(), "hello".to_owned())]
    );

    // Retrying an abandoned input is still possible: the placement was kept.
    let retry = driver
        .retry_input(submitted.input_id, turn, turn_request(root))
        .await
        .expect("retry an abandoned input");
    assert_eq!(
        placement(&driver.snapshot().await, submitted.input_id).turn,
        retry
    );
}

/// A retired conversation keeps its placed input and refuses a new attempt: the
/// placement is archival, the answer intent is not.
#[tokio::test]
async fn a_retired_conversation_refuses_a_retry() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let owner = session
        .create_task(turn_request(root))
        .expect("owner task")
        .task_id;
    let worker = session
        .create_conversation(ion_core::ConversationSpec {
            parent: None,
            owner_task: Some(owner),
        })
        .expect("worker conversation")
        .conversation_id;
    let submitted = session
        .admit_input(
            submission(worker, "turn-1", "hello"),
            Some(turn_request(worker)),
        )
        .expect("admit");
    let turn = submitted.task_id.expect("turn root");
    let driver = TaskDriver::new(session, registry([Script::Stream(vec![completed("x")])]));
    driver.drive_task(turn).await.expect("drive");
    let placed = placement(&driver.snapshot().await, submitted.input_id);

    driver.retire_conversation(worker).await.expect("retire");
    let refused = driver
        .retry_input(submitted.input_id, turn, turn_request(worker))
        .await
        .expect_err("a retired conversation answers nothing");
    assert!(matches!(
        refused,
        ion_core::TaskDriverError::Session(ion_core::SessionError::ConversationRetired(id))
            if id == worker
    ));
    assert_eq!(
        placement(&driver.snapshot().await, submitted.input_id),
        placed,
        "the refusal changed nothing"
    );
}
