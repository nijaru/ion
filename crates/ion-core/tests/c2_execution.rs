//! The execution boundary: stop, join, close and late evidence.
//!
//! These regressions exist because a turn that stops waiting for an action must
//! not drop it. Each one drives an action that outlives its turn and asserts
//! what the engine owns, what it records, and what it refuses to claim.

mod support;

use std::sync::Arc;
use std::time::Duration;

use ion_ai::{Script, ScriptedModelService, ToolCall};
use ion_core::{
    CloseOutcome, Error, InvocationState, Resolution, ResolveRequest, Session, SessionEvent,
    SubmitRequest, ToolOutcome, ToolRegistry, TurnOutcome, TurnPhase, WatchError,
};
use support::{
    DeafTool, ImmortalTool, PanickingTool, answer, database, eventually, services, spec, stream,
    tool_answer,
};
use tokio::time::timeout;

fn call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: serde_json::json!({}),
    }
}

#[tokio::test]
async fn an_action_that_outlives_its_stop_is_published_rather_than_revised() {
    let path = database("late-evidence");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(
        tool_answer(call("call-1", "deaf")),
    ))]));
    let tool = DeafTool::new("deaf");
    let mut tools = ToolRegistry::new();
    tools.insert(tool.clone());
    let mut spec = spec();
    spec.config.tool_names = vec!["deaf".to_owned()];

    let mut session = Session::create(&path, spec, services(model.clone(), tools))
        .await
        .expect("create session");
    let handle = session.handle();
    let mut watch = handle.watch();
    let turn = handle
        .submit(SubmitRequest::user("do the thing"))
        .await
        .expect("submit")
        .turn
        .expect("turn");
    tool.started().await;

    let receipt = handle.cancel(turn).await.expect("cancel");
    assert!(receipt.generation.is_some());
    let outcome = handle.wait(turn).await.expect("wait");
    let unresolved = match &outcome {
        TurnOutcome::Cancelled { unresolved } => unresolved.clone(),
        other => panic!("expected cancellation, got {other:?}"),
    };
    assert_eq!(
        unresolved.len(),
        1,
        "an action that never reported stays identified"
    );

    // The action is still running and the session still owns it. Its report
    // arrives after the turn settled, so it is published instead of revising
    // the turn or resuming the cancelled continuation.
    tool.release();
    let (reported, evidence) = timeout(Duration::from_secs(5), async {
        loop {
            match watch.recv().await {
                Ok(SessionEvent::InvocationEvidence {
                    invocation,
                    outcome,
                    ..
                }) => break (invocation, outcome),
                Ok(_) => {}
                Err(WatchError::Lagged { .. }) => {}
                Err(WatchError::Closed) => panic!("the session closed before the report"),
            }
        }
    })
    .await
    .expect("a late report is published");
    assert_eq!(reported, unresolved[0]);
    assert_eq!(
        evidence,
        ToolOutcome::Completed(serde_json::json!({"completed": true}))
    );

    // Nothing was revised and nothing continued.
    assert_eq!(model.requests().len(), 1, "a late report resumes nothing");
    let view = handle.turn(turn).await.expect("view").expect("turn");
    assert_eq!(
        view.invocations[0].state,
        InvocationState::Indeterminate,
        "the durable record keeps the conservative claim"
    );
    assert!(view.turn.outcome.is_some(), "the turn stays settled");

    // The action has reported, so it is joined and the session can close.
    assert!(
        session.close().await.expect("close").is_closed(),
        "a joined action does not keep the session open"
    );
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}

