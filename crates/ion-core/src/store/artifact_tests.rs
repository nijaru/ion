//! Durable pre-state and queue-boundary regressions; no timing-dependent DB races.
use super::*;
use crate::*;
use rusqlite::params;
use std::time::Duration;

struct Fixture {
    store: SessionStore,
    root: PathBuf,
    step: StepId,
    attempt: AttemptId,
}

impl Fixture {
    async fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ion-artifact-queue-{}", SessionId::new()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("session.sqlite");
        let (store, metadata, _) = SessionStore::create(
            &path,
            SessionId::new(),
            crate::config::tests::config(),
            ObservationHub::new(),
            BlobStoreLimits::default(),
        )
        .await
        .unwrap();
        let input = store
            .admit_input(
                metadata.primary_conversation,
                AdmitInputRequest {
                    sender: InputSender::User,
                    mode: InputMode::Submit,
                    request_key: None,
                    body: InputBody::Text("read".into()),
                },
            )
            .await
            .unwrap();
        let started = store
            .start_turn(StartTurnRequest {
                conversation: metadata.primary_conversation,
                input: input.input().id,
                admitted_at_unix_ms: 0,
                wall_deadline_unix_ms: None,
            })
            .await
            .unwrap();
        let basis = store.drive_basis(started.turn.id).await.unwrap();
        let cutoff = basis.entries.last().map(|entry| entry.id);
        let request = assemble(
            &basis.turn.environment,
            &basis.turn.settings,
            &basis.entries,
            cutoff,
        )
        .unwrap();
        let manifest = RequestManifest {
            environment_digest: basis.turn.environment.digest().unwrap(),
            settings: basis.turn.settings,
            context_boundary: None,
            cutoff,
            included_inputs: basis.included_inputs,
            assembly: semantic_request_assembly_revision(),
            semantic_digest: request.semantic_digest,
            provider_fingerprint: ProviderFingerprint {
                encoding: basis.turn.environment.providers[0].request_encoding.clone(),
                digest: ContentDigest::of_bytes(b"test"),
            },
        };
        let step = store
            .create_initial_model_step(started.turn.id, manifest)
            .await
            .unwrap()
            .step
            .id;
        // A durable attempt immediately after intent. Explicit IDs are above the
        // small fixture's allocated range; advance the allocator with that pre-state.
        let entry = EntryId::new(1000).unwrap();
        let invocation = InvocationId::new(1001).unwrap();
        let attempt = AttemptId::new(1002).unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute("INSERT INTO entries (id,conversation_id,commit_seq,kind,data,projection) VALUES (?1,?2,1003,'assistant',?3,'[]')", params![entry.get(), metadata.primary_conversation.get(), serde_json::to_string(&EntryData::Assistant { step }).unwrap()]).unwrap();
        let action = PreparedAction::new(
            basis.turn.environment.tools[0].id.clone(),
            serde_json::json!({}),
            EgressRealm::Local,
            ToolAuthority::ReadOnly,
            None,
            vec![],
        )
        .unwrap();
        connection.execute("INSERT INTO tool_invocations (id,step_id,assistant_entry,source_index,binding_id,prepared_action,result_limit_bytes,approval,exchange_state) VALUES (?1,?2,?3,0,'read',?4,1024,?5,?6)", params![invocation.get(),step.get(),entry.get(),serde_json::to_string(&ToolPreparation::Ready(action)).unwrap(),serde_json::to_string(&ApprovalState::NotRequired).unwrap(),serde_json::to_string(&ToolExchangeState::Pending).unwrap()]).unwrap();
        connection.execute("INSERT INTO tool_attempts (id,invocation_id,ordinal,generation,executor,state) VALUES (?1,?2,1,0,?3,?4)", params![attempt.get(),invocation.get(),serde_json::to_string(&SemanticCompatibilityId::new("local").unwrap()).unwrap(),serde_json::to_string(&ToolAttemptState::IntentCommitted { start_receipt: None }).unwrap()]).unwrap();
        connection
            .execute("UPDATE session_meta SET last_seq=1003,last_commit=1003", [])
            .unwrap();
        Self {
            store,
            root,
            step,
            attempt,
        }
    }

    async fn publication(&self, attempt: AttemptId) -> (BlobRef, PublishedBlob) {
        let scope = self.store.publication_scope(attempt).await.unwrap();
        let reference = scope
            .publisher()
            .publish(&b"complete output"[..])
            .await
            .unwrap();
        (reference, scope.finish().await.unwrap())
    }

    fn evidence(&self, reference: BlobRef, publication: Option<PublishedBlob>) -> ToolMutation {
        ToolMutation::Evidence {
            step: self.step,
            attempt: self.attempt,
            publication,
            state: Box::new(ToolAttemptState::Settled {
                result: ToolResult {
                    value: serde_json::json!("preview"),
                    is_error: false,
                    capture: OutputCapture::CompleteArtifact {
                        full_output: reference,
                    },
                },
                effect: EffectSummary::NoMutation,
                receipt: None,
                retryable: false,
            }),
        }
    }

    async fn close(self) {
        self.store.shutdown().await.unwrap();
        std::fs::remove_dir_all(self.root).unwrap();
    }
}

