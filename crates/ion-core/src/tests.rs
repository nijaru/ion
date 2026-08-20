use std::time::Duration;

use serde_json::json;
use tokio::time::{sleep, timeout};

use crate::error::{CommandError, RuntimeError};
use crate::provider::{ScriptedMessage, ScriptedProvider};
use crate::runtime::{Runtime, RuntimeEvent, SaturatedHandle, TurnStatus};
use crate::tool::{ToolRegistry, ToolSpec};

const STEP: Duration = Duration::from_millis(50);

async fn collect_until_terminal(
    events: &mut crate::EventSubscription,
) -> Result<Vec<RuntimeEvent>, RuntimeError> {
    let mut out = Vec::new();
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event recv timed out")?;
        let done = matches!(
            event,
            RuntimeEvent::TurnFinished { .. }
                | RuntimeEvent::TurnCancelled { .. }
                | RuntimeEvent::TurnFailed { .. }
                | RuntimeEvent::RuntimeShutdown { .. }
        );
        out.push(event);
        if done {
            return Ok(out);
        }
    }
}

fn kinds(events: &[RuntimeEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|event| match event {
            RuntimeEvent::RuntimeStarted { .. } => "runtime_started",
            RuntimeEvent::TurnStarted { .. } => "turn_started",
            RuntimeEvent::AssistantTextDelta { .. } => "assistant_text_delta",
            RuntimeEvent::TurnFinished { .. } => "turn_finished",
            RuntimeEvent::TurnCancelled { .. } => "turn_cancelled",
            RuntimeEvent::TurnFailed { .. } => "turn_failed",
            RuntimeEvent::RuntimeShutdown { .. } => "runtime_shutdown",
        })
        .collect()
}

fn texts(events: &[RuntimeEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::AssistantTextDelta { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

fn failure_message(events: &[RuntimeEvent]) -> Option<String> {
    events.iter().find_map(|event| match event {
        RuntimeEvent::TurnFailed { message, .. } => Some(message.clone()),
        _ => None,
    })
}

fn tool_runtime() -> Runtime {
    Runtime::start(ScriptedProvider::echo(), ToolRegistry::default())
}

// ---- Phase 1 (print-mode) regression tests ----

#[tokio::test]
async fn streams_scripted_text_in_order() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("hel"),
            ScriptedMessage::text("lo"),
        ]),
        ToolRegistry::default(),
    );
    let handle = runtime.handle();
    let (snapshot, mut events) = handle.subscribe().await.expect("subscribe");
    assert_eq!(snapshot.turn, TurnStatus::Idle);

    let turn_id = handle.submit("hi").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert_eq!(
        kinds(&recorded),
        [
            "turn_started",
            "assistant_text_delta",
            "assistant_text_delta",
            "turn_finished"
        ]
    );
    assert!(matches!(
        recorded[0],
        RuntimeEvent::TurnStarted { turn_id: id, .. } if id == turn_id
    ));
    assert_eq!(texts(&recorded), vec!["hel".to_owned(), "lo".to_owned()]);
    let cursors: Vec<_> = recorded.iter().map(RuntimeEvent::cursor).collect();
    assert!(cursors.windows(2).all(|pair| pair[0] < pair[1]));

    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_stops_provider_before_later_chunks() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("one"),
            ScriptedMessage::delayed(Duration::from_secs(30), "two"),
        ]),
        ToolRegistry::default(),
    );
    let handle = runtime.handle();
    let (_snapshot, mut events) = handle.subscribe().await.expect("subscribe");
    let turn_id = handle.submit("slow").await.expect("submit");

    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("delta")
            .expect("recv");
        if matches!(event, RuntimeEvent::AssistantTextDelta { .. }) {
            break;
        }
    }

    handle.cancel(turn_id).await.expect("cancel");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(recorded.iter().any(
        |event| matches!(event, RuntimeEvent::TurnCancelled { turn_id: id, .. } if *id == turn_id)
    ));
    assert!(!recorded.iter().any(|event| matches!(
        event,
        RuntimeEvent::AssistantTextDelta { text, .. } if text == "two"
    )));

    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn busy_submit_is_rejected() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "later",
        )]),
        ToolRegistry::default(),
    );
    let handle = runtime.handle();
    let first = handle.submit("a").await.expect("first submit");
    let err = handle.submit("b").await.expect_err("second submit");
    assert_eq!(err, CommandError::Busy { turn_id: first });
    handle.cancel(first).await.expect("cancel");
    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn saturated_queue_returns_error() {
    let saturated = SaturatedHandle::new();
    let err = saturated
        .handle()
        .submit("overflow")
        .await
        .expect_err("saturated");
    assert_eq!(err, CommandError::QueueSaturated);
}

