//! Session surface tests: publication authority, auxiliary loss and crash ordering.
use super::*;
use std::{
    fs,
    io::{self, Read},
    path::Path,
    time::Duration,
};

#[cfg(unix)]
#[path = "artifact_process_loss.rs"]
mod process_loss;

const CONTENT: &[u8] = b"complete tool output, not automatically placed in model context";

#[derive(Clone)]
enum Output {
    Publish,
    Forge(BlobRef),
    Fault,
}

struct ArtifactTool {
    base: Tool,
    output: Output,
    saved: Mutex<Option<(ArtifactPublisher, BlobRef)>>,
    published: tokio::sync::Notify,
    release: tokio::sync::Notify,
    wait: bool,
    crash_marker: Option<PathBuf>,
}

impl ArtifactTool {
    fn new(output: Output) -> Arc<Self> {
        Arc::new(Self {
            base: Tool::new(success()),
            output,
            saved: Mutex::new(None),
            published: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            wait: false,
            crash_marker: None,
        })
    }
    fn boundaries(self: &Arc<Self>) -> ToolBoundaries {
        ToolBoundaries::new([self.clone() as Arc<dyn ToolBoundary>]).unwrap()
    }
}

struct FailedSource(bool);
impl Read for FailedSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.0 {
            return Err(io::Error::other("injected source loss after partial spool"));
        }
        self.0 = true;
        buffer[..4].copy_from_slice(b"part");
        Ok(4)
    }
}

impl ToolBoundary for ArtifactTool {
    fn binding(&self) -> ToolBinding {
        self.base.binding()
    }
    fn executor(&self) -> SemanticCompatibilityId {
        self.base.executor()
    }
    fn prepare(&self, arguments: serde_json::Value) -> Result<PreparedAction, ToolBoundaryError> {
        self.base.prepare(arguments)
    }
    fn live_authority(&self, _: &PreparedAction, _: &WorkspaceBinding) -> LiveToolAuthority {
        LiveToolAuthority::Allow
    }
    fn execute<'a>(
        &'a self,
        execution: ToolExecution,
        _: CancellationToken,
    ) -> BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move {
            self.base.executes.fetch_add(1, Ordering::SeqCst);
            let reference = match &self.output {
                Output::Publish => execution.artifacts.publish(CONTENT).await,
                Output::Forge(reference) => Ok(reference.clone()),
                Output::Fault => execution.artifacts.publish(FailedSource(false)).await,
            };
            let capture = match reference {
                Ok(reference) => {
                    *self.saved.lock().unwrap() =
                        Some((execution.artifacts.clone(), reference.clone()));
                    if let Some(marker) = &self.crash_marker {
                        fs::write(
                            marker,
                            serde_json::to_vec(&(execution.attempt, &reference)).unwrap(),
                        )
                        .unwrap();
                        std::future::pending::<()>().await;
                    }
                    OutputCapture::CompleteArtifact {
                        full_output: reference,
                    }
                }
                Err(ArtifactError::Storage(BlobStoreError::QuotaExceeded { .. })) => {
                    OutputCapture::Incomplete {
                        reason: OutputLoss::Quota,
                        retained_bytes: 0,
                        observed_bytes: None,
                    }
                }
                Err(_) => OutputCapture::Incomplete {
                    reason: OutputLoss::BackendCapacity,
                    retained_bytes: 0,
                    observed_bytes: None,
                },
            };
            self.published.notify_one();
            if self.wait {
                self.release.notified().await;
            }
            ToolAttemptState::Settled {
                result: ToolResult {
                    value: json!("bounded preview"),
                    is_error: false,
                    capture,
                },
                effect: EffectSummary::KnownChanges {
                    paths: vec!["effect-remains-known".into()],
                },
                receipt: Some(StartReceipt {
                    kind: "joined".into(),
                    data: json!(1),
                }),
                retryable: false,
            }
        })
    }
}

fn namespace(path: &Path, session: SessionId) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".blobs-{session}"));
    PathBuf::from(name)
}

fn result(attempt: &ToolAttempt) -> &ToolResult {
    let ToolAttemptState::Settled {
        result,
        effect,
        receipt,
        retryable,
    } = &attempt.state
    else {
        panic!("terminal effect lost")
    };
    assert_eq!(
        effect,
        &EffectSummary::KnownChanges {
            paths: vec!["effect-remains-known".into()]
        }
    );
    assert!(receipt.is_some());
    assert!(!retryable);
    result
}

async fn run(session: &Session, turn: TurnId, tool: &Arc<ArtifactTool>) -> ToolRecords {
    let exit = session
        .handle()
        .resume_with_tools(
            turn,
            models(&model()),
            tool.boundaries(),
            DrivePolicy::default(),
        )
        .await
        .unwrap();
    assert!(
        matches!(exit, DriveExit::Settled(TurnOutcome::Completed { .. })),
        "{exit:?}"
    );
    session
        .handle()
        .tool_records(tool_step(session).await)
        .await
        .unwrap()
}