#[tokio::test]
async fn artifact_db_rejects_forged_wrong_session_wrong_attempt_and_modified_refs() {
    let fixture = Fixture::new().await;
    let other = Fixture::new().await;
    let (reference, proof) = fixture.publication(fixture.attempt).await;
    drop(proof);
    let (cross, cross_proof) = other.publication(fixture.attempt).await;
    let (stale, stale_proof) = fixture.publication(AttemptId::new(1004).unwrap()).await;
    let (mut modified, modified_proof) = fixture.publication(fixture.attempt).await;
    modified.length += 1;
    for operation in [
        fixture.evidence(reference, None),
        fixture.evidence(cross, Some(cross_proof)),
        fixture.evidence(stale, Some(stale_proof)),
        fixture.evidence(modified, Some(modified_proof)),
    ] {
        assert!(matches!(
            fixture.store.tool_mutate(operation).await,
            Err(StoreError::InvalidState(_))
        ));
        assert!(matches!(
            fixture
                .store
                .tool_records(fixture.step)
                .await
                .unwrap()
                .attempts[0]
                .state,
            ToolAttemptState::IntentCommitted { .. }
        ));
        assert!(matches!(
            fixture.store.read_artifact(fixture.attempt, 0, 10).await,
            Err(StoreError::NotFound { .. })
        ));
    }
    assert_eq!(
        fixture
            .store
            .collect_artifacts()
            .await
            .unwrap()
            .object_count,
        1
    );
    fixture.close().await;
    other.close().await;
}

#[tokio::test]
async fn artifact_gc_cannot_overtake_queued_evidence_even_without_a_waiter() {
    let fixture = Fixture::new().await;
    let (reference, proof) = fixture.publication(fixture.attempt).await;
    let (reached, paused) = oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    fixture
        .store
        .tx
        .send(Command::Pause {
            reached,
            release: released,
        })
        .await
        .unwrap();
    paused.await.unwrap();
    let (reply, waiter) = oneshot::channel();
    fixture
        .store
        .tx
        .send(Command::ToolMutate {
            operation: fixture.evidence(reference, Some(proof)),
            reply,
        })
        .await
        .unwrap();
    drop(waiter);
    let mut gc = Box::pin(fixture.store.collect_artifacts());
    assert!(
        tokio::time::timeout(Duration::from_millis(30), &mut gc)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), gc)
            .await
            .unwrap()
            .unwrap()
            .object_count,
        0
    );
    assert!(
        matches!(fixture.store.read_artifact(fixture.attempt, 0, 64).await.unwrap(), ArtifactRead::Page { bytes, .. } if bytes == b"complete output")
    );
    fixture.close().await;
}

#[tokio::test]
async fn artifact_dropped_publication_waiter_keeps_scope_until_worker_finishes() {
    struct BlockedSource {
        reached: Option<oneshot::Sender<()>>,
        release: std::sync::mpsc::Receiver<()>,
    }
    impl std::io::Read for BlockedSource {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            self.reached.take().unwrap().send(()).unwrap();
            self.release.recv().unwrap();
            Ok(0)
        }
    }
    for abandon_scope in [false, true] {
        let fixture = Fixture::new().await;
        let scope = fixture
            .store
            .publication_scope(fixture.attempt)
            .await
            .unwrap();
        let publisher = scope.publisher();
        let stale = publisher.clone();
        let (reached, working) = oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let task = tokio::spawn(async move {
            publisher
                .publish(BlockedSource {
                    reached: Some(reached),
                    release: released,
                })
                .await
        });
        working.await.unwrap();
        assert!(matches!(
            stale.publish(&b"concurrent"[..]).await,
            Err(ArtifactError::PublisherBusy)
        ));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let alias = fixture.root.join("alias.sqlite");
            symlink(fixture.root.join("session.sqlite"), &alias).unwrap();
            assert!(matches!(
                Session::open(&alias).await,
                Err(SessionError::Storage(_))
            ));
        }
        if abandon_scope {
            drop(scope);
            let mut close = Box::pin(fixture.store.shutdown());
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut close)
                    .await
                    .is_err()
            );
            release.send(()).unwrap();
            close.await.unwrap();
            assert!(matches!(
                stale.publish(&b"late"[..]).await,
                Err(ArtifactError::PublisherClosed)
            ));
            // Close has drained the worker and released the Session lock, not just
            // marked the owner closed while a detached job can still touch its files.
            let reopened = Session::open(fixture.root.join("session.sqlite"))
                .await
                .unwrap();
            assert_eq!(
                reopened
                    .handle()
                    .collect_artifacts()
                    .await
                    .unwrap()
                    .object_count,
                1
            );
            reopened.close().await.unwrap();
            std::fs::remove_dir_all(fixture.root).unwrap();
        } else {
            let mut finish = Box::pin(scope.finish());
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut finish)
                    .await
                    .is_err()
            );
            release.send(()).unwrap();
            let proof = finish.await.unwrap();
            assert!(matches!(
                stale.publish(&b"late"[..]).await,
                Err(ArtifactError::PublisherClosed)
            ));
            drop(proof);
            assert_eq!(
                fixture
                    .store
                    .collect_artifacts()
                    .await
                    .unwrap()
                    .object_count,
                1
            );
            fixture.close().await;
        }
    }
}
