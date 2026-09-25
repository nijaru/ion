use std::path::PathBuf;

use ion_ai::{GenerationControls, ModelRef, Reasoning, ToolChoice};
use ion_core::{
    AbandonResult, Admission, AuthorityCeiling, ContextPolicy, ControlCeiling, ConversationConfig,
    EgressRealm, InputBody, InputMode, InputSender, ProviderBinding, ProviderBindingId,
    ProviderCapabilities, RequestKey, ReturnedModelPolicy, SemanticCompatibilityId, Session,
    SessionError, SessionHealth, SessionId, SnapshotRequest, StartTurnRequest, TurnLimits,
    WatchQueueLimits, WatchRequest, WorkspaceBinding,
};

fn database(name: &str) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "ion-r1b-{name}-{}-{}",
        std::process::id(),
        SessionId::new()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let database = dir.join("session.sqlite");
    (dir, database)
}

fn config(instructions: &str) -> ConversationConfig {
    let provider_id = ProviderBindingId::new("scripted").expect("provider id");
    ConversationConfig {
        instructions: instructions.to_owned(),
        project_context: Vec::new(),
        providers: vec![ProviderBinding {
            id: provider_id.clone(),
            model: ModelRef {
                provider: "scripted".to_owned(),
                model: "test".to_owned(),
            },
            adapter: SemanticCompatibilityId::new("scripted-adapter-v1").expect("adapter"),
            request_encoding: SemanticCompatibilityId::new("scripted-request-v1")
                .expect("encoding"),
            replay_family: None,
            capabilities: ProviderCapabilities {
                max_input_tokens: 100_000,
                max_output_tokens: 8_192,
                tools: false,
                parallel_tool_calls: false,
                structured_output: true,
                replay: false,
                reasoning: true,
            },
            returned_model: ReturnedModelPolicy::Exact,
            start_receipts: ion_core::StartReceiptCapability::None,
            egress: EgressRealm::Local,
        }],
        default_provider: provider_id,
        fallback_route: Vec::new(),
        compaction_route: Vec::new(),
        tools: Vec::new(),
        initial_tools: Vec::new(),
        controls: GenerationControls {
            max_output_tokens: 4096,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::None,
            parallel_tool_calls: false,
        },
        control_ceiling: ControlCeiling {
            max_output_tokens: 8192,
            sampling: false,
            parallel_tool_calls: false,
            allowed_reasoning: vec![
                Reasoning::ProviderDefault,
                Reasoning::Off,
                Reasoning::Low,
                Reasoning::Medium,
                Reasoning::High,
            ],
        },
        context: ContextPolicy {
            max_request_bytes: 4 * 1024 * 1024,
            max_input_tokens: 100_000,
            max_checkpoint_bytes: 128 * 1024,
            max_tail_bytes: 1024 * 1024,
        },
        workspace: WorkspaceBinding {
            id: "workspace".to_owned(),
            canonical_root: "/tmp/project".to_owned(),
            backend: "local".to_owned(),
            object_identity: "dev:ino".to_owned(),
        },
        authority: AuthorityCeiling {
            workspace_mutation: false,
            unconfined_execution: false,
            remote_tools: false,
            egress_realms: vec![EgressRealm::Local],
        },
        limits: TurnLimits {
            max_model_steps: 32,
            max_model_attempts_per_step: 4,
            max_tool_invocations: 128,
            max_parallel_read_tools: 8,
            max_response_bytes: 1024 * 1024,
            max_tool_preview_bytes: 64 * 1024,
            max_cost_microusd: None,
        },
    }
}

fn input(key: &str, text: &str) -> ion_core::AdmitInputRequest {
    ion_core::AdmitInputRequest {
        sender: InputSender::User,
        mode: InputMode::Submit,
        request_key: Some(RequestKey::new(key).expect("request key")),
        body: InputBody::Text(text.to_owned()),
    }
}

fn snapshot_request(conversation: ion_core::ConversationId) -> SnapshotRequest {
    SnapshotRequest {
        conversation,
        max_inputs: 64,
        max_entries: 64,
        max_bytes: 1024 * 1024,
    }
}