#[tokio::test]
async fn close_reports_still_closing_while_an_action_is_unjoined() {
    let path = database("still-closing");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(
        tool_answer(call("call-1", "immortal")),
    ))]));
    let tool = ImmortalTool::new("immortal");
    let mut tools = ToolRegistry::new();
    tools.insert(tool.clone());
    let mut spec = spec();
    spec.config.tool_names = vec!["immortal".to_owned()];

    let mut session = Session::create(&path, spec, services(model, tools))
        .await
        .expect("create session");
    let handle = session.handle();
    let turn = handle
        .submit(SubmitRequest::user("do the thing"))
        .await
        .expect("submit")
        .turn
        .expect("turn");
    tool.started().await;

    // The turn settles truthfully once it stops waiting; the action does not.
    let receipt = handle.cancel(turn).await.expect("cancel");
    assert!(receipt.generation.is_some());
    let outcome = handle.wait(turn).await.expect("wait");
    match &outcome {
        TurnOutcome::Cancelled { unresolved } => assert_eq!(unresolved.len(), 1),
        other => panic!("expected cancellation, got {other:?}"),
    }

    let closed = session.close().await.expect("close");
    match closed {
        CloseOutcome::StillClosing { pending } => {
            assert_eq!(pending, 1, "the unjoined action keeps the session open");
        }
        CloseOutcome::Closed => panic!("an action that never reported cannot be joined"),
    }
    // Closing again retries the join and still refuses to claim quiescence.
    let again = session.close().await.expect("close again");
    assert!(!again.is_closed(), "got {again:?}");
    assert_eq!(again.pending(), 1);

    // A client cannot admit work into a session that is still closing.
    let refused = handle.submit(SubmitRequest::user("more")).await;
    assert!(matches!(refused, Err(Error::Closed)), "got {refused:?}");
}

#[tokio::test]
async fn close_retains_ownership_until_a_retry_joins_the_action() {
    let path = database("close-ownership");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(
        tool_answer(call("call-1", "deaf")),
    ))]));
    let tool = DeafTool::new("deaf");
    let mut tools = ToolRegistry::new();
    tools.insert(tool.clone());
    let mut config = spec();
    config.config.tool_names = vec!["deaf".to_owned()];
    let mut session = Session::create(&path, config, services(model.clone(), tools))
        .await
        .expect("create");
    let handle = session.handle();
    handle
        .submit(SubmitRequest::user("run"))
        .await
        .expect("submit");
    timeout(Duration::from_secs(5), tool.started())
        .await
        .expect("started");

    let outcome = timeout(Duration::from_secs(5), session.close())
        .await
        .expect("bounded close")
        .expect("close");
    assert!(
        matches!(outcome, CloseOutcome::StillClosing { .. }),
        "{outcome:?}"
    );
    let refused = Session::open(
        &path,
        support::limits(),
        services(model.clone(), ToolRegistry::new()),
    )
    .await;
    assert!(
        matches!(refused, Err(Error::SessionInUse(_))),
        "{refused:?}"
    );

    tool.release();
    let outcome = timeout(Duration::from_secs(5), session.close())
        .await
        .expect("bounded retry")
        .expect("retry");
    assert!(outcome.is_closed(), "{outcome:?}");
    let mut reopened = Session::open(
        &path,
        support::limits(),
        services(model, ToolRegistry::new()),
    )
    .await
    .expect("ownership released after join");
    assert!(reopened.close().await.expect("close reopened").is_closed());
    std::fs::remove_dir_all(path.parent().expect("dir")).expect("cleanup");
}

