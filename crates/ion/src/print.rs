//! Print frontend: projects session events to a writer (DESIGN.md §21.1).
//! One frontend over the runtime contract; it owns no agent truth.

use std::io::Write;

use ion_core::{
    CommandError, EventSubscription, OperationId, OperationOutcome, OperationStatus, RuntimeError,
    RuntimeEvent, SessionEntry, SessionHandle, SessionSnapshot,
};

pub struct PrintFrontend<W> {
    writer: W,
}

impl<W: Write> PrintFrontend<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }

    pub async fn run(
        &mut self,
        session: &SessionHandle,
        prompt: impl Into<String>,
    ) -> Result<(), RuntimeError> {
        let (snapshot, events) = session.subscribe().await?;
        let history_baseline = print_text(&snapshot);
        let operation_id = session.submit_if_idle(prompt).await?;
        self.run_operation(session, events, operation_id, history_baseline)
            .await
    }

    async fn run_operation(
        &mut self,
        session: &SessionHandle,
        mut events: EventSubscription,
        operation_id: OperationId,
        history_baseline: String,
    ) -> Result<(), RuntimeError> {
        let mut emitted = String::new();
        loop {
            let event = match events.recv().await {
                Ok(event) => event,
                Err(RuntimeError::SubscriptionLagged) => {
                    let (snapshot, fresh) = session.subscribe().await?;
                    events = fresh;
                    self.reconstruct_after_lag(&snapshot, &history_baseline, &mut emitted)?;
                    if let Some(result) = settlement_result(&snapshot, operation_id) {
                        return result;
                    }
                    match &snapshot.operation {
                        OperationStatus::Active {
                            operation_id: active,
                            ..
                        } if *active == operation_id => continue,
                        OperationStatus::Active { .. } => {
                            return Err(RuntimeError::OperationFailed(
                                "print event stream resynchronized to a different active operation"
                                    .to_owned(),
                            ));
                        }
                        OperationStatus::Idle => {
                            return Err(RuntimeError::OperationFailed(
                                "print event stream lagged; operation settlement unavailable"
                                    .to_owned(),
                            ));
                        }
                    }
                }
                Err(err) => return Err(err),
            };
            if event.operation_id().is_some_and(|id| id != operation_id) {
                continue;
            }
            match event {
                RuntimeEvent::AssistantTextDelta { text, .. } => {
                    self.write_text(&text)?;
                    emitted.push_str(&text);
                }
                // Tool settlement and usage are durable-state news, not
                // print-mode output.
                RuntimeEvent::ToolStarted { .. }
                | RuntimeEvent::ToolProgress { .. }
                | RuntimeEvent::ToolSettled { .. }
                | RuntimeEvent::UsageUpdate { .. }
                // User shell passthrough output is not print-mode output;
                // the settled entry is durable session history.
                | RuntimeEvent::ShellStarted { .. }
                | RuntimeEvent::ShellOutput { .. }
                | RuntimeEvent::ShellSettled { .. } => {}
                // Print mode is quiet output only.
                RuntimeEvent::ThinkingDelta { .. } => {}
                RuntimeEvent::OperationFinished { .. } => return Ok(()),
                RuntimeEvent::OperationCancelled { .. } => {
                    return Err(RuntimeError::OperationCancelled);
                }
                RuntimeEvent::OperationApprovalRequired { tool, .. } => {
                    return Err(RuntimeError::ApprovalRequired { tool });
                }
                // Print mode is non-interactive, so a parked approval
                // cannot occur; fail closed the same way if it ever does.
                RuntimeEvent::ApprovalPending { tool, .. } => {
                    return Err(RuntimeError::ApprovalRequired { tool });
                }
                RuntimeEvent::OperationFailed { message, .. } => {
                    return Err(RuntimeError::OperationFailed(message));
                }
                RuntimeEvent::OperationIndeterminate { message, .. } => {
                    return Err(RuntimeError::OperationFailed(format!(
                        "indeterminate operation: {message}"
                    )));
                }
                RuntimeEvent::SessionClosed { .. } => {
                    return Err(RuntimeError::Command(CommandError::Closed));
                }
                RuntimeEvent::OperationStarted { .. } => {}
            }
        }
    }

    fn reconstruct_after_lag(
        &mut self,
        snapshot: &SessionSnapshot,
        history_baseline: &str,
        emitted: &mut String,
    ) -> Result<(), RuntimeError> {
        let authoritative = print_text(snapshot);
        let Some(current_turn) = authoritative.strip_prefix(history_baseline) else {
            return Err(RuntimeError::OperationFailed(
                "print event stream lagged after its session-history baseline changed; exact output reconstruction is unavailable"
                    .to_owned(),
            ));
        };
        let Some(missing) = current_turn.strip_prefix(emitted.as_str()) else {
            return Err(RuntimeError::OperationFailed(
                "print event stream lagged after non-durable partial output; exact output reconstruction is unavailable"
                    .to_owned(),
            ));
        };
        if !missing.is_empty() {
            self.write_text(missing)?;
            emitted.push_str(missing);
        }
        Ok(())
    }

    fn write_text(&mut self, text: &str) -> Result<(), RuntimeError> {
        self.writer
            .write_all(text.as_bytes())
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))?;
        self.writer
            .flush()
            .map_err(|err| RuntimeError::OperationFailed(err.to_string()))
    }
}

