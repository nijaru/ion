//! Cancellation precedence and the effect of an unresolved action.

mod support;

use std::sync::Arc;
use std::time::Duration;

use ion_ai::{Script, ScriptedModelService, ToolCall};
use ion_core::{
    InvocationState, Resolution, ResolveRequest, ScriptedTool, Session, SubmitRequest, ToolOutcome,
    ToolRegistry, TurnOutcome, TurnPhase,
};
use support::{UncertainTool, answer, database, eventually, services, spec, stream, tool_answer};

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: serde_json::json!({}),
    }
}

#[tokio::test]
async fn only_a_committed_turn_success_beats_cancellation() {
    let path = database("cancel-terminal");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(answer(
        "already done",
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
    let completed = handle.wait(turn).await.expect("wait");
    assert!(completed.is_completed());

    // A cancellation that arrives after the turn settled returns the outcome
    // rather than rewriting it.
    let receipt = handle.cancel(turn).await.expect("cancel");
    assert!(receipt.generation.is_none());
    assert_eq!(receipt.outcome, Some(completed.clone()));
    let view = handle.turn(turn).await.expect("view").expect("turn");
    assert_eq!(view.turn.outcome, Some(completed));
    assert_eq!(model.requests().len(), 1);

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn cancelling_an_in_flight_tool_settles_truthfully_and_repeats_nothing() {
    let path = database("cancel-tool");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(
        tool_answer(call("call-1", "slow")),
    ))]));
    let mut tools = ToolRegistry::new();
    tools.insert(support::PendingTool::new("slow"));
    let mut spec = spec();
    spec.config.tool_names = vec!["slow".to_owned()];

    let session = Session::create(&path, spec, services(model.clone(), tools))
        .await
        .expect("create session");
    let handle = session.handle();
    let turn = handle
        .submit(SubmitRequest::user("do the thing"))
        .await
        .expect("submit")
        .turn
        .expect("turn");

    // Wait until the tool has actually been dispatched, so this is the
    // "dispatch intent committed, result unknown" case.
    eventually(|| async {
        handle.turn(turn).await.expect("view").filter(|view| {
            view.invocations
                .iter()
                .any(|invocation| invocation.state == InvocationState::Dispatched)
        })
    })
    .await;

    let receipt = handle.cancel(turn).await.expect("cancel");
    assert!(receipt.generation.is_some());
    let outcome = handle.wait(turn).await.expect("wait");
    match &outcome {
        TurnOutcome::Cancelled { unresolved } => {
            assert_eq!(unresolved.len(), 1, "the unknown action stays identified");
        }
        other => panic!("expected cancellation, got {other:?}"),
    }

    // The exchange records what happened without inventing success, and the
    // unestablished action is preserved rather than repeated.
    let view = handle.turn(turn).await.expect("view").expect("turn");
    assert_eq!(view.invocations.len(), 1);
    assert_eq!(view.invocations[0].state, InvocationState::Indeterminate);
    assert_eq!(model.requests().len(), 1, "no further model step started");

    let page = handle
        .entries(ion_core::EntryQuery {
            conversation: handle.root(),
            after: None,
            limit: 16,
        })
        .await
        .expect("entries");
    let tool_entry = page
        .entries
        .iter()
        .find(|entry| entry.kind.as_str() == ion_core::TOOL_ENTRY)
        .expect("a truthful result is recorded for the admitted call");
    let recorded = serde_json::to_string(&tool_entry.data).expect("encode");
    assert!(recorded.contains("call-1"), "got {recorded}");

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn an_unknown_tool_outcome_parks_the_turn_until_a_client_decides() {
    let path = database("uncertain");
    let model = Arc::new(ScriptedModelService::new([
        Script::Stream(stream(tool_answer(call("call-1", "write")))),
        Script::Stream(stream(answer("recorded"))),
    ]));
    let mut tools = ToolRegistry::new();
    tools.insert(UncertainTool::new("write"));
    let mut spec = spec();
    spec.config.tool_names = vec!["write".to_owned()];

    let session = Session::create(&path, spec, services(model.clone(), tools))
        .await
        .expect("create session");
    let handle = session.handle();
    let turn = handle
        .submit(SubmitRequest::user("write the file"))
        .await
        .expect("submit")
        .turn
        .expect("turn");

    // The turn parks instead of building on an unestablished fact.
    let blocked = eventually(|| async {
        handle.turn(turn).await.expect("view").filter(|view| {
            matches!(view.turn.phase, TurnPhase::Blocked { .. }) && view.turn.outcome.is_none()
        })
    })
    .await;
    let invocation = blocked.invocations[0].id;
    assert_eq!(blocked.invocations[0].state, InvocationState::Indeterminate);
    assert!(
        handle.turn(turn).await.expect("view").is_some(),
        "the parked turn stays readable"
    );
    assert_eq!(model.requests().len(), 1);

    // Resuming a parked turn does not repeat the action or re-ask the model.
    handle.resume(handle.root()).await.expect("resume");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(model.requests().len(), 1);
    let still = handle.turn(turn).await.expect("view").expect("turn");
    assert!(matches!(still.turn.phase, TurnPhase::Blocked { .. }));
    assert!(still.turn.outcome.is_none());

    // An explicit decision records the uncertainty and lets the exchange finish.
    handle
        .resolve(ResolveRequest {
            turn,
            invocation,
            resolution: Resolution::Indeterminate,
        })
        .await
        .expect("resolve");
    let outcome = handle.wait(turn).await.expect("wait");
    assert!(outcome.is_completed(), "got {outcome:?}");
    assert_eq!(model.requests().len(), 2, "the model saw a truthful result");

    let view = handle.turn(turn).await.expect("view").expect("turn");
    assert_eq!(view.invocations[0].state, InvocationState::Unknown);

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn repeating_an_unknown_action_requires_a_repeat_safe_policy() {
    let path = database("not-repeat-safe");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(
        tool_answer(call("call-1", "write")),
    ))]));
    let mut tools = ToolRegistry::new();
    // A tool that is not repeat-safe: exactly the default for a mutating action.
    tools.insert(Arc::new(ScriptedTool::new(
        "write",
        [ToolOutcome::Indeterminate("did it land?".to_owned())],
    )));
    let mut spec = spec();
    spec.config.tool_names = vec!["write".to_owned()];

    let session = Session::create(&path, spec, services(model, tools))
        .await
        .expect("create session");
    let handle = session.handle();
    let turn = handle
        .submit(SubmitRequest::user("write"))
        .await
        .expect("submit")
        .turn
        .expect("turn");
    let blocked = eventually(|| async {
        handle.turn(turn).await.expect("view").filter(|view| {
            matches!(view.turn.phase, TurnPhase::Blocked { .. }) && view.turn.outcome.is_none()
        })
    })
    .await;
    let invocation = blocked.invocations[0].id;

    let refused = handle
        .resolve(ResolveRequest {
            turn,
            invocation,
            resolution: Resolution::Repeat,
        })
        .await;
    assert!(
        matches!(refused, Err(ion_core::Error::NotRepeatSafe(_))),
        "got {refused:?}"
    );

    session.close().await.expect("close");
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}
