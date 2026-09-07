//! Git checkpoint preparation is part of the first model effect's lifetime.
//! The worker captures tracked changes before invoking the provider; the
//! session writer publishes metadata before allowing that effect to proceed.

use super::*;
use std::path::Path;

pub(super) struct PreparedCheckpoint {
    operation_id: OperationId,
    step: u64,
    leaf: EntryId,
    reference: String,
    resume: oneshot::Sender<()>,
}

impl<P: Provider> SessionRuntime<P> {
    pub(super) async fn record_prepared_checkpoint(&mut self, checkpoint: PreparedCheckpoint) {
        let PreparedCheckpoint {
            operation_id,
            step,
            leaf,
            reference,
            resume,
        } = checkpoint;
        if self
            .live(operation_id)
            .is_none_or(|live| live.model_step != step)
            || self
                .active(operation_id)
                .is_none_or(|active| active.cancel.is_cancelled())
        {
            return;
        }
        match self
            .store
            .record_checkpoint(self.session_id, leaf, reference)
            .await
        {
            Ok(()) => {
                let _ = resume.send(());
            }
            Err(error) => {
                self.fail_operation_on_persistence_for(operation_id, error)
                    .await;
            }
        }
    }
}

/// A failed capture skips this optional feature; a successfully captured
/// checkpoint must be durably published before model execution. Cancellation
/// covers capture, mailbox backpressure, and the writer acknowledgement.
pub(super) async fn prepare_before_model(
    cwd: &str,
    session_id: SessionId,
    operation_id: OperationId,
    step: u64,
    leaf: EntryId,
    cancel: &CancellationToken,
    output: &mpsc::Sender<PreparedCheckpoint>,
) -> bool {
    let prepare = async {
        let reference = match capture(Path::new(cwd), session_id, leaf).await {
            Ok(Some(reference)) => reference,
            Ok(None) => return true,
            Err(error) => {
                tracing::warn!(%operation_id, %error, "git checkpoint capture skipped");
                return true;
            }
        };
        let (resume, resumed) = oneshot::channel();
        if output
            .send(PreparedCheckpoint {
                operation_id,
                step,
                leaf,
                reference,
                resume,
            })
            .await
            .is_err()
        {
            return false;
        }
        resumed.await.is_ok()
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        ready = prepare => ready,
    }
}

/// `stash create` observes tracked files only and does not change the index,
/// worktree, or HEAD. Pinning the object prevents Git garbage collection from
/// invalidating the durable record. References have session/entry identity;
/// interrupted publication may leave a retained object, never a dangling
/// durable checkpoint. Concurrent external writers are outside this snapshot's
/// guarantee: it precedes this operation's provider work, not all workspace work.
async fn capture(
    cwd: &Path,
    session_id: SessionId,
    leaf: EntryId,
) -> Result<Option<String>, String> {
    let capture = async {
        let object = git(cwd, &["stash", "create"]).await?;
        let object = object.trim();
        if object.is_empty() {
            return Ok(None);
        }
        let reference = format!("refs/ion/checkpoints/{session_id}/{leaf}");
        git(cwd, &["update-ref", &reference, object]).await?;
        // Store the immutable object ID; the named ref exists only to retain it.
        Ok(Some(object.to_owned()))
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), capture)
        .await
        .map_err(|_| "capture timed out".to_owned())?
}

async fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.editor=:",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]).await.unwrap();
        git(dir.path(), &["config", "user.email", "ion@example.invalid"])
            .await
            .unwrap();
        git(dir.path(), &["config", "user.name", "Ion Test"])
            .await
            .unwrap();
        std::fs::write(dir.path().join("tracked"), "initial\n").unwrap();
        git(dir.path(), &["add", "tracked"]).await.unwrap();
        git(dir.path(), &["commit", "-qm", "initial"])
            .await
            .unwrap();
        dir
    }

    #[tokio::test]
    async fn capture_retains_tracked_state_without_changing_worktree_or_index() {
        let dir = repository().await;
        std::fs::write(dir.path().join("tracked"), "before model\n").unwrap();
        std::fs::write(dir.path().join("untracked"), "excluded\n").unwrap();
        let status = git(dir.path(), &["status", "--porcelain"]).await.unwrap();
        let session = SessionId::generate();
        let leaf = EntryId::generate();
        let object = capture(dir.path(), session, leaf).await.unwrap().unwrap();
        assert_eq!(
            git(dir.path(), &["status", "--porcelain"]).await.unwrap(),
            status
        );
        let reference = format!("refs/ion/checkpoints/{session}/{leaf}");
        assert_eq!(
            git(dir.path(), &["rev-parse", &reference])
                .await
                .unwrap()
                .trim(),
            object
        );
        git(dir.path(), &["gc", "--prune=now"]).await.unwrap();
        assert_eq!(
            git(dir.path(), &["show", &format!("{object}:tracked")])
                .await
                .unwrap(),
            "before model\n"
        );
        assert!(
            git(dir.path(), &["show", &format!("{object}:untracked")])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn preparation_waits_for_writer_and_is_cancellable() {
        let dir = repository().await;
        std::fs::write(dir.path().join("tracked"), "changed\n").unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let prepare = prepare_before_model(
            dir.path().to_str().unwrap(),
            SessionId::generate(),
            OperationId::generate(),
            1,
            EntryId::generate(),
            &cancel,
            &tx,
        );
        tokio::pin!(prepare);
        let checkpoint = tokio::select! {
            result = &mut prepare => panic!("finished before writer: {result}"),
            checkpoint = rx.recv() => checkpoint.unwrap(),
        };
        // Receipt alone is insufficient: the worker waits for durable publication.
        tokio::select! {
            biased;
            result = &mut prepare => panic!("finished before acknowledgement: {result}"),
            () = std::future::ready(()) => {}
        }
        cancel.cancel();
        assert!(!prepare.await);
        assert!(checkpoint.resume.send(()).is_err());
    }

    #[tokio::test]
    async fn preparation_resumes_after_writer_acknowledges() {
        let dir = repository().await;
        std::fs::write(dir.path().join("tracked"), "changed\n").unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let cancel = CancellationToken::new();
        let prepare = prepare_before_model(
            dir.path().to_str().unwrap(),
            SessionId::generate(),
            OperationId::generate(),
            1,
            EntryId::generate(),
            &cancel,
            &tx,
        );
        let writer = async {
            rx.recv().await.unwrap().resume.send(()).unwrap();
        };
        let (ready, ()) = tokio::join!(prepare, writer);
        assert!(ready);
    }
    struct CheckpointProvider {
        store: SessionStore,
        cwd: PathBuf,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Provider for CheckpointProvider {
        async fn run(
            &self,
            request: ProviderRequest,
            _cancel: CancellationToken,
            out: mpsc::Sender<EngineSignal>,
        ) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let loaded = self.store.load(request.session_id).await.unwrap();
            let leaf = loaded
                .lanes
                .iter()
                .find(|lane| lane.name == "main")
                .unwrap()
                .state
                .leaf
                .unwrap();
            let (object, _, _) = self
                .store
                .latest_checkpoint(request.session_id, leaf)
                .await
                .unwrap()
                .expect("checkpoint is durable before provider invocation");
            assert_eq!(
                git(&self.cwd, &["show", &format!("{object}:tracked")])
                    .await
                    .unwrap(),
                "before model\n"
            );
            std::fs::write(self.cwd.join("tracked"), "model changed this\n").unwrap();
            out.send(EngineSignal::Completed {
                operation_id: request.operation_id,
                step: request.step,
            })
            .await
            .unwrap();
        }
    }

    async fn runtime_checkpoint_case(fail_persistence: bool) {
        let dir = repository().await;
        std::fs::write(dir.path().join("tracked"), "before model\n").unwrap();
        let store = SessionStore::open_in_memory().unwrap();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = CheckpointProvider {
            store: store.clone(),
            cwd: dir.path().to_path_buf(),
            calls: calls.clone(),
        };
        let mut composition = Composition::new(provider, ToolRegistry::default(), store.clone());
        composition.cwd = Some(dir.path().to_str().unwrap().to_owned());
        composition.checkpoint_enabled = true;
        let gate = EffectGate::new(EffectBoundary::ModelExecution);
        composition.effect_gate = Some(Arc::new(gate.clone()));
        let runtime = composition.spawn(SessionId::generate(), None);
        let session = runtime.session();
        let (_, mut events) = session.subscribe().await.unwrap();
        let submit_session = session.clone();
        let submit =
            tokio::spawn(async move { submit_session.submit_if_idle("change a file").await });
        tokio::time::timeout(std::time::Duration::from_secs(5), gate.wait_until_reached())
            .await
            .unwrap();
        if fail_persistence {
            store.fail_next_write();
        }
        gate.release();
        submit.await.unwrap().unwrap();
        let terminal = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let event = events.recv().await.unwrap();
                if matches!(
                    event,
                    RuntimeEvent::OperationFinished { .. }
                        | RuntimeEvent::OperationFailed { .. }
                        | RuntimeEvent::OperationCancelled { .. }
                ) {
                    break event;
                }
            }
        })
        .await
        .unwrap();
        if fail_persistence {
            assert!(matches!(terminal, RuntimeEvent::OperationFailed { .. }));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(
                std::fs::read_to_string(dir.path().join("tracked")).unwrap(),
                "before model\n"
            );
        } else {
            assert!(matches!(terminal, RuntimeEvent::OperationFinished { .. }));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        session.close().await.unwrap();
        runtime.join().await.unwrap();
    }

    #[tokio::test]
    async fn runtime_publishes_checkpoint_before_provider_can_edit() {
        runtime_checkpoint_case(false).await;
    }

    #[tokio::test]
    async fn checkpoint_persistence_failure_prevents_provider_execution() {
        runtime_checkpoint_case(true).await;
    }
    #[tokio::test]
    async fn noninteractive_composition_does_not_capture_checkpoints() {
        let dir = repository().await;
        std::fs::write(dir.path().join("tracked"), "changed\n").unwrap();
        let provider =
            crate::provider::ScriptedProvider::new(vec![crate::provider::ScriptedMessage::text(
                "done",
            )]);
        let runtime = Runtime::start_with_policy_and_resources_in_cwd(
            provider,
            ToolRegistry::default(),
            SessionStore::open_in_memory().unwrap(),
            Arc::new(crate::policy::DefaultPolicy),
            Vec::new(),
            dir.path().to_str().unwrap(),
        );
        let session = runtime.session();
        let (_, mut events) = session.subscribe().await.unwrap();
        session.submit_if_idle("go").await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if matches!(
                    events.recv().await.unwrap(),
                    RuntimeEvent::OperationFinished { .. }
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(
            git(dir.path(), &["for-each-ref", "refs/ion/checkpoints/"])
                .await
                .unwrap()
                .is_empty()
        );
        session.close().await.unwrap();
        runtime.join().await.unwrap();
    }
}