fn print_text(snapshot: &SessionSnapshot) -> String {
    let mut text = snapshot
        .entries
        .iter()
        .filter_map(|entry| match entry {
            SessionEntry::AssistantMessage { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    if let Some(live) = &snapshot.live {
        text.push_str(&live.draft_text);
    }
    text
}

fn settlement_result(
    snapshot: &SessionSnapshot,
    operation_id: OperationId,
) -> Option<Result<(), RuntimeError>> {
    let settlement = snapshot
        .latest_settlement
        .as_ref()
        .filter(|settlement| settlement.operation_id == operation_id)?;
    Some(match &settlement.outcome {
        OperationOutcome::Completed => Ok(()),
        OperationOutcome::Failed(message) => Err(RuntimeError::OperationFailed(message.clone())),
        OperationOutcome::Cancelled => Err(RuntimeError::OperationCancelled),
        OperationOutcome::Indeterminate => {
            let message = snapshot
                .indeterminate
                .as_ref()
                .filter(|warning| warning.operation_id == operation_id)
                .map_or("external effect outcome is indeterminate", |warning| {
                    warning.message.as_str()
                });
            Err(RuntimeError::OperationFailed(format!(
                "indeterminate operation: {message}"
            )))
        }
        OperationOutcome::ApprovalRequired { tool } => {
            Err(RuntimeError::ApprovalRequired { tool: tool.clone() })
        }
    })
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::time::Duration;

    use super::*;
    use ion_core::{
        EngineSignal, Provider, ProviderRequest, Runtime, ScriptedMessage, ScriptedProvider,
        SessionStore, ToolRegistry,
    };
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    #[derive(Clone, Copy)]
    struct BurstProvider {
        settle_delay: Duration,
    }

    impl Provider for BurstProvider {
        fn run(
            &self,
            request: ProviderRequest,
            _cancel: CancellationToken,
            out: mpsc::Sender<EngineSignal>,
        ) -> impl Future<Output = ()> + Send {
            let settle_delay = self.settle_delay;
            async move {
                for _ in 0..96 {
                    if out
                        .send(EngineSignal::TextDelta {
                            operation_id: request.operation_id,
                            step: request.step,
                            text: "x".to_owned(),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
                tokio::time::sleep(settle_delay).await;
                let _ = out
                    .send(EngineSignal::Completed {
                        operation_id: request.operation_id,
                        step: request.step,
                    })
                    .await;
            }
        }
    }

    async fn wait_for_snapshot(
        session: &SessionHandle,
        predicate: impl Fn(&SessionSnapshot) -> bool,
    ) -> SessionSnapshot {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = session.snapshot().await.expect("snapshot");
                if predicate(&snapshot) {
                    break snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("snapshot condition timed out")
    }

    #[tokio::test]
    async fn print_frontend_writes_streamed_text() {
        let store = SessionStore::open_in_memory().expect("in-memory store");
        let runtime = Runtime::start_with_store(
            ScriptedProvider::new(vec![
                ScriptedMessage::text("hel"),
                ScriptedMessage::text("lo\n"),
            ]),
            ToolRegistry::default(),
            store,
        );
        let session = runtime.session();
        let mut buf = Vec::new();
        PrintFrontend::new(&mut buf)
            .run(&session, "hi")
            .await
            .expect("print");
        assert_eq!(String::from_utf8(buf).expect("utf8"), "hello\n");
        session.close().await.expect("close");
        runtime.join().await.expect("join");
    }

    #[tokio::test]
    async fn lagged_print_reconstructs_completed_output_from_snapshot() {
        let runtime = Runtime::start_with_store(
            BurstProvider {
                settle_delay: Duration::ZERO,
            },
            ToolRegistry::default(),
            SessionStore::open_in_memory().expect("store"),
        );
        let session = runtime.session();
        let (_snapshot, events) = session.subscribe().await.expect("subscribe");
        let operation_id = session.submit_if_idle("go").await.expect("submit");
        wait_for_snapshot(&session, |snapshot| {
            snapshot
                .latest_settlement
                .as_ref()
                .is_some_and(|settlement| {
                    settlement.operation_id == operation_id
                        && settlement.outcome == OperationOutcome::Completed
                })
        })
        .await;

        let mut output = Vec::new();
        PrintFrontend::new(&mut output)
            .run_operation(&session, events, operation_id, String::new())
            .await
            .expect("lagged print should reconstruct");
        assert_eq!(output, vec![b'x'; 96]);

        session.close().await.expect("close");
        runtime.join().await.expect("join");
    }

    #[tokio::test]
    async fn lagged_print_reconstructs_only_current_turn_after_prior_history() {
        let runtime = Runtime::start_with_store(
            BurstProvider {
                settle_delay: Duration::ZERO,
            },
            ToolRegistry::default(),
            SessionStore::open_in_memory().expect("store"),
        );
        let session = runtime.session();

        let mut first_output = Vec::new();
        PrintFrontend::new(&mut first_output)
            .run(&session, "first")
            .await
            .expect("first turn");
        assert_eq!(first_output, vec![b'x'; 96]);

        let (snapshot, events) = session.subscribe().await.expect("subscribe");
        let history_baseline = print_text(&snapshot);
        assert_eq!(history_baseline.as_bytes(), vec![b'x'; 96]);
        let operation_id = session.submit_if_idle("second").await.expect("submit");
        wait_for_snapshot(&session, |snapshot| {
            snapshot
                .latest_settlement
                .as_ref()
                .is_some_and(|settlement| {
                    settlement.operation_id == operation_id
                        && settlement.outcome == OperationOutcome::Completed
                })
        })
        .await;

        let mut second_output = Vec::new();
        PrintFrontend::new(&mut second_output)
            .run_operation(&session, events, operation_id, history_baseline)
            .await
            .expect("lagged second turn should reconstruct");
        assert_eq!(second_output, vec![b'x'; 96]);

        session.close().await.expect("close");
        runtime.join().await.expect("join");
    }

    #[tokio::test]
    async fn lagged_print_reconstructs_live_draft_and_stays_attached() {
        let runtime = Runtime::start_with_store(
            BurstProvider {
                settle_delay: Duration::from_millis(100),
            },
            ToolRegistry::default(),
            SessionStore::open_in_memory().expect("store"),
        );
        let session = runtime.session();
        let (_snapshot, events) = session.subscribe().await.expect("subscribe");
        let operation_id = session.submit_if_idle("go").await.expect("submit");
        wait_for_snapshot(&session, |snapshot| {
            matches!(
                (&snapshot.operation, &snapshot.live),
                (
                    OperationStatus::Active { operation_id: active, .. },
                    Some(live)
                ) if *active == operation_id && live.draft_text.len() == 96
            )
        })
        .await;

        let mut output = Vec::new();
        PrintFrontend::new(&mut output)
            .run_operation(&session, events, operation_id, String::new())
            .await
            .expect("lagged active print should stay attached");
        assert_eq!(output, vec![b'x'; 96]);

        session.close().await.expect("close");
        runtime.join().await.expect("join");
    }
}