fn start_request(
    conversation: ion_core::ConversationId,
    input: ion_core::InputId,
    admitted_at_unix_ms: i64,
) -> StartTurnRequest {
    StartTurnRequest {
        conversation,
        input,
        admitted_at_unix_ms,
        wall_deadline_unix_ms: None,
    }
}

async fn cleanup(session: Session, dir: PathBuf) {
    session.close().await.expect("close");
    std::fs::remove_dir_all(dir).expect("cleanup");
}

#[tokio::test]
async fn unimplemented_control_modes_are_rejected_without_a_durable_queue_entry() {
    let (dir, path) = database("unsupported-controls");
    let session = Session::create(&path, config("v1")).await.unwrap().session;
    let handle = session.handle();
    let conversation = session.primary_conversation();
    let coverage = handle
        .snapshot(snapshot_request(conversation))
        .await
        .unwrap()
        .coverage;
    for mode in [InputMode::Steer, InputMode::InteractionReply] {
        let mut request = input("unsupported", "do not accept");
        request.mode = mode;
        assert!(matches!(
            handle.admit_input(conversation, request).await,
            Err(SessionError::InvalidState(_))
        ));
    }
    let mut wrong_body = input("unsupported", "do not accept");
    wrong_body.body = InputBody::InteractionReply {
        invocation: ion_core::InvocationId::new(1).unwrap(),
        answer: serde_json::json!("yes"),
    };
    assert!(matches!(
        handle.admit_input(conversation, wrong_body).await,
        Err(SessionError::InvalidState(_))
    ));
    let snapshot = handle
        .snapshot(snapshot_request(conversation))
        .await
        .unwrap();
    assert_eq!(snapshot.coverage, coverage);
    assert!(snapshot.queued_inputs.is_empty());
    cleanup(session, dir).await;
}

#[tokio::test]
async fn same_request_key_and_payload_replays_original_admission() {
    let (dir, path) = database("replay");
    let created = Session::create(&path, config("v1")).await.expect("create");
    let session = created.session;
    let handle = session.handle();
    let conversation = session.primary_conversation();

    let first = handle
        .admit_input(conversation, input("same", "hello"))
        .await
        .expect("first admission");
    let (input_id, admitted_at, commit) = match first {
        Admission::Created {
            input,
            admitted_at,
            receipt,
        } => (input.id, admitted_at, receipt.seq),
        Admission::Replayed { .. } => panic!("first admission cannot be replayed"),
    };

    let replay = handle
        .admit_input(conversation, input("same", "hello"))
        .await
        .expect("replay");
    match replay {
        Admission::Replayed {
            input,
            admitted_at: replay_commit,
        } => {
            assert_eq!(input.id, input_id);
            assert_eq!(replay_commit, admitted_at);
        }
        Admission::Created { .. } => panic!("duplicate admission created new durable state"),
    }

    let snapshot = handle
        .snapshot(snapshot_request(conversation))
        .await
        .expect("snapshot");
    assert_eq!(snapshot.coverage, commit);
    assert_eq!(snapshot.queued_inputs.len(), 1);
    cleanup(session, dir).await;
}

#[tokio::test]
async fn conflicting_request_key_reuse_rejects_without_mutation() {
    let (dir, path) = database("conflict");
    let created = Session::create(&path, config("v1")).await.expect("create");
    let session = created.session;
    let handle = session.handle();
    let conversation = session.primary_conversation();

    let first = handle
        .admit_input(conversation, input("same", "one"))
        .await
        .expect("first");
    let coverage = match first {
        Admission::Created { receipt, .. } => receipt.seq,
        Admission::Replayed { .. } => panic!("first admission"),
    };

    let error = handle
        .admit_input(conversation, input("same", "two"))
        .await
        .expect_err("conflicting reuse must fail");
    assert!(matches!(error, SessionError::RequestKeyConflict { .. }));
    assert_eq!(handle.health(), SessionHealth::Open);

    let snapshot = handle
        .snapshot(snapshot_request(conversation))
        .await
        .expect("snapshot");
    assert_eq!(snapshot.coverage, coverage);
    assert_eq!(snapshot.queued_inputs.len(), 1);
    cleanup(session, dir).await;
}

