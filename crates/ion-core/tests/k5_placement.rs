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
use ion_core::conversation::context::{ContextControl, ContextEdit};
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
    registry_with(Arc::new(ScriptedModelService::new(scripts)))
}

fn registry_with(service: Arc<ScriptedModelService>) -> TaskRegistry {
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
    let db = TempDb::new("r4-cancelled");
    let driver = durable([Script::Stream(vec![completed("never sent")])], db.path());
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
    let TaskStatus::Terminal(outcome) = record.status.clone() else {
        panic!("the aborted turn is terminal");
    };
    assert_eq!(outcome.kind, TaskOutcomeKind::Aborted);
    assert_eq!(
        transcript(&driver, root).await,
        vec![("user".to_owned(), "answer me".to_owned())],
        "the accepted message is history even though no answer was attempted"
    );
    let placed = placement(&driver.snapshot().await, submitted.input_id);
    assert_eq!(placed.turn, turn);
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    // After a restart the placed input is still answerable, and the new attempt
    // is not born cancelled: the barrier belonged to the turn that ended.
    let reopened = reopen(db.path(), [Script::Stream(vec![completed("answered")])]);
    assert_eq!(
        transcript(&reopened, root).await,
        vec![("user".to_owned(), "answer me".to_owned())],
        "a handover places nothing new"
    );
    let retry = reopened
        .retry_input(submitted.input_id, turn, turn_request(root))
        .await
        .expect("a cancelled attempt can be retried");
    let snapshot = reopened.snapshot().await;
    assert_eq!(placement(&snapshot, submitted.input_id).entry, placed.entry);
    assert!(
        !snapshot
            .tasks
            .iter()
            .find(|task| task.id == retry)
            .expect("retry task")
            .cancel_requested,
        "a retry after cancellation is fresh work"
    );
    let conversation = snapshot
        .conversations
        .iter()
        .find(|candidate| candidate.id == root)
        .expect("root conversation");
    assert!(!conversation.turn_cancelled, "the old barrier is over");
    let DriveOutcome::Settled(settlement) = reopened.drive_task(retry).await.expect("drive") else {
        panic!("the retry settles");
    };
    assert_eq!(settlement.outcome.kind, TaskOutcomeKind::Completed);
    assert!(
        reopened.turn_closed_by(turn).await.is_some(),
        "the cancelled attempt's closure receipt survives"
    );
    assert_eq!(
        transcript(&reopened, root).await,
        vec![
            ("user".to_owned(), "answer me".to_owned()),
            ("assistant".to_owned(), "answered".to_owned()),
        ]
    );
    reopened.close(CloseMode::Graceful).await;
}

/// A terminal root is not a closed turn: live members still hold the slot, so a
/// retry would overlap work that is still running.
#[tokio::test]
async fn a_retry_is_refused_while_a_member_of_the_turn_is_still_live() {
    let mut registry = TaskRegistry::new();
    let service = Arc::new(ScriptedModelService::new([Script::Stream(vec![
        ion_ai::ModelStreamEvent::Completed(ion_ai::ModelResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ion_ai::ToolCall {
                    id: "call-1".to_owned(),
                    name: "echo".to_owned(),
                    arguments: json!({}),
                })],
                provider_replay: None,
            },
            usage: Usage::known(1, 1),
            termination: ResponseTermination::Completed,
        }),
    ])]));
    registry
        .register(
            kind(ion_core::builtin::GENERATION),
            1,
            Arc::new(ion_core::builtin::GenerationKind::new(
                model_ref(),
                service,
                Arc::new(ion_core::builtin::ToolCatalog::new()),
            )),
        )
        .expect("generation");
    registry
        .register(
            kind(ion_core::builtin::POST_TOOLS),
            1,
            Arc::new(ion_core::builtin::PostToolsKind),
        )
        .expect("post-tools");
    // The tool kind is deliberately absent, so the turn's tool member stays
    // pending and the turn never closes.
    let driver = TaskDriver::new(Session::new().expect("session"), registry);
    let root = driver.snapshot().await.root_conversation;
    let submitted = driver
        .submit_input(submission(root, "turn-1", "use a tool"), turn_request(root))
        .await
        .expect("submit");
    let turn = submitted.task_id.expect("turn root");
    let DriveOutcome::Settled(_) = driver.drive_task(turn).await.expect("drive") else {
        panic!("the generation settles even though its tool child cannot run");
    };
    assert!(
        driver.turn_closed_by(turn).await.is_none(),
        "the turn is still open"
    );
    let refused = driver
        .retry_input(submitted.input_id, turn, turn_request(root))
        .await
        .expect_err("a live turn cannot be retried");
    assert!(matches!(
        refused,
        ion_core::TaskDriverError::Session(ion_core::SessionError::TurnStillOpen(id)) if id == turn
    ));
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
    let failed = driver.task(turn).await.expect("attempt").status;
    let placed = placement(&driver.snapshot().await, submitted.input_id);
    driver.close(CloseMode::Graceful).await;
    drop(driver);

    // An explicit retry answers the same placed input: no second admission, no
    // second user message, and a genuinely new attempt.
    let service = Arc::new(ScriptedModelService::new([Script::Stream(vec![
        completed("retried"),
    ])]));
    let reopened = TaskDriver::open(db.path(), registry_with(service.clone())).expect("reopen");
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
    assert_eq!(
        reopened.task(turn).await.expect("attempt").status,
        failed,
        "a retry never rewrites the failed attempt's outcome"
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
        service
            .requests()
            .iter()
            .filter(|request| request_includes_messages(request, "hello"))
            .count(),
        1,
        "the retry sends the placed message exactly once"
    );
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

