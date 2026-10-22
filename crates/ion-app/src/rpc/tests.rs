use super::*;
use crate::publication::ReplyCredits;
use ion_host::{ControlUpdate, Host, SavedSelection, Wire};
use std::{
    fs,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

#[path = "request_tests.rs"]
mod requests;

struct Fixture {
    control: Option<Control>,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ion-rpc-{}", uuid::Uuid::now_v7()));
        let cwd = root.join("work");
        fs::create_dir_all(&cwd).unwrap();
        let host = Arc::new(Host::new(root.join("config"), root.join("state")));
        let selected = host
            .models()
            .save_default(&SavedSelection {
                provider: "desktop".into(),
                model: "test".into(),
                endpoint: Some("http://127.0.0.1:9/v1/chat/completions".into()),
                wire: Some(Wire::ChatCompletions),
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: false,
            })
            .unwrap();
        let path = host.sessions(Some(cwd.clone())).new_path().unwrap();
        let session = Arc::new(ion_core::CodingSession::create(path, Some(&cwd)).unwrap());
        session.select_model(selected.identity()).unwrap();
        let setup = ion_host::CodingApplication::new(host.clone(), None)
            .setup(session, selected, None, None)
            .unwrap();
        Self {
            control: Some(Control::new(setup, host)),
            root,
        }
    }
    fn take(&mut self) -> Control {
        self.control.take().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn records(bytes: &[u8]) -> Vec<Value> {
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}
fn invoke(control: &mut Control, value: Value) -> Option<Value> {
    control
        .command(value, ReplyCredits::new(1).acquire().unwrap())
        .map(|(record, _)| record)
}
async fn started_shell(control: &mut Control, root: &std::path::Path) {
    let ready = root.join("ready");
    let command = format!("printf ready > '{}'; sleep 30", ready.display());
    assert_eq!(
        invoke(
            control,
            json!({"type":"shell","id":"shell","command":command})
        )
        .unwrap()["success"],
        true
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn prompt_ack_is_only_the_durable_host_accepted_update() {
    let mut fixture = Fixture::new();
    let mut control = fixture.take();
    assert!(
        invoke(
            &mut control,
            json!({"type":"prompt","id":"root","message":"literal input"})
        )
        .is_none()
    );
    let mut bytes = Vec::new();
    let mut output = Output::new(&mut bytes);
    loop {
        let update = control.host.next().await;
        let accepted = matches!(update, ControlUpdate::Accepted { .. });
        control.update(update, &mut output).unwrap();
        output.finish().await.unwrap();
        drop(output);
        assert!(
            accepted
                || !records(&bytes)
                    .iter()
                    .any(|record| record["command"] == "prompt" && record["success"] == true),
            "prompt success preceded durable acceptance"
        );
        output = Output::new(&mut bytes);
        if accepted {
            break;
        }
    }
    let report = control.host.shutdown().await;
    for update in report.updates {
        control.update(update, &mut output).unwrap();
    }
    output.finish().await.unwrap();
    drop(output);
    let records = records(&bytes);
    let acknowledgement = records
        .iter()
        .find(|record| record["command"] == "prompt" && record["success"] == true)
        .unwrap();
    assert_eq!(acknowledgement["id"], "root");
    assert_eq!(acknowledgement["data"]["turn"], 1);
    assert!(
        records
            .iter()
            .any(|record| record["type"] == "turn_end" && record["turn"] == 1)
    );
    assert!(
        !records
            .iter()
            .any(|record| record["type"] == "uncommitted_prompt")
    );
}

#[tokio::test]
async fn unaccepted_followup_settlement_retains_its_protocol_correlation() {
    let mut fixture = Fixture::new();
    let mut control = fixture.take();
    assert_eq!(
        invoke(
            &mut control,
            json!({"type":"shell","id":"shell","command":"true"})
        )
        .unwrap()["success"],
        true
    );
    assert!(
        invoke(
            &mut control,
            json!({"type":"follow_up","id":"queued","message":"never dispatched to a provider"})
        )
        .is_none()
    );
    // Close Core after the shell settles but before Host drives the queued
    // follow-up. Its attempted dispatch must fail before durable acceptance.
    tokio::time::timeout(Duration::from_secs(5), async {
        while control.host.session().ensure_idle().is_err() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    control.host.session().shutdown().await.unwrap();
    let mut bytes = Vec::new();
    let mut output = Output::new(&mut bytes);
    loop {
        let update = control.host.next().await;
        let idle = matches!(update, ControlUpdate::Idle);
        control.update(update, &mut output).unwrap();
        if idle {
            break;
        }
    }
    let report = control.host.shutdown().await;
    for update in report.updates {
        control.update(update, &mut output).unwrap();
    }
    assert!(report.cleanup_errors.is_empty());
    output.finish().await.unwrap();
    drop(output);
    let records = records(&bytes);
    assert!(
        records
            .iter()
            .any(|record| record["type"] == "uncommitted_follow_up" && record["id"] == "queued")
    );
    assert!(
        records
            .iter()
            .any(|record| record["type"] == "operation_failed"
                && record["id"] == "queued"
                && record["status"] == "failed")
    );
    assert!(
        !records
            .iter()
            .any(|record| record["type"] == "follow_up_started")
    );
}

#[tokio::test]
async fn original_typed_input_and_protocol_id_survive_clear_queue() {
    let mut fixture = Fixture::new();
    let mut control = fixture.take();
    started_shell(&mut control, &fixture.root).await;
    // Use the public Host admission directly to exercise RPC's Returned encoder
    // without manufacturing an execution slot or client-side input queue.
    let credit = ReplyCredits::new(1).acquire().unwrap();
    let id = Some(json!("literal"));
    let input = UserInput::Prompt {
        text: "/do not re-expand".into(),
        images: vec![],
    };
    let host_id = control
        .host
        .submit(
            Delivery::FollowUp,
            input.clone(),
            RpcMetadata {
                id: id.clone(),
                command: "follow_up",
                credit: credit.clone(),
            },
        )
        .unwrap();
    control.correlations.insert(
        host_id,
        Correlation {
            id,
            command: "follow_up",
            credit,
            acknowledged: false,
            position: 1,
            turn: None,
        },
    );
    control.host.clear_queue();
    control.host.request_stop();
    let report = control.host.shutdown().await;
    let mut bytes = Vec::new();
    let mut output = Output::new(&mut bytes);
    for update in report.updates {
        control.update(update, &mut output).unwrap();
    }
    output.finish().await.unwrap();
    drop(output);
    let records = records(&bytes);
    let returned = records
        .iter()
        .find(|record| record["type"] == "uncommitted_follow_up")
        .unwrap();
    assert_eq!(returned["id"], "literal");
    assert_eq!(returned["kind"], "follow_up");
    assert_eq!(returned["input"], serde_json::to_value(input).unwrap());
    assert_eq!(returned["reason"], "cleared");
}

#[tokio::test]
async fn context_navigation_shares_the_single_stalled_inspection_lane() {
    use std::future::Future;

    for (first, second) in [
        ("get_context", "get_context"),
        ("inspect", "get_context"),
        ("get_context", "inspect"),
    ] {
        let mut fixture = Fixture::new();
        let commands = format!(
            "{}\n{}\n",
            json!({"type":first,"id":"one"}),
            json!({"type":second,"id":"two"})
        );
        let (writer, mut reader) = tokio::io::duplex(1);
        let pending = connection::connection(
            fixture.take(),
            CommandReader::new(BufReader::new(commands.as_bytes())),
            Output::new(writer),
        );
        tokio::pin!(pending);
        // Process the ready commands before any consumer can release the first
        // reply's credit. Even an empty Session reply cannot fit this pipe.
        std::future::poll_fn(|context| {
            assert!(matches!(pending.as_mut().poll(context), Poll::Pending));
            Poll::Ready(())
        })
        .await;
        let reading = tokio::spawn(async move {
            let mut bytes = Vec::new();
            reader.read_to_end(&mut bytes).await.unwrap();
            bytes
        });
        pending.await.unwrap();
        let records = records(&reading.await.unwrap());
        let replies: Vec<_> = records
            .iter()
            .filter(|record| record["type"] == "response")
            .collect();
        assert_eq!(replies.len(), 1, "{first}/{second}: {records:?}");
        assert_eq!(replies[0]["id"], "one");
        assert_eq!(replies[0]["success"], true);
        assert_eq!(
            records
                .iter()
                .filter(|record| record["type"] == "publication_gap")
                .map(|record| record["commands_not_admitted"].as_u64().unwrap())
                .sum::<u64>(),
            1
        );
    }
}

#[tokio::test]
async fn stalled_inspection_does_not_block_abort_or_eof_join() {
    for abort in [false, true] {
        let mut fixture = Fixture::new();
        let mut control = fixture.take();
        started_shell(&mut control, &fixture.root).await;
        let session = control.host.session().clone();
        let (input, mut commands) = tokio::io::duplex(4096);
        let (writer, mut reader) = tokio::io::duplex(7);
        let large = json!({"type":"inspection","data":"x".repeat(1024 * 1024)});
        let mut output = Output::new(writer);
        output
            .enqueue_reliable(large.clone(), ReplyCredits::new(1).acquire().unwrap())
            .unwrap();
        let running = tokio::spawn(connection::connection(
            control,
            CommandReader::new(BufReader::new(input)),
            output,
        ));
        let mut bytes = vec![0; 7];
        reader.read_exact(&mut bytes).await.unwrap();
        if abort {
            for index in 0..40 {
                commands
                    .write_all(
                        format!("{{\"type\":\"get_state\",\"id\":\"state-{index}\"}}\n").as_bytes(),
                    )
                    .await
                    .unwrap();
            }
            commands
                .write_all(b"{\"type\":\"abort\",\"id\":\"stop\"}\n")
                .await
                .unwrap();
        } else {
            commands.shutdown().await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let view = session.view().unwrap();
                if view
                    .entries
                    .iter()
                    .any(|entry| matches!(entry, ion_core::SessionEntry::UserShellSettled { .. }))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("stdout pressure blocked native settlement");
        if abort {
            commands.shutdown().await.unwrap();
        }
        // The pressure proof is complete. Schedule both ends as tasks: driving
        // a seven-byte pipe from the runtime root adds driver-yield overhead to
        // every exchange and makes this deadline depend on scheduler throughput.
        let mut draining =
            tokio::spawn(async move { reader.read_to_end(&mut bytes).await.map(|_| bytes) });
        let drained = tokio::time::timeout(Duration::from_secs(5), &mut draining).await;
        if drained.is_err() {
            draining.abort();
            let _ = draining.await;
        }
        let mut running = running;
        let joined = tokio::time::timeout(Duration::from_secs(5), &mut running).await;
        if joined.is_err() {
            running.abort();
            let _ = running.await;
        }
        let bytes = drained
            .expect("RPC publication did not drain")
            .unwrap()
            .unwrap();
        joined
            .expect("RPC connection did not join")
            .unwrap()
            .unwrap();
        let records = records(&bytes);
        assert_eq!(
            records[0], large,
            "partial publication was duplicated or lost"
        );
        assert!(records.iter().any(|record| record["type"] == "shell_end"
            && record["outcome"]["output"]["cancelled"] == true));
        if abort {
            assert!(
                records
                    .iter()
                    .any(|record| record["id"] == "stop" && record["success"] == true)
            );
            assert!(
                records
                    .iter()
                    .filter(|record| record["command"] == "get_state")
                    .count()
                    <= connection::REPLY_SLOTS
            );
            assert!(
                records
                    .iter()
                    .filter(|record| record["type"] == "publication_gap")
                    .map(|record| record["commands_not_admitted"].as_u64().unwrap())
                    .sum::<u64>()
                    >= 8
            );
        }
    }
}

struct FailedInput;
impl AsyncRead for FailedInput {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut TaskContext<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::Error::other("injected RPC read failure")))
    }
}
struct FailedOutput(Arc<AtomicUsize>);
impl AsyncWrite for FailedOutput {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut TaskContext<'_>,
        _: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Err(std::io::Error::other("injected RPC write failure")))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[tokio::test]
async fn actual_io_failures_join_native_work_and_preserve_healthy_recovery() {
    for write_failure in [false, true] {
        let mut fixture = Fixture::new();
        let mut control = fixture.take();
        started_shell(&mut control, &fixture.root).await;
        let session = control.host.session().clone();
        if write_failure {
            let calls = Arc::new(AtomicUsize::new(0));
            let mut output = Output::new(FailedOutput(calls.clone()));
            output.enqueue(json!({"type":"progress"})).unwrap();
            let (input, _open_client) = tokio::io::duplex(64);
            let error =
                connection::connection(control, CommandReader::new(BufReader::new(input)), output)
                    .await
                    .unwrap_err();
            assert!(format!("{error:#}").contains("injected RPC write failure"));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        } else {
            let mut bytes = Vec::new();
            let error = connection::connection(
                control,
                CommandReader::new(BufReader::new(FailedInput)),
                Output::new(&mut bytes),
            )
            .await
            .unwrap_err();
            assert!(format!("{error:#}").contains("injected RPC read failure"));
            assert!(
                records(&bytes)
                    .iter()
                    .any(|record| record["type"] == "shell_end")
            );
        }
        assert!(
            session
                .view()
                .unwrap()
                .entries
                .iter()
                .any(|entry| matches!(entry, ion_core::SessionEntry::UserShellSettled { .. }))
        );
    }
}

#[tokio::test]
async fn passive_overflow_discards_suffix_without_stopping_core() {
    let mut fixture = Fixture::new();
    let mut control = fixture.take();
    started_shell(&mut control, &fixture.root).await;
    let mut bytes = Vec::new();
    let mut output = Output::new(&mut bytes);
    output
        .enqueue(json!({"text":"x".repeat(8 * 1024 * 1024)}))
        .unwrap();
    let operation = control.host.current_operation().unwrap();
    for text in ["lost prefix", "must not splice suffix"] {
        control
            .update(
                ControlUpdate::Activity(ion_host::Observation::Event {
                    operation,
                    event: Arc::new(ion_core::CodingAgentEvent::TextDelta(text.into())),
                }),
                &mut output,
            )
            .unwrap();
    }
    assert!(control.suppress_activity);
    // Publication saturation has not requested stop; the actual shell remains
    // owned and busy until this explicit foreground shutdown.
    assert!(control.host.is_busy());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(
        !control
            .host
            .session()
            .view()
            .unwrap()
            .entries
            .iter()
            .any(|entry| matches!(entry, ion_core::SessionEntry::UserShellSettled { .. })),
        "passive pressure stopped the native operation"
    );
    let report = control.host.shutdown().await;
    for update in report.updates {
        control.update(update, &mut output).unwrap();
    }
    output.finish().await.unwrap();
    control.publish_gap(&mut output).unwrap();
    output.finish().await.unwrap();
    drop(output);
    let records = records(&bytes);
    assert!(!records.iter().any(|record| record["type"] == "text_delta"));
    assert_eq!(
        records
            .iter()
            .filter(|record| record["type"] == "publication_gap")
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .find(|record| record["type"] == "shell_end")
            .unwrap()["progress_complete"],
        false
    );
}
