//! Reconcile tests.

use super::support::*;

use crate::tool::{
    FileSnapshot, ReconcileVerdict, classify_reconciliation, classify_reconciliation_snapshot,
    reconciliation_evidence,
};
use std::path::PathBuf;

fn sha_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex_encode(Sha256::digest(bytes).as_slice())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn write_evidence_records_preimage_and_postimage() {
    let dir = tempfile::tempdir().expect("tempdir");
    let existing = dir.path().join("a.txt");
    std::fs::write(&existing, "old").expect("seed");
    let evidence = reconciliation_evidence(
        dir.path(),
        "write",
        &json!({ "path": "a.txt", "contents": "new" }),
    )
    .await
    .expect("evidence");
    assert_eq!(evidence["preimage"]["exists"], true);
    assert_eq!(evidence["preimage"]["hash"], sha_hex(b"old"));
    assert!(evidence["preimage"]["identity"].is_string());
    assert_eq!(evidence["postimage_hash"], sha_hex(b"new"));
    assert!(evidence["path"].as_str().unwrap().ends_with("a.txt"));

    // Absent preimage is recorded as absent, not as a fake hash.
    let evidence = reconciliation_evidence(
        dir.path(),
        "write",
        &json!({ "path": "missing.txt", "contents": "x" }),
    )
    .await
    .expect("evidence");
    assert_eq!(evidence["preimage"]["exists"], false);
    assert!(evidence["preimage"].get("hash").is_none());
}

#[tokio::test]
async fn edit_evidence_hashes_the_patched_result() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("b.txt");
    std::fs::write(&file, "hello world").expect("seed");
    let evidence = reconciliation_evidence(
        dir.path(),
        "edit",
        &json!({ "path": "b.txt", "old_str": "world", "new_str": "ion" }),
    )
    .await
    .expect("evidence");
    assert_eq!(evidence["postimage_hash"], sha_hex(b"hello ion"));
    // A missing old_str cannot be classified: evidence fails.
    assert!(
        reconciliation_evidence(
            dir.path(),
            "edit",
            &json!({ "path": "b.txt", "old_str": "nope", "new_str": "x" }),
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn write_rejects_same_content_file_replacement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "old").expect("seed");
    let arguments = json!({ "path": "a.txt", "contents": "new" });
    let evidence = reconciliation_evidence(dir.path(), "write", &arguments)
        .await
        .expect("evidence");

    let replacement = dir.path().join("replacement.txt");
    std::fs::write(&replacement, "old").expect("replacement");
    std::fs::rename(&replacement, &file).expect("replace file");

    let registry = ToolRegistry::with_cwd(dir.path());
    let outcome = registry
        .execute_with_reconciliation(
            "write",
            &arguments,
            Some(&evidence),
            None,
            tokio_util::sync::CancellationToken::new(),
            None,
        )
        .await;
    assert!(
        outcome.is_error,
        "replacement must not be overwritten: {outcome:?}"
    );
    assert!(outcome.output.contains("precondition"), "{outcome:?}");
    assert_eq!(std::fs::read_to_string(&file).expect("file"), "old");
}

#[test]
fn classification_covers_all_verdicts() {
    let preimage_hash = sha_hex(b"preimage");
    let postimage_hash = sha_hex(b"postimage");
    let evidence = json!({
        "path": "/tmp/x",
        "preimage": { "exists": true, "hash": preimage_hash },
        "postimage_hash": postimage_hash,
    });
    let digest = |bytes: &[u8]| {
        use sha2::{Digest, Sha256};
        Some(Sha256::digest(bytes).into())
    };
    assert_eq!(
        classify_reconciliation(&evidence, digest(b"postimage")),
        ReconcileVerdict::AlreadyApplied
    );
    assert_eq!(
        classify_reconciliation(&evidence, digest(b"preimage")),
        ReconcileVerdict::SafeToExecute
    );
    assert_eq!(
        classify_reconciliation(&evidence, digest(b"conflict")),
        ReconcileVerdict::Conflict
    );
    assert_eq!(
        classify_reconciliation(&evidence, None),
        ReconcileVerdict::Conflict
    );
    // Absent preimage must match an absent file.
    let created = json!({
        "path": "/tmp/x",
        "preimage": { "exists": false },
        "postimage_hash": postimage_hash,
    });
    assert_eq!(
        classify_reconciliation(&created, None),
        ReconcileVerdict::SafeToExecute
    );
    assert_eq!(
        classify_reconciliation(&json!(null), Some([0u8; 32])),
        ReconcileVerdict::Unknown
    );

    let identity_evidence = json!({
        "preimage": {
            "exists": true,
            "hash": sha_hex(b"preimage"),
            "identity": "1:1",
        },
        "postimage_hash": sha_hex(b"postimage"),
    });
    let replacement = FileSnapshot {
        hash: digest(b"preimage").expect("digest"),
        identity: Some("2:2".to_owned()),
    };
    assert_eq!(
        classify_reconciliation_snapshot(&identity_evidence, Some(&replacement)),
        ReconcileVerdict::Conflict,
        "same-content replacement must not be replayed"
    );
    let original = FileSnapshot {
        identity: Some("1:1".to_owned()),
        ..replacement
    };
    assert_eq!(
        classify_reconciliation_snapshot(&identity_evidence, Some(&original)),
        ReconcileVerdict::SafeToExecute
    );
}