/// Provenance is contribution, not presence: an edit that omits the placed entry
/// must not leave the frozen request claiming it included that input, and the
/// input must still be answerable and answerable again.
#[tokio::test]
async fn an_omitted_input_is_not_claimed_as_included() {
    let service = Arc::new(ScriptedModelService::new([
        Script::Stream(vec![completed("first")]),
        Script::Stream(vec![completed("second")]),
    ]));
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let submitted = session
        .admit_input(
            submission(root, "turn-1", "please forget me"),
            Some(turn_request(root)),
        )
        .expect("admit");
    let turn = submitted.task_id.expect("turn root");
    let placed = session.snapshot().inputs[0]
        .disposition
        .placement()
        .expect("placed");

    // The context control exists before the attempt freezes its request.
    session
        .append_entry(ion_core::EntryRequest {
            conversation_id: root,
            kind: ion_core::EntryKind::new("note").expect("entry kind"),
            data: json!({"text": "context control"}),
            projection: Vec::new(),
            context: ContextControl {
                head: None,
                edits: vec![ContextEdit::Omit {
                    target: placed.entry,
                }],
            },
        })
        .expect("an omit edit");

    let driver = TaskDriver::new(session, registry_with(service.clone()));
    driver.drive_task(turn).await.expect("drive the attempt");
    assert!(
        !request_includes(&service.requests()[0], "please forget me"),
        "the omitted input is not in the request"
    );
    let checkpoint = driver
        .task(turn)
        .await
        .expect("turn record")
        .checkpoint
        .expect("a frozen request");
    assert_eq!(
        checkpoint["inputs"],
        json!([]),
        "a frozen request must not claim an input it did not include"
    );

    // The input is still answerable: the binding records intent, and what the
    // next attempt sends is decided by the context it reads.
    let retry = driver
        .retry_input(submitted.input_id, turn, turn_request(root))
        .await
        .expect("a retry does not require the input to be model-visible");
    assert_eq!(
        placement(&driver.snapshot().await, submitted.input_id).turn,
        retry
    );
    driver.drive_task(retry).await.expect("drive the retry");
    assert!(
        !request_includes(&service.requests()[1], "please forget me"),
        "the retry sends the context as it stands"
    );
}

/// Abandonment records answer intent only, so an edit that drops the placed
/// content from context must not make it impossible.
#[tokio::test]
async fn an_input_outside_the_context_can_still_be_abandoned() {
    let mut session = Session::new().expect("session");
    let root = session.root_conversation();
    let submitted = session
        .admit_input(
            submission(root, "turn-1", "hello"),
            Some(turn_request(root)),
        )
        .expect("admit");
    let turn = submitted.task_id.expect("turn root");
    let placed = session.snapshot().inputs[0]
        .disposition
        .placement()
        .expect("placed");
    session
        .append_entry(ion_core::EntryRequest {
            conversation_id: root,
            kind: ion_core::EntryKind::new("note").expect("entry kind"),
            data: json!({"text": "context control"}),
            projection: Vec::new(),
            context: ContextControl {
                head: None,
                edits: vec![ContextEdit::Omit {
                    target: placed.entry,
                }],
            },
        })
        .expect("an omit edit");

    let driver = TaskDriver::new(
        session,
        registry([Script::Stream(vec![completed("answer")])]),
    );
    driver.drive_task(turn).await.expect("drive");
    driver
        .abandon_input(submitted.input_id, turn)
        .await
        .expect("abandonment does not depend on future model context");
    let snapshot = driver.snapshot().await;
    assert_eq!(
        snapshot
            .inputs
            .iter()
            .find(|input| input.id == submitted.input_id)
            .expect("input")
            .disposition,
        ion_core::InputDisposition::Abandoned(placed),
        "the placement is retained"
    );
}

fn request_includes(request: &ion_ai::ModelRequest, text: &str) -> bool {
    request.messages.iter().any(|message| {
        message
            .content
            .iter()
            .any(|content| matches!(content, Content::Text(found) if found.contains(text)))
    })
}

fn request_includes_messages(request: &ion_ai::ModelRequest, text: &str) -> bool {
    request.messages.iter().any(|message| {
        message.role == Role::User
            && message
                .content
                .iter()
                .any(|content| matches!(content, Content::Text(found) if found.contains(text)))
    })
}