#[tokio::test]
async fn request_keys_are_conversation_scoped() {
    let (dir, path) = database("scope");
    let created = Session::create(&path, config("v1")).await.expect("create");
    let session = created.session;
    let handle = session.handle();
    let first_conversation = session.primary_conversation();
    let second = handle
        .create_conversation(config("v1"))
        .await
        .expect("second conversation");

    let first = handle
        .admit_input(first_conversation, input("shared", "one"))
        .await
        .expect("first");
    let second_admission = handle
        .admit_input(second.conversation.id, input("shared", "two"))
        .await
        .expect("second");

    assert_ne!(first.input().id, second_admission.input().id);
    assert_eq!(
        first.input().request_key,
        second_admission.input().request_key
    );
    cleanup(session, dir).await;
}

#[tokio::test]
async fn one_unfinished_turn_per_conversation() {
    let (dir, path) = database("one-turn");
    let created = Session::create(&path, config("v1")).await.expect("create");
    let session = created.session;
    let handle = session.handle();
    let conversation = session.primary_conversation();

    let first = handle
        .admit_input(conversation, input("one", "one"))
        .await
        .expect("first input");
    let second = handle
        .admit_input(conversation, input("two", "two"))
        .await
        .expect("second input");
    let first_turn = handle
        .start_turn(start_request(conversation, first.input().id, 10))
        .await
        .expect("first turn");
    let coverage = first_turn.receipt.seq;

    let error = handle
        .start_turn(start_request(conversation, second.input().id, 11))
        .await
        .expect_err("second unfinished turn must fail");
    assert!(matches!(error, SessionError::ConversationBusy(id) if id == conversation));
    assert_eq!(
        handle
            .snapshot(snapshot_request(conversation))
            .await
            .expect("snapshot")
            .coverage,
        coverage
    );

    handle
        .abandon_turn(first_turn.turn.id)
        .await
        .expect("abandon first turn");
    handle
        .start_turn(start_request(conversation, second.input().id, 11))
        .await
        .expect("second turn after terminalization");
    cleanup(session, dir).await;
}

#[tokio::test]
async fn active_turn_freezes_environment_and_later_turn_uses_new_config() {
    let (dir, path) = database("frozen-config");
    let created = Session::create(&path, config("old instructions"))
        .await
        .expect("create");
    let session = created.session;
    let handle = session.handle();
    let conversation = session.primary_conversation();

    let first_input = handle
        .admit_input(conversation, input("one", "first"))
        .await
        .expect("input");
    let first_turn = handle
        .start_turn(start_request(conversation, first_input.input().id, 10))
        .await
        .expect("start");
    assert_eq!(first_turn.turn.environment.instructions, "old instructions");

    let current = handle
        .current_config(conversation)
        .await
        .expect("current config");
    let configured = handle
        .configure(conversation, current.revision, config("new instructions"))
        .await
        .expect("configure");
    assert_eq!(configured.config.config.instructions, "new instructions");

    let snapshot = handle
        .snapshot(snapshot_request(conversation))
        .await
        .expect("snapshot");
    assert_eq!(
        snapshot
            .unfinished_turn
            .as_ref()
            .expect("active turn")
            .environment
            .instructions,
        "old instructions"
    );

    assert!(matches!(
        handle
            .abandon_turn(first_turn.turn.id)
            .await
            .expect("abandon"),
        AbandonResult::Committed { .. }
    ));
    let second_input = handle
        .admit_input(conversation, input("two", "second"))
        .await
        .expect("second input");
    let second_turn = handle
        .start_turn(start_request(conversation, second_input.input().id, 20))
        .await
        .expect("second turn");
    assert_eq!(
        second_turn.turn.environment.instructions,
        "new instructions"
    );
    assert_eq!(
        second_turn.turn.environment.config_revision,
        configured.config.revision
    );
    cleanup(session, dir).await;
}