/// Build a session whose operation is pending on one Reconcile
/// write effect, then reopen and assert the recovery verdict.
async fn pending_write_session(store: &SessionStore, cwd: &std::path::Path) -> crate::SessionId {
    let session_id = crate::SessionId::generate();
    store
        .create_session(SessionRecord {
            id: session_id,
            cwd: cwd.to_string_lossy().into_owned(),
            title: "reconcile".to_owned(),
            initial_model_ref: "test-model".to_owned(),
            parent_session_id: None,
        })
        .await
        .expect("create session");
    let operation_id = OperationId::generate();
    let (mut machine, _) = OperationMachine::accept(operation_id, "go", Vec::new());
    let root_inbox = InboxRecord {
        id: InboxId::generate(),
        kind: InboxKind::Prompt,
        text: "go".to_owned(),
        status: crate::InboxStatus::Applied,
    };
    let entry = EntryRecord::provision(
        1,
        SessionEntry::UserMessage {
            text: "go".to_owned(),
        },
    );
    let checkpoint = CheckpointRecord {
        state_seq: 1,
        payload: CheckpointPayload {
            state: machine.state().clone(),
            cancel_requested: false,
            prompt: "go".to_owned(),
            capability_snapshot_id: CapabilitySnapshot::new(Vec::new()).id.clone(),
            open_effect: None,
        },
        capability_snapshot: CapabilitySnapshot::new(Vec::new()),
    };
    store
        .begin_operation(session_id, operation_id, root_inbox, checkpoint, entry)
        .await
        .expect("begin");

    // The intended write: a.txt from "old" to "new".
    let target = cwd.join("a.txt");
    let evidence =
        reconciliation_evidence(cwd, "write", &json!({ "path": "a.txt", "contents": "new" }))
            .await
            .expect("evidence");
    machine
        .apply(Transition::StartModelStep {
            model: step_model(),
            plan: ContextPlan {
                system: String::new(),
                messages: Vec::new(),
            },
        })
        .expect("start model step");
    machine
        .apply(Transition::ProviderCompleted {
            text: String::new(),
            tool_calls: vec![ToolCall {
                operation_id,
                call_id: 1,
                name: "write".to_owned(),
                arguments: json!({ "path": "a.txt", "contents": "new" }),
            }],
        })
        .expect("plan the write");
    let applied = machine
        .apply(Transition::AdmitNextTool)
        .expect("admit the write");
    let EffectIntent::Tool { call } = applied.intents[0].clone() else {
        panic!("tool intent expected");
    };
    let effect = crate::EffectRecord {
        id: EffectId::generate(),
        kind: "tool:write".to_owned(),
        recovery_class: RecoveryClass::Reconcile,
        effective_input: json!({
            "tool": "write",
            "arguments": call.arguments,
            "call_id": call.call_id,
            "canonical": { "Path": { "path": target } },
            "reconciliation": evidence,
        }),
        attempt: 1,
    };
    let checkpoint = CheckpointRecord {
        state_seq: 2,
        payload: CheckpointPayload {
            state: machine.state().clone(),
            cancel_requested: false,
            prompt: "go".to_owned(),
            capability_snapshot_id: CapabilitySnapshot::new(Vec::new()).id.clone(),
            open_effect: Some(effect.clone()),
        },
        capability_snapshot: CapabilitySnapshot::new(Vec::new()),
    };
    store
        .commit(CommitRequest {
            session_id,
            operation_id,
            checkpoint,
            entries: Vec::new(),
            open_effects: vec![effect],
            settled_effects: Vec::new(),
            indeterminate_effects: Vec::new(),
            inbox: Vec::new(),
            inbox_applied: Vec::new(),
            usage: Vec::new(),
            context_manifests: Vec::new(),
            assistant_frames_delete: Vec::new(),
            tool_progress_delete: Vec::new(),
        })
        .await
        .expect("commit pending write");
    session_id
}