#[tokio::test]
async fn shutdown_rejects_new_work_and_joins() {
    let runtime = tool_runtime();
    let handle = runtime.handle();
    handle.shutdown().await.expect("shutdown");
    let err = handle.submit("after").await.expect_err("closed");
    assert_eq!(err, CommandError::Closed);
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn print_frontend_writes_streamed_text() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![
            ScriptedMessage::text("hel"),
            ScriptedMessage::text("lo\n"),
        ]),
        ToolRegistry::default(),
    );
    let handle = runtime.handle();
    let mut buf = Vec::new();
    crate::PrintFrontend::new(&mut buf)
        .run(&handle, "hi")
        .await
        .expect("print");
    assert_eq!(String::from_utf8(buf).expect("utf8"), "hello\n");
    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

#[tokio::test(start_paused = true)]
async fn delayed_chunk_respects_cancel_without_waiting_full_delay() {
    let runtime = Runtime::start(
        ScriptedProvider::new(vec![ScriptedMessage::delayed(
            Duration::from_secs(30),
            "late",
        )]),
        ToolRegistry::default(),
    );
    let handle = runtime.handle();
    let (_snapshot, mut events) = handle.subscribe().await.expect("subscribe");
    let turn_id = handle.submit("wait").await.expect("submit");
    sleep(STEP).await;
    handle.cancel(turn_id).await.expect("cancel");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::TurnCancelled { .. })
    ));
    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

// ---- Tool registry unit tests ----

#[tokio::test]
async fn registry_exposes_core_tool_specs() {
    let registry = ToolRegistry::default();
    let specs = registry.specs();
    let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"read"));
    assert!(names.contains(&"write"));
    assert!(names.contains(&"edit"));
    assert!(names.contains(&"bash"));
    assert!(names.contains(&"search"));
    assert!(names.contains(&"find"));
    let _ = registry.get("read");
}

#[tokio::test]
async fn registry_rejects_missing_required_args() {
    let registry = ToolRegistry::default();
    let err = registry
        .validate("read", &json!({}))
        .expect_err("missing path should fail");
    assert!(err.contains("path"), "got: {err}");

    assert!(
        registry.validate("read", &json!({"path": 5})).is_ok(),
        "present key is structurally valid"
    );
}

#[tokio::test]
async fn registry_rejects_unknown_tool() {
    let registry = ToolRegistry::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    let outcome = registry.execute("nope", &json!({}), cancel).await;
    assert!(outcome.is_error);
    assert!(outcome.output.contains("unknown tool"));
}

#[tokio::test]
async fn tool_result_classifies_ok_and_err() {
    let ok = crate::tool::ToolResult::Ok {
        call_id: 1,
        output: "hi".into(),
    };
    let err = crate::tool::ToolResult::Err {
        call_id: 2,
        error: "boom".into(),
    };
    assert_eq!(ok.call_id(), 1);
    assert!(ok.is_ok());
    assert_eq!(err.call_id(), 2);
    assert!(!err.is_ok());
    assert_eq!(
        crate::tool::ToolResult::Ok {
            call_id: 3,
            output: "x".into()
        }
        .into_text(),
        "x"
    );
    assert_eq!(err.into_text(), "boom");
}

#[test]
fn tool_spec_is_clonable_and_comparable() {
    let a = ToolSpec {
        name: "read".into(),
        description: "d".into(),
        input_schema: json!({"type":"object"}),
    };
    let b = a.clone();
    assert_eq!(a, b);
}

// ---- Tool execution (read/write/edit/search/find/bash) unit tests ----