#[tokio::test]
async fn artifact_commit_reopen_pages_and_auxiliary_loss_preserve_truth() {
    let (session, path, turn) = setup(config()).await;
    let root = namespace(&path, session.session_id());
    assert!(!root.exists(), "Session creation does not spool anything");
    let tool = ArtifactTool::new(Output::Publish);
    let records = run(&session, turn, &tool).await;
    let attempt = &records.attempts[0];
    let OutputCapture::CompleteArtifact { full_output } = &result(attempt).capture else {
        panic!("missing artifact")
    };
    let expected = full_output.clone();
    let (stale, _) = tool.saved.lock().unwrap().clone().unwrap();
    assert!(matches!(
        stale.publish(CONTENT).await,
        Err(ArtifactError::PublisherClosed)
    ));
    assert!(
        matches!(session.handle().read_artifact(attempt.id, 9, 4).await.unwrap(), ArtifactRead::Page { bytes, .. } if bytes == CONTENT[9..13])
    );
    assert!(
        session
            .handle()
            .read_artifact(attempt.id, 0, 65 * 1024)
            .await
            .is_err()
    );
    assert_eq!(
        session
            .handle()
            .collect_artifacts()
            .await
            .unwrap()
            .object_count,
        0
    );
    let step = tool_step(&session).await;
    let entries = session
        .handle()
        .page_entries(session.primary_conversation(), None, 100)
        .await
        .unwrap();
    let old_handle = session.handle();
    session.close().await.unwrap();
    // Neither dormant Session handles nor escaped publishers retain the namespace lock.
    let session = Session::open(&path).await.unwrap();
    assert_eq!(old_handle.health(), SessionHealth::Closed);
    fs::write(root.join("staging/crash-leftover"), b"partial").unwrap();
    session.close().await.unwrap();
    fs::write(
        root.join("objects").join(expected.digest.to_string()),
        vec![b'x'; CONTENT.len()],
    )
    .unwrap();
    let session = Session::open(&path).await.unwrap();
    assert!(root.join("staging/crash-leftover").exists());
    assert_eq!(session.handle().tool_records(step).await.unwrap(), records);
    assert_eq!(
        session
            .handle()
            .page_entries(session.primary_conversation(), None, 100)
            .await
            .unwrap(),
        entries
    );
    assert!(matches!(
        session
            .handle()
            .read_artifact(attempt.id, 0, 10)
            .await
            .unwrap(),
        ArtifactRead::ContentUnavailable { .. }
    ));
    assert_eq!(session.health(), SessionHealth::Open);
    // Entire auxiliary namespace loss is also inspectable on passive reopen.
    session.close().await.unwrap();
    fs::remove_dir_all(&root).unwrap();
    let session = Session::open(&path).await.unwrap();
    assert!(!root.exists());
    assert!(matches!(
        session
            .handle()
            .read_artifact(attempt.id, 0, 10)
            .await
            .unwrap(),
        ArtifactRead::ContentUnavailable { .. }
    ));
    assert_eq!(session.handle().tool_records(step).await.unwrap(), records);
    assert_eq!(session.health(), SessionHealth::Open);
    session.close().await.unwrap();
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn artifact_forged_cross_session_and_stale_refs_never_publish() {
    let (session, path, turn) = setup(config()).await;
    let source = ArtifactTool::new(Output::Publish);
    let records = run(&session, turn, &source).await;
    let reference = source.saved.lock().unwrap().as_ref().unwrap().1.clone();
    let root = namespace(&path, session.session_id());
    // A second Session cannot launder the source's durable reference into evidence.
    let (other, other_path, other_turn) = setup(config()).await;
    let forged = ArtifactTool::new(Output::Forge(reference.clone()));
    let rejected = run(&other, other_turn, &forged).await;
    assert!(matches!(
        result(&rejected.attempts[0]).capture,
        OutputCapture::Incomplete {
            reason: OutputLoss::BackendCapacity,
            ..
        }
    ));
    assert!(matches!(
        other
            .handle()
            .read_artifact(rejected.attempts[0].id, 0, 10)
            .await,
        Err(SessionError::NotFound { .. })
    ));
    other.close().await.unwrap();
    fs::remove_file(other_path).unwrap();

    // Same namespace, old attempt's ref: even existing verified content is not proof.
    // Use another conversation so the scripted provider produces a new tool call.
    let conversation = session
        .handle()
        .create_conversation(config())
        .await
        .unwrap()
        .conversation
        .id;
    let input = session
        .handle()
        .admit_input(
            conversation,
            AdmitInputRequest {
                sender: InputSender::User,
                mode: InputMode::Submit,
                request_key: None,
                body: InputBody::Text("again".into()),
            },
        )
        .await
        .unwrap();
    let turn = session
        .handle()
        .start_turn(StartTurnRequest {
            conversation,
            input: input.input().id,
            admitted_at_unix_ms: 0,
            wall_deadline_unix_ms: None,
        })
        .await
        .unwrap()
        .turn
        .id;
    let stale = ArtifactTool::new(Output::Forge(reference));
    session
        .handle()
        .resume_with_tools(
            turn,
            models(&model()),
            stale.boundaries(),
            DrivePolicy::default(),
        )
        .await
        .unwrap();
    let entries = session
        .handle()
        .page_entries(conversation, None, 100)
        .await
        .unwrap();
    let step = entries
        .entries
        .iter()
        .find_map(|e| {
            if let EntryData::Assistant { step } = e.data {
                Some(step)
            } else {
                None
            }
        })
        .unwrap();
    let stale_records = session.handle().tool_records(step).await.unwrap();
    assert!(matches!(
        result(&stale_records.attempts[0]).capture,
        OutputCapture::Incomplete { .. }
    ));
    assert_eq!(
        session
            .handle()
            .tool_records(records.invocations[0].step)
            .await
            .unwrap(),
        records
    );
    session.close().await.unwrap();
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn artifact_quota_and_partial_spool_failure_preserve_terminal_effect() {
    for output in [Output::Publish, Output::Fault] {
        let (session, path, turn) = setup(config()).await;
        let root = namespace(&path, session.session_id());
        session.close().await.unwrap();
        let session = Session::open_with_blob_limits(&path, BlobStoreLimits::new(8, 8, 8, 8, 1))
            .await
            .unwrap();
        let tool = ArtifactTool::new(output.clone());
        let records = run(&session, turn, &tool).await;
        let expected = if matches!(output, Output::Publish) {
            OutputLoss::Quota
        } else {
            OutputLoss::BackendCapacity
        };
        assert!(
            matches!(result(&records.attempts[0]).capture, OutputCapture::Incomplete { reason, .. } if reason == expected)
        );
        assert_eq!(tool.base.executes.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read_dir(root.join("objects")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(root.join("staging")).unwrap().count(), 0);
        assert_eq!(session.health(), SessionHealth::Open);
        session.close().await.unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(path).unwrap();
    }
}

#[tokio::test]
async fn artifact_gc_waits_for_evidence_after_resume_waiter_is_dropped() {
    let (session, path, turn) = setup(config()).await;
    let root = namespace(&path, session.session_id());
    let mut tool = ArtifactTool::new(Output::Publish);
    Arc::get_mut(&mut tool).unwrap().wait = true;
    let handle = session.handle();
    let boundaries = tool.boundaries();
    let drive = tokio::spawn(async move {
        handle
            .resume_with_tools(turn, models(&model()), boundaries, DrivePolicy::default())
            .await
    });
    tool.published.notified().await;
    drive.abort();
    assert!(drive.await.unwrap_err().is_cancelled());
    let h = session.handle();
    let mut gc = Box::pin(h.collect_artifacts());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut gc)
            .await
            .is_err()
    );
    tool.release.notify_one();
    let removed = tokio::time::timeout(Duration::from_secs(5), gc)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        removed.object_count, 0,
        "queued evidence owns the gate, not the dropped waiter"
    );
    let step = tool_step(&session).await;
    let records = session.handle().tool_records(step).await.unwrap();
    assert!(matches!(
        result(&records.attempts[0]).capture,
        OutputCapture::CompleteArtifact { .. }
    ));
    session.close().await.unwrap();
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn artifact_evidence_commit_fault_leaves_only_collectable_orphan() {
    let (session, path, turn) = setup(config()).await;
    let root = namespace(&path, session.session_id());
    let mut tool = ArtifactTool::new(Output::Publish);
    Arc::get_mut(&mut tool).unwrap().wait = true;
    let h = session.handle();
    let boundaries = tool.boundaries();
    let drive = tokio::spawn(async move {
        h.resume_with_tools(turn, models(&model()), boundaries, DrivePolicy::default())
            .await
    });
    tool.published.notified().await;
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TRIGGER fail_artifact BEFORE UPDATE ON tool_attempts BEGIN SELECT RAISE(ABORT, 'injected evidence failure'); END;").unwrap();
    tool.release.notify_one();
    assert!(matches!(
        drive.await.unwrap().unwrap(),
        DriveExit::Faulted { .. }
    ));
    assert_eq!(session.health(), SessionHealth::Fenced);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM tool_artifacts", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        0
    );
    let step = tool_step(&session).await;
    let records = session.handle().tool_records(step).await.unwrap();
    assert!(matches!(
        records.attempts[0].state,
        ToolAttemptState::IntentCommitted { .. }
    ));
    assert!(session.handle().collect_artifacts().await.is_err());
    connection
        .execute_batch("DROP TRIGGER fail_artifact")
        .unwrap();
    drop(connection);
    session.close().await.unwrap();
    let session = Session::open(&path).await.unwrap();
    assert_eq!(fs::read_dir(root.join("objects")).unwrap().count(), 1);
    assert_eq!(
        session
            .handle()
            .collect_artifacts()
            .await
            .unwrap()
            .object_count,
        1
    );
    assert_eq!(session.handle().tool_records(step).await.unwrap(), records);
    session.close().await.unwrap();
    fs::remove_dir_all(root).unwrap();
    fs::remove_file(path).unwrap();
}
