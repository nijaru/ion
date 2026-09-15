//! A scripted turn: admission, model steps, tool execution and selection.

mod support;

use std::sync::Arc;

use ion_ai::{Script, ScriptedModelService, ToolCall};
use ion_core::{
    Error, InputDisposition, InputMode, RunLimits, ScriptedTool, Session, SessionSpec,
    SubmitRequest, ToolOutcome, ToolRegistry, TurnOutcome,
};
use support::{answer, config, database, eventually, services, spec, stream, tool_answer};

fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: serde_json::json!({"path": "src/main.rs"}),
    }
}

#[tokio::test]
async fn a_single_submission_runs_to_a_selected_final_answer() {
    let path = database("turn");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(answer(
        "done",
    )))]));
    let session = Session::create(&path, spec(), services(model.clone(), ToolRegistry::new()))
        .await
        .expect("create session");
    let handle = session.handle();

    let receipt = handle
        .submit(SubmitRequest::user("hello"))
        .await
        .expect("submit");
    assert!(!receipt.replay);
    let turn = receipt.turn.expect("admission starts a turn");

    let outcome = handle.wait(turn).await.expect("wait");
    match outcome {
        TurnOutcome::Completed { entry } => {
            // The completed outcome names the assistant entry, not the input.
            let page = handle
                .entries(ion_core::EntryQuery {
                    conversation: handle.root(),
                    after: None,
                    limit: 16,
                })
                .await
                .expect("read entries");
            assert_eq!(page.entries.len(), 2, "input and assistant entries");
            assert_eq!(page.entries[1].id, entry);
            assert_eq!(page.entries[0].kind.as_str(), ion_core::INPUT_ENTRY);
            assert_eq!(page.entries[1].kind.as_str(), ion_core::ASSISTANT_ENTRY);
        }
        other => panic!("expected a completed turn, got {other:?}"),
    }

    let view = handle.turn(turn).await.expect("turn view").expect("turn");
    assert_eq!(view.attempts.len(), 1);
    assert_eq!(view.attempts[0].state, ion_core::AttemptState::Settled);
    assert!(view.step.is_some());

    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].instructions.as_deref(), Some("be careful"));
    // The frozen basis carries the project context ahead of the transcript.
    assert_eq!(requests[0].messages.len(), 2);

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn tool_calls_run_in_order_and_the_next_request_carries_their_results() {
    let path = database("tools");
    let model = Arc::new(ScriptedModelService::new([
        Script::Stream(stream(tool_answer(tool_call("call-1", "read")))),
        Script::Stream(stream(answer("read it"))),
    ]));
    let read = Arc::new(
        ScriptedTool::new(
            "read",
            [ToolOutcome::Completed(
                serde_json::json!({"content": "fn main() {}"}),
            )],
        )
        .repeat_safe(true),
    );
    let mut tools = ToolRegistry::new();
    tools.insert(read.clone());
    let mut spec = spec();
    spec.config.tool_names = vec!["read".to_owned()];

    let session = Session::create(&path, spec, services(model.clone(), tools))
        .await
        .expect("create session");
    let handle = session.handle();
    let turn = handle
        .submit(SubmitRequest::user("read src/main.rs"))
        .await
        .expect("submit")
        .turn
        .expect("turn");

    let outcome = handle.wait(turn).await.expect("wait");
    assert!(
        outcome.is_completed(),
        "expected completion, got {outcome:?}"
    );

    assert_eq!(read.calls().len(), 1, "the tool ran exactly once");
    let requests = model.requests();
    assert_eq!(requests.len(), 2, "one request per model step");
    let second = &requests[1];
    assert!(
        second.messages.iter().any(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ion_ai::Content::ToolResult(_)))
        }),
        "the second request must carry the tool result"
    );
    // Two steps, one attempt each: a step is charged per request basis.
    let view = handle.turn(turn).await.expect("view").expect("turn");
    assert_eq!(view.attempts.len(), 1, "the current step's attempts");

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn a_busy_conversation_queues_follow_ups_and_drains_one_successor() {
    let path = database("queue");
    let model = Arc::new(ScriptedModelService::new([
        Script::Stream(stream(tool_answer(tool_call("call-1", "slow")))),
        Script::Stream(stream(answer("finished"))),
        Script::Stream(stream(answer("second answer"))),
    ]));
    let mut tools = ToolRegistry::new();
    tools.insert(Arc::new(
        ScriptedTool::new("slow", [ToolOutcome::Completed(serde_json::json!("ok"))])
            .repeat_safe(true),
    ));
    let mut spec = spec();
    spec.config.tool_names = vec!["slow".to_owned()];

    let session = Session::create(&path, spec, services(model.clone(), tools))
        .await
        .expect("create session");
    let handle = session.handle();
    let first = handle
        .submit(SubmitRequest::user("first"))
        .await
        .expect("submit")
        .turn
        .expect("turn");

    // A second submission while the turn is unfinished is refused: one
    // conversation answers one turn at a time.
    let busy = handle.submit(SubmitRequest::user("second")).await;
    assert!(matches!(busy, Err(Error::Busy(_))), "got {busy:?}");

    // A follow-up waits instead of being refused, and is not in the transcript.
    let queued = handle
        .submit(SubmitRequest::follow_up("second"))
        .await
        .expect("follow up");
    assert!(queued.turn.is_none());
    let input = handle
        .input(queued.input)
        .await
        .expect("read input")
        .expect("input");
    assert_eq!(input.mode, InputMode::FollowUp);
    assert_eq!(input.disposition, InputDisposition::Queued);
    let before = handle
        .entries(ion_core::EntryQuery {
            conversation: handle.root(),
            after: None,
            limit: 16,
        })
        .await
        .expect("entries");
    assert_eq!(before.entries.len(), 1, "queued input is not placed yet");

    let first_outcome = handle.wait(first).await.expect("wait");
    assert!(first_outcome.is_completed());

    // The follow-up becomes its own successor turn, and only then is it placed.
    let placed = eventually(|| async {
        handle
            .input(queued.input)
            .await
            .expect("read input")
            .filter(|input| matches!(input.disposition, InputDisposition::Placed(_)))
    })
    .await;
    let placement = placed.disposition.placement().expect("placement");
    assert_ne!(placement.turn, first, "the successor is a new turn");

    let successor = eventually(|| async {
        let page = handle
            .entries(ion_core::EntryQuery {
                conversation: handle.root(),
                after: None,
                limit: 16,
            })
            .await
            .expect("entries");
        (page.entries.len() >= 6).then_some(page)
    })
    .await;
    assert_eq!(
        successor.entries[4].kind.as_str(),
        ion_core::INPUT_ENTRY,
        "the successor turn places the queued input"
    );
    assert_eq!(
        successor.entries.last().expect("entry").kind.as_str(),
        ion_core::ASSISTANT_ENTRY
    );

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn an_exact_replay_returns_the_original_receipt_and_a_conflict_is_refused() {
    let path = database("replay");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(answer(
        "ok",
    )))]));
    let session = Session::create(&path, spec(), services(model.clone(), ToolRegistry::new()))
        .await
        .expect("create session");
    let handle = session.handle();

    let key = Some(ion_core::RequestKey::new("request-1").expect("key"));
    let first = handle
        .submit(SubmitRequest {
            request_key: key.clone(),
            ..SubmitRequest::user("hello")
        })
        .await
        .expect("submit");
    handle.wait(first.turn.expect("turn")).await.expect("wait");

    let replay = handle
        .submit(SubmitRequest {
            request_key: key.clone(),
            ..SubmitRequest::user("hello")
        })
        .await
        .expect("replay");
    assert!(replay.replay);
    assert_eq!(replay.input, first.input);
    assert_eq!(model.requests().len(), 1, "a replay starts no new work");

    let conflict = handle
        .submit(SubmitRequest {
            request_key: key,
            ..SubmitRequest::user("something else")
        })
        .await;
    assert!(
        matches!(conflict, Err(Error::RequestKeyConflict { .. })),
        "got {conflict:?}"
    );

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn a_dropped_waiter_does_not_stop_the_turn() {
    let path = database("dropped-waiter");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(answer(
        "still here",
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

    // Abandon a client wait before the outcome exists.
    drop(handle.wait(turn));

    let outcome = eventually(|| async {
        handle
            .turn(turn)
            .await
            .expect("view")
            .and_then(|view| view.turn.outcome)
    })
    .await;
    assert!(outcome.is_completed(), "got {outcome:?}");

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn withdrawing_an_unplaced_input_keeps_the_transcript_untouched() {
    let path = database("withdraw");
    let model = Arc::new(ScriptedModelService::new([
        Script::Stream(stream(tool_answer(tool_call("call-1", "slow")))),
        Script::Stream(stream(answer("done"))),
    ]));
    let mut tools = ToolRegistry::new();
    tools.insert(Arc::new(
        ScriptedTool::new("slow", [ToolOutcome::Completed(serde_json::json!("ok"))])
            .repeat_safe(true),
    ));
    let mut spec = spec();
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
        .submit(SubmitRequest::follow_up("never mind"))
        .await
        .expect("follow up");

    handle.withdraw(queued.input).await.expect("withdraw");
    let input = handle
        .input(queued.input)
        .await
        .expect("read input")
        .expect("input");
    assert_eq!(input.disposition, InputDisposition::Cancelled);

    let outcome = handle.wait(turn).await.expect("wait");
    assert!(outcome.is_completed());
    // No successor turn was started for the withdrawn input.
    let page = handle
        .entries(ion_core::EntryQuery {
            conversation: handle.root(),
            after: None,
            limit: 32,
        })
        .await
        .expect("entries");
    assert!(
        !page
            .entries
            .iter()
            .any(|entry| serde_json::to_string(&entry.data)
                .is_ok_and(|data| data.contains("never mind"))),
        "the withdrawn text must not appear in the transcript"
    );

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn a_step_budget_limit_fails_the_turn_instead_of_looping() {
    let path = database("limit");
    // The model always asks for the same tool, so the turn would never finish
    // without the step ceiling.
    let model = Arc::new(ScriptedModelService::new((0..8).map(|index| {
        Script::Stream(stream(tool_answer(tool_call(
            &format!("call-{index}"),
            "read",
        ))))
    })));
    let mut tools = ToolRegistry::new();
    tools.insert(Arc::new(
        ScriptedTool::new(
            "read",
            (0..8).map(|_| ToolOutcome::Completed(serde_json::json!("again"))),
        )
        .repeat_safe(true),
    ));
    let mut spec: SessionSpec = spec();
    spec.config.tool_names = vec!["read".to_owned()];
    spec.config.limits = RunLimits {
        max_model_steps: 2,
        ..config().limits
    };

    let session = Session::create(&path, spec, services(model.clone(), tools))
        .await
        .expect("create session");
    let handle = session.handle();
    let turn = handle
        .submit(SubmitRequest::user("loop"))
        .await
        .expect("submit")
        .turn
        .expect("turn");

    let outcome = handle.wait(turn).await.expect("wait");
    match outcome {
        TurnOutcome::Failed { cause } => {
            assert!(
                format!("{cause:?}").contains("max_model_steps"),
                "unexpected cause: {cause:?}"
            );
        }
        other => panic!("expected a limit failure, got {other:?}"),
    }
    assert_eq!(model.requests().len(), 2, "the ceiling bounds the work");

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}