#[tokio::test]
async fn closing_finishes_after_the_action_without_another_close_request() {
    let path = database("automatic-close");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(
        tool_answer(call("call-1", "deaf")),
    ))]));
    let tool = DeafTool::new("deaf");
    let mut tools = ToolRegistry::new();
    tools.insert(tool.clone());
    let mut config = spec();
    config.config.tool_names = vec!["deaf".to_owned()];
    let mut session = Session::create(&path, config, services(model.clone(), tools))
        .await
        .expect("create");
    let handle = session.handle();
    let mut watch = handle.watch();
    handle
        .submit(SubmitRequest::user("run"))
        .await
        .expect("submit");
    timeout(Duration::from_secs(5), tool.started())
        .await
        .expect("started");
    assert!(!session.close().await.expect("close").is_closed());
    tool.release();
    timeout(Duration::from_secs(5), async {
        loop {
            if matches!(watch.recv().await.expect("event"), SessionEvent::Closed) {
                break;
            }
        }
    })
    .await
    .expect("close completes without a retry");
    let mut reopened = Session::open(
        &path,
        support::limits(),
        services(model, ToolRegistry::new()),
    )
    .await
    .expect("ownership released");
    assert!(
        session
            .close()
            .await
            .expect("join already closed supervisor")
            .is_closed()
    );
    assert!(reopened.close().await.expect("close reopened").is_closed());
    std::fs::remove_dir_all(path.parent().expect("dir")).expect("cleanup");
}

#[tokio::test]
async fn a_turn_deadline_stops_waiting_without_claiming_the_action_stopped() {
    let path = database("execution-deadline");
    let model = Arc::new(ScriptedModelService::new([Script::Stream(stream(
        tool_answer(call("call-1", "deaf")),
    ))]));
    let tool = DeafTool::new("deaf");
    let mut tools = ToolRegistry::new();
    tools.insert(tool.clone());
    let mut config = spec();
    config.config.tool_names = vec!["deaf".to_owned()];
    config.config.limits.deadline_ms = 200;
    let mut session = Session::create(&path, config, services(model.clone(), tools))
        .await
        .expect("create");
    let handle = session.handle();
    let turn = handle
        .submit(SubmitRequest::user("run"))
        .await
        .expect("submit")
        .turn
        .expect("turn");
    timeout(Duration::from_secs(5), tool.started())
        .await
        .expect("started");
    let outcome = timeout(Duration::from_secs(5), handle.wait(turn))
        .await
        .expect("bounded deadline")
        .expect("wait");
    assert!(
        matches!(
            outcome,
            TurnOutcome::Failed {
                cause: ion_core::TurnFailure::Deadline
            }
        ),
        "{outcome:?}"
    );
    let view = handle.turn(turn).await.expect("view").expect("turn");
    assert_eq!(view.invocations[0].state, InvocationState::Indeterminate);
    assert_eq!(model.requests().len(), 1);
    assert!(!session.close().await.expect("close").is_closed());
    tool.release();
    assert!(session.close().await.expect("retry").is_closed());
    std::fs::remove_dir_all(path.parent().expect("dir")).expect("cleanup");
}

#[tokio::test]
async fn a_panicking_action_is_unresolved_and_never_a_known_failure() {
    let path = database("panicking");
    let model = Arc::new(ScriptedModelService::new([
        Script::Stream(stream(tool_answer(call("call-1", "boom")))),
        Script::Stream(stream(answer("recorded"))),
    ]));
    let mut tools = ToolRegistry::new();
    tools.insert(PanickingTool::new("boom"));
    let mut spec = spec();
    spec.config.tool_names = vec!["boom".to_owned()];

    let mut session = Session::create(&path, spec, services(model.clone(), tools))
        .await
        .expect("create session");
    let handle = session.handle();
    let turn = handle
        .submit(SubmitRequest::user("do the thing"))
        .await
        .expect("submit")
        .turn
        .expect("turn");

    // A panic establishes nothing about the action, so the exchange parks
    // instead of reading the panic as a known failure.
    let blocked = eventually(|| async {
        handle.turn(turn).await.expect("view").filter(|view| {
            matches!(view.turn.phase, TurnPhase::Blocked { .. }) && view.turn.outcome.is_none()
        })
    })
    .await;
    assert_eq!(blocked.invocations[0].state, InvocationState::Indeterminate);
    assert_eq!(model.requests().len(), 1);

    // A client decides, and only then does the exchange continue.
    let invocation = blocked.invocations[0].id;
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

    assert!(session.close().await.expect("close").is_closed());
    std::fs::remove_dir_all(path.parent().expect("dir")).ok();
}
