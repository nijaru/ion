use std::time::Duration;

use tokio::time::{sleep, timeout};

use crate::error::{CommandError, RuntimeError};
use crate::provider::{ScriptedChunk, ScriptedProvider};
use crate::runtime::{Runtime, RuntimeEvent, SaturatedHandle, TurnStatus};

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

#[tokio::test]
async fn streams_scripted_text_in_order() {
    let runtime = Runtime::start(ScriptedProvider::new(vec![
        ScriptedChunk::immediate("hel"),
        ScriptedChunk::immediate("lo"),
    ]));
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
    assert_eq!(
        recorded
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::AssistantTextDelta { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>(),
        ["hel", "lo"]
    );
    let cursors: Vec<_> = recorded.iter().map(RuntimeEvent::cursor).collect();
    assert!(cursors.windows(2).all(|pair| pair[0] < pair[1]));

    handle.shutdown().await.expect("shutdown");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn cancel_stops_provider_before_later_chunks() {
    let runtime = Runtime::start(ScriptedProvider::new(vec![
        ScriptedChunk::immediate("one"),
        ScriptedChunk::after(Duration::from_secs(30), "two"),
    ]));
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
    let runtime = Runtime::start(ScriptedProvider::new(vec![ScriptedChunk::after(
        Duration::from_secs(30),
        "later",
    )]));
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
    let runtime = Runtime::start(ScriptedProvider::echo());
    let handle = runtime.handle();
    handle.shutdown().await.expect("shutdown");
    let err = handle.submit("after").await.expect_err("closed");
    assert_eq!(err, CommandError::Closed);
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn print_frontend_writes_streamed_text() {
    let runtime = Runtime::start(ScriptedProvider::new(vec![
        ScriptedChunk::immediate("hel"),
        ScriptedChunk::immediate("lo\n"),
    ]));
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
    let runtime = Runtime::start(ScriptedProvider::new(vec![ScriptedChunk::after(
        Duration::from_secs(30),
        "late",
    )]));
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