#[tokio::test]
async fn passive_reopen_performs_no_semantic_recovery_write() {
    let (dir, path) = database("passive-open");
    let created = Session::create(&path, config("v1")).await.expect("create");
    let session = created.session;
    let handle = session.handle();
    let conversation = session.primary_conversation();
    let admitted = handle
        .admit_input(conversation, input("one", "work"))
        .await
        .expect("input");
    let started = handle
        .start_turn(start_request(conversation, admitted.input().id, 10))
        .await
        .expect("turn");
    let before = handle
        .snapshot(snapshot_request(conversation))
        .await
        .expect("snapshot");
    session.close().await.expect("close");

    let reopened = Session::open(&path).await.expect("reopen");
    let handle = reopened.handle();
    let after = handle
        .snapshot(snapshot_request(conversation))
        .await
        .expect("snapshot after reopen");

    assert_eq!(after.coverage, before.coverage);
    let turn = after.unfinished_turn.expect("unfinished turn");
    assert_eq!(turn.id, started.turn.id);
    assert!(!turn.cancellation.requested);
    assert_eq!(turn.cancellation.generation, 0);
    cleanup(reopened, dir).await;
}

#[tokio::test]
async fn cancellation_generation_is_durable() {
    let (dir, path) = database("cancel-generation");
    let created = Session::create(&path, config("v1")).await.expect("create");
    let session = created.session;
    let handle = session.handle();
    let conversation = session.primary_conversation();
    let admitted = handle
        .admit_input(conversation, input("one", "work"))
        .await
        .expect("input");
    let started = handle
        .start_turn(start_request(conversation, admitted.input().id, 10))
        .await
        .expect("turn");

    let cancelled = handle.cancel_turn(started.turn.id).await.expect("cancel");
    let cancel_seq = match cancelled {
        ion_core::CancellationResult::Committed { turn, receipt } => {
            assert!(turn.cancellation.requested);
            assert_eq!(turn.cancellation.generation, 1);
            receipt.seq
        }
        other => panic!("unexpected cancellation result: {other:?}"),
    };
    let second = handle
        .cancel_turn(started.turn.id)
        .await
        .expect("repeat cancellation");
    assert!(matches!(
        second,
        ion_core::CancellationResult::AlreadyRequested(_)
    ));
    assert_eq!(
        handle
            .snapshot(snapshot_request(conversation))
            .await
            .expect("snapshot")
            .coverage,
        cancel_seq
    );
    session.close().await.expect("close");

    let reopened = Session::open(&path).await.expect("reopen");
    let turn = reopened
        .handle()
        .snapshot(snapshot_request(conversation))
        .await
        .expect("snapshot")
        .unfinished_turn
        .expect("unfinished turn");
    assert!(turn.cancellation.requested);
    assert_eq!(turn.cancellation.generation, 1);
    cleanup(reopened, dir).await;
}

#[tokio::test]
async fn failed_semantic_commit_publishes_nothing_and_fences_mutation() {
    let (dir, path) = database("commit-fault");
    let created = Session::create(&path, config("v1")).await.expect("create");
    let session = created.session;
    let handle = session.handle();
    let conversation = session.primary_conversation();

    let established = handle
        .snapshot_and_watch(WatchRequest {
            snapshot: snapshot_request(conversation),
            queue: WatchQueueLimits {
                max_receipts: 8,
                max_bytes: 64 * 1024,
            },
        })
        .await
        .expect("watch");
    let coverage = established.snapshot.coverage;

    let injector = rusqlite::Connection::open(&path).expect("fault injector");
    injector
        .execute_batch(
            "CREATE TRIGGER fail_input_commit
             BEFORE INSERT ON inputs
             BEGIN
               SELECT RAISE(ABORT, 'injected commit failure');
             END;",
        )
        .expect("inject trigger");

    let error = handle
        .admit_input(conversation, input("boom", "must not commit"))
        .await
        .expect_err("fault must fail");
    assert!(matches!(error, SessionError::Fenced(_)));
    assert_eq!(handle.health(), SessionHealth::Fenced);
    assert!(matches!(
        established.watch.try_recv(),
        Err(ion_core::ObservationError::Empty)
    ));

    let snapshot = handle
        .snapshot(snapshot_request(conversation))
        .await
        .expect("reads remain available while fenced");
    assert_eq!(snapshot.coverage, coverage);
    assert!(snapshot.queued_inputs.is_empty());

    let rejected = handle
        .admit_input(conversation, input("later", "blocked"))
        .await
        .expect_err("later mutation must be fenced");
    assert!(matches!(rejected, SessionError::Fenced(_)));

    drop(injector);
    cleanup(session, dir).await;
}