#[tokio::test]
async fn read_write_edit_search_find_roundtrip() {
    let tmp = std::env::temp_dir().join(format!("ion-tool-test-{}-{}", std::process::id(), 1));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::create_dir_all(&tmp);
    let registry = ToolRegistry::with_cwd(&tmp);
    let cancel = tokio_util::sync::CancellationToken::new();

    // write creates parent dirs and content
    let out = registry
        .execute(
            "write",
            &json!({"path":"sub/note.txt","contents":"hello world"}),
            cancel.clone(),
        )
        .await;
    assert!(!out.is_error, "write failed: {out:?}");
    assert_eq!(out.output, "written");

    // read returns the content
    let out = registry
        .execute("read", &json!({"path":"sub/note.txt"}), cancel.clone())
        .await;
    assert!(!out.is_error, "read failed: {out:?}");
    assert_eq!(out.output, "hello world");

    // edit replaces the first occurrence
    let out = registry
        .execute(
            "edit",
            &json!({"path":"sub/note.txt","old_str":"world","new_str":"ion"}),
            cancel.clone(),
        )
        .await;
    assert!(!out.is_error, "edit failed: {out:?}");
    let out = registry
        .execute("read", &json!({"path":"sub/note.txt"}), cancel.clone())
        .await;
    assert_eq!(out.output, "hello ion");

    // edit of a missing substring fails
    let out = registry
        .execute(
            "edit",
            &json!({"path":"sub/note.txt","old_str":"zzz","new_str":"x"}),
            cancel.clone(),
        )
        .await;
    assert!(out.is_error);
    assert!(out.output.contains("not found"));

    // search finds the pattern in the file
    let out = registry
        .execute("search", &json!({"pattern":"hello"}), cancel.clone())
        .await;
    assert!(!out.is_error, "search failed: {out:?}");
    assert!(out.output.contains("note.txt"), "got: {out:?}");

    // find matches a glob
    let out = registry
        .execute("find", &json!({"pattern":"*.txt"}), cancel.clone())
        .await;
    assert!(!out.is_error, "find failed: {out:?}");
    assert!(out.output.contains("note.txt"), "got: {out:?}");

    // path escape is refused
    let out = registry
        .execute("read", &json!({"path":"../outside.txt"}), cancel.clone())
        .await;
    assert!(out.is_error);
    assert!(out.output.contains("escapes"), "got: {out:?}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[tokio::test]
async fn bash_runs_command_and_reports_nonzero_exit() {
    let registry = ToolRegistry::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    let out = registry
        .execute("bash", &json!({"command":"echo hi"}), cancel.clone())
        .await;
    assert!(!out.is_error, "bash failed: {out:?}");
    assert!(out.output.contains("hi"), "got: {out:?}");

    let out = registry
        .execute("bash", &json!({"command":"exit 3"}), cancel.clone())
        .await;
    assert!(out.is_error, "nonzero exit should error");
    assert!(out.output.contains("3"), "got: {out:?}");
}

#[tokio::test]
async fn bash_cancel_kills_long_running_command() {
    let registry = ToolRegistry::default();
    let cancel = tokio_util::sync::CancellationToken::new();
    let tool = registry.get("bash");
    let _ = tool; // just ensure the tool is registered
    let cancel_clone = cancel.clone();
    let handle = tokio::spawn(async move {
        registry
            .execute("bash", &json!({"command":"sleep 30"}), cancel_clone)
            .await
    });
    sleep(STEP).await;
    cancel.cancel();
    let outcome = timeout(Duration::from_secs(3), handle)
        .await
        .expect("tool should be killed on cancel")
        .expect("task join");
    assert!(outcome.is_error);
    assert_eq!(outcome.output, "cancelled");
}

// ---- Tool loop integration tests ----

#[tokio::test]
async fn tool_loop_success_feeds_result_and_completes() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("bash", json!({"command":"echo tool-said-hello"})),
        ScriptedMessage::text("final answer\n"),
    ]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let handle = runtime.handle();
    let (_snapshot, mut events) = handle.subscribe().await.expect("subscribe");
    let turn_id = handle.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");

    // The tool's output is streamed as an assistant delta, then the final
    // text, then the turn finishes.
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::TurnFinished { turn_id: id, .. }) if *id == turn_id
    ));
    let joined = texts(&recorded).join("");
    assert!(
        joined.contains("tool-said-hello"),
        "tool output must reach the model stream, got: {joined}"
    );
    assert!(joined.contains("final answer"));
    // No failure/cancel events.
    assert!(recorded.iter().all(|e| !matches!(
        e,
        RuntimeEvent::TurnFailed { .. } | RuntimeEvent::TurnCancelled { .. }
    )));

    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn tool_loop_failure_propagates_as_turn_failed() {
    // Read a file that does not exist -> tool errors -> provider reports Failed.
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool(
        "read",
        json!({"path":"definitely-not-here.txt"}),
    )]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let handle = runtime.handle();
    let (_snapshot, mut events) = handle.subscribe().await.expect("subscribe");
    let turn_id = handle.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::TurnFailed { turn_id: id, .. }) if *id == turn_id
    ));
    let msg = failure_message(&recorded).expect("failure message");
    assert!(
        msg.contains("read failed") || msg.contains("No such file"),
        "got: {msg}"
    );

    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn tool_loop_malformed_args_fail_cleanly() {
    // Missing the required `path` argument -> validation error -> Failed.
    let provider =
        ScriptedProvider::new(vec![ScriptedMessage::tool("read", json!({"bogus": true}))]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let handle = runtime.handle();
    let (_snapshot, mut events) = handle.subscribe().await.expect("subscribe");
    let turn_id = handle.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::TurnFailed { turn_id: id, .. }) if *id == turn_id
    ));
    let msg = failure_message(&recorded).expect("failure message");
    assert!(
        msg.contains("path"),
        "malformed-args message should name the arg, got: {msg}"
    );

    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn tool_loop_unknown_tool_fails_cleanly() {
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool("frobnicate", json!({}))]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let handle = runtime.handle();
    let (_snapshot, mut events) = handle.subscribe().await.expect("subscribe");
    let turn_id = handle.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::TurnFailed { turn_id: id, .. }) if *id == turn_id
    ));
    let msg = failure_message(&recorded).expect("failure message");
    assert!(msg.contains("unknown tool"), "got: {msg}");

    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_during_tool_cancels_turn_and_kills_process() {
    // Provider requests a slow bash command; we cancel while it is running.
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool(
        "bash",
        json!({"command":"sleep 30 && echo PWNED"}),
    )]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let handle = runtime.handle();
    let (_snapshot, mut events) = handle.subscribe().await.expect("subscribe");
    let turn_id = handle.submit("go").await.expect("submit");

    // Wait until the tool call has actually been dispatched (turn is running).
    let mut dispatched = false;
    while !dispatched {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::TurnStarted { .. }) {
            dispatched = true;
        }
    }
    // Give the controller a moment to spawn the bash child before cancelling.
    sleep(STEP * 4).await;

    handle.cancel(turn_id).await.expect("cancel");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");

    // The turn must be cancelled (not finished) and the tool's output must
    // never reach the model stream.
    assert!(
        recorded.iter().any(
            |e| matches!(e, RuntimeEvent::TurnCancelled { turn_id: id, .. } if *id == turn_id)
        )
    );
    assert!(!recorded.iter().any(|e| matches!(
        e,
        RuntimeEvent::AssistantTextDelta { text, .. } if text.contains("PWNED")
    )));

    // Shutdown must join cleanly (the killed bash child must not leak).
    let shutdown = handle.shutdown().await;
    let join = timeout(Duration::from_secs(5), runtime.join());
    assert!(shutdown.is_ok(), "shutdown: {shutdown:?}");
    assert!(join.await.is_ok(), "runtime should join after cancel");
}