/// Recovery events are live-only and predate any subscriber (P6, §21.2);
/// reopen tests observe recovery through the snapshot and the store.
async fn wait_until_idle(session: &SessionHandle) {
    for _ in 0..100 {
        let snapshot = session.snapshot().await.expect("snapshot");
        if snapshot.operation == OperationStatus::Idle {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("operation never went idle after reopen");
}

#[tokio::test]
async fn preimage_intact_reexecutes_the_write_exactly_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "old").expect("seed preimage");
    let store = SessionStore::open_in_memory().expect("store");
    let session_id = pending_write_session(&store, dir.path()).await;

    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::with_cwd(dir.path()),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    wait_until_idle(&session).await;
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).expect("file"),
        "new",
        "recovery must execute the intended write"
    );
    let loaded = store.load(session_id).await.expect("load");
    let recovered = loaded.entries.iter().any(|(_, entry)| {
        matches!(entry, SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. },
        } if output == "written")
    });
    assert!(recovered, "{loaded:?}");
}

#[tokio::test]
async fn postimage_present_settles_without_repeating() {
    let dir = tempfile::tempdir().expect("tempdir");
    // The write already happened before the crash.
    std::fs::write(dir.path().join("a.txt"), "new").expect("seed postimage");
    let store = SessionStore::open_in_memory().expect("store");
    let session_id = pending_write_session(&store, dir.path()).await;

    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::with_cwd(dir.path()),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    wait_until_idle(&session).await;
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    let loaded = store.load(session_id).await.expect("load");
    let settled = loaded.entries.iter().any(|(_, entry)| {
        matches!(entry, SessionEntry::ToolResult {
            result: ToolResult::Ok { output, .. },
        } if output.contains("already applied"))
    });
    assert!(
        settled,
        "postimage must settle without repeating: {loaded:?}"
    );
}

#[tokio::test]
async fn same_content_replacement_settles_indeterminate_without_overwrite() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("a.txt"), "old").expect("seed preimage");
    let store = SessionStore::open_in_memory().expect("store");
    let session_id = pending_write_session(&store, dir.path()).await;

    let replacement = dir.path().join("replacement.txt");
    std::fs::write(&replacement, "old").expect("replacement");
    std::fs::rename(&replacement, dir.path().join("a.txt")).expect("replace file");

    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::with_cwd(dir.path()),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    wait_until_idle(&session).await;
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).expect("file"),
        "old",
        "recovery must not overwrite a same-content replacement"
    );
    let loaded = store.load(session_id).await.expect("load");
    assert!(matches!(
        loaded.operations[0].latest.1.state,
        OperationState::Finished(OperationOutcome::Indeterminate)
    ));
}

#[tokio::test]
async fn conflicting_file_state_settles_indeterminate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::open_in_memory().expect("store");
    // Evidence records the absent preimage FIRST; only then does
    // someone else create the file with unrelated contents.
    let session_id = pending_write_session(&store, dir.path()).await;
    std::fs::write(dir.path().join("a.txt"), "something else").expect("seed conflict");

    let runtime = Runtime::open_session(
        ScriptedProvider::echo(),
        ToolRegistry::with_cwd(dir.path()),
        store.clone(),
        session_id,
    )
    .await
    .expect("reopen");
    let session = runtime.session();
    wait_until_idle(&session).await;
    session.close().await.expect("close");
    runtime.join().await.expect("join");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("a.txt")).expect("file"),
        "something else",
        "a conflicting file must never be overwritten"
    );
    let loaded = store.load(session_id).await.expect("load");
    assert!(matches!(
        loaded.operations[0].latest.1.state,
        OperationState::Finished(OperationOutcome::Indeterminate)
    ));
}

#[allow(dead_code)]
fn path_type_check(_: PathBuf) {}