#[tokio::test]
async fn snapshot_watch_handoff_has_no_gap() {
    let (dir, path) = database("watch");
    let created = Session::create(&path, config("v1")).await.expect("create");
    let session = created.session;
    let handle = session.handle();
    let conversation = session.primary_conversation();

    let established = handle
        .snapshot_and_watch(WatchRequest {
            snapshot: snapshot_request(conversation),
            queue: WatchQueueLimits {
                max_receipts: 8,
                max_bytes: 64 * 1024,
            },
        })
        .await
        .expect("watch");
    let coverage = established.snapshot.coverage;

    let admitted = handle
        .admit_input(conversation, input("one", "after snapshot"))
        .await
        .expect("admit");
    let receipt = match admitted {
        Admission::Created { receipt, .. } => receipt,
        Admission::Replayed { .. } => panic!("new key cannot replay"),
    };
    assert!(receipt.seq > coverage);

    let observed = established.watch.recv().await.expect("update");
    assert_eq!(observed, receipt);
    cleanup(session, dir).await;
}

#[tokio::test]
async fn snapshot_is_bounded_and_old_history_remains_paginated() {
    let (dir, path) = database("bounded-snapshot");
    let created = Session::create(&path, config("v1")).await.expect("create");
    let session = created.session;
    let handle = session.handle();
    let conversation = session.primary_conversation();

    for index in 0..12 {
        let admitted = handle
            .admit_input(
                conversation,
                input(&format!("key-{index}"), &format!("message {index}")),
            )
            .await
            .expect("input");
        let started = handle
            .start_turn(start_request(
                conversation,
                admitted.input().id,
                i64::from(index),
            ))
            .await
            .expect("turn");
        handle.abandon_turn(started.turn.id).await.expect("abandon");
    }

    let snapshot = handle
        .snapshot(SnapshotRequest {
            conversation,
            max_inputs: 2,
            max_entries: 3,
            max_bytes: 64 * 1024,
        })
        .await
        .expect("bounded snapshot");
    assert_eq!(snapshot.transcript_tail.len(), 3);
    assert!(snapshot.has_older_entries);
    assert!(snapshot.queued_inputs.is_empty());

    let page = handle
        .page_entries(conversation, None, 5)
        .await
        .expect("page history");
    assert_eq!(page.entries.len(), 5);
    assert!(page.has_more);
    cleanup(session, dir).await;
}

#[tokio::test]
async fn current_and_historical_config_reads_are_revisioned() {
    let (dir, path) = database("config-history");
    let created = Session::create(&path, config("one")).await.expect("create");
    let session = created.session;
    let handle = session.handle();
    let conversation = session.primary_conversation();

    let first = handle
        .current_config(conversation)
        .await
        .expect("initial config");
    let second = handle
        .configure(conversation, first.revision, config("two"))
        .await
        .expect("configure");

    assert_eq!(
        handle
            .current_config(conversation)
            .await
            .expect("current")
            .config
            .instructions,
        "two"
    );
    assert_eq!(
        handle
            .config_as_of(conversation, first.revision)
            .await
            .expect("historical")
            .config
            .instructions,
        "one"
    );
    assert_eq!(
        handle
            .config_as_of(conversation, second.config.revision)
            .await
            .expect("second")
            .config
            .instructions,
        "two"
    );
    cleanup(session, dir).await;
}