#[tokio::test]
async fn tool_loop_multiple_calls_share_one_turn() {
    let provider = ScriptedProvider::new(vec![
        ScriptedMessage::tool("bash", json!({"command":"echo one"})),
        ScriptedMessage::tool("bash", json!({"command":"echo two"})),
        ScriptedMessage::text("done\n"),
    ]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let handle = runtime.handle();
    let (_snapshot, mut events) = handle.subscribe().await.expect("subscribe");
    let turn_id = handle.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::TurnFinished { turn_id: id, .. }) if *id == turn_id
    ));
    let joined = texts(&recorded).join("");
    assert!(joined.contains("one"), "got: {joined}");
    assert!(joined.contains("two"), "got: {joined}");
    assert!(joined.contains("done"));

    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn provider_request_carries_tool_specs() {
    // The controller must hand the provider the tool list it can dispatch.
    // We assert this indirectly: a tool call in the script resolves, which
    // requires the registry to have been wired into the runtime.
    let provider = ScriptedProvider::new(vec![ScriptedMessage::tool(
        "read",
        json!({"path":"Cargo.toml"}),
    )]);
    let runtime = Runtime::start(provider, ToolRegistry::default());
    let handle = runtime.handle();
    let (_snapshot, mut events) = handle.subscribe().await.expect("subscribe");
    let turn_id = handle.submit("go").await.expect("submit");
    let recorded = collect_until_terminal(&mut events).await.expect("collect");
    assert!(matches!(
        recorded.last(),
        Some(RuntimeEvent::TurnFinished { turn_id: id, .. }) if *id == turn_id
    ));
    let joined = texts(&recorded).join("");
    assert!(
        joined.contains("ion"),
        "should read Cargo.toml, got: {joined}"
    );

    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}
