//! Native tools tests.

use super::support::*;

// ---- Tool execution (read/write/edit/search/find/bash) unit tests ----

#[tokio::test]
async fn read_write_edit_search_find_roundtrip() {
    let tmp = std::env::temp_dir().join(format!("ion-tool-test-{}-{}", std::process::id(), 1));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::create_dir_all(&tmp);
    let registry = ToolRegistry::with_cwd(&tmp);
    let cancel = tokio_util::sync::CancellationToken::new();

    let out = registry
        .execute(
            "write",
            &json!({"path":"sub/note.txt","contents":"hello world"}),
            cancel.clone(),
        )
        .await;
    assert!(!out.is_error, "write failed: {out:?}");
    assert_eq!(out.output, "written");

    let out = registry
        .execute("read", &json!({"path":"sub/note.txt"}), cancel.clone())
        .await;
    assert!(!out.is_error, "read failed: {out:?}");
    assert_eq!(out.output, "hello world");

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

    let out = registry
        .execute(
            "edit",
            &json!({"path":"sub/note.txt","old_str":"zzz","new_str":"x"}),
            cancel.clone(),
        )
        .await;
    assert!(out.is_error);
    assert!(out.output.contains("not found"));

    let out = registry
        .execute("search", &json!({"pattern":"hello"}), cancel.clone())
        .await;
    assert!(!out.is_error, "search failed: {out:?}");
    assert!(out.output.contains("note.txt"), "got: {out:?}");

    let out = registry
        .execute("find", &json!({"pattern":"*.txt"}), cancel.clone())
        .await;
    assert!(!out.is_error, "find failed: {out:?}");
    assert!(out.output.contains("note.txt"), "got: {out:?}");

    let out = registry
        .execute("read", &json!({"path":"../outside.txt"}), cancel.clone())
        .await;
    assert!(out.is_error);
    assert!(out.output.contains("escapes"), "got: {out:?}");

    let _ = std::fs::remove_dir_all(&tmp);
}

#[cfg(unix)]
#[tokio::test]
async fn native_file_tools_reject_symlink_targets_and_parents() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root tempdir");
    let outside = tempfile::tempdir().expect("outside tempdir");
    std::fs::write(outside.path().join("secret.txt"), "outside").expect("seed outside file");
    std::fs::create_dir(outside.path().join("nested")).expect("seed outside directory");
    std::fs::write(outside.path().join("nested/secret.txt"), "nested outside")
        .expect("seed nested outside file");
    symlink(
        outside.path().join("secret.txt"),
        root.path().join("link.txt"),
    )
    .expect("link file");
    symlink(outside.path().join("nested"), root.path().join("link-dir")).expect("link directory");

    let registry = ToolRegistry::with_cwd(root.path());
    let cancel = tokio_util::sync::CancellationToken::new();
    for (name, arguments) in [
        ("read", json!({"path": "link.txt"})),
        (
            "write",
            json!({"path": "link.txt", "contents": "overwritten"}),
        ),
        (
            "write",
            json!({"path": "link-dir/new.txt", "contents": "escaped"}),
        ),
        (
            "edit",
            json!({"path": "link.txt", "old_str": "outside", "new_str": "changed"}),
        ),
        ("search", json!({"path": "link-dir", "pattern": "secret"})),
        ("find", json!({"path": "link-dir", "pattern": "*.txt"})),
    ] {
        let outcome = registry.execute(name, &arguments, cancel.clone()).await;
        assert!(outcome.is_error, "{name} followed a symlink: {outcome:?}");
        assert!(
            outcome.output.contains("symlink"),
            "{name} did not explain the rejected link: {outcome:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(outside.path().join("secret.txt")).expect("outside file"),
        "outside"
    );
}

#[tokio::test]
async fn native_file_tools_reject_protected_git_paths() {
    let root = tempfile::tempdir().expect("root tempdir");
    std::fs::create_dir(root.path().join(".git")).expect("git directory");
    std::fs::write(root.path().join(".git/config"), "private").expect("git config");
    let registry = ToolRegistry::with_cwd(root.path());
    let cancel = tokio_util::sync::CancellationToken::new();

    for (name, arguments) in [
        ("read", json!({"path": ".git/config"})),
        (
            "write",
            json!({"path": ".git/config", "contents": "changed"}),
        ),
        (
            "edit",
            json!({"path": ".git/config", "old_str": "private", "new_str": "changed"}),
        ),
        ("search", json!({"path": ".git", "pattern": "private"})),
        ("find", json!({"path": ".git", "pattern": "*"})),
    ] {
        let outcome = registry.execute(name, &arguments, cancel.clone()).await;
        assert!(
            outcome.is_error,
            "{name} accessed protected path: {outcome:?}"
        );
        assert!(
            outcome.output.contains("protected"),
            "{name} did not explain the protected path: {outcome:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(root.path().join(".git/config")).expect("git config"),
        "private"
    );
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
async fn bash_progress_checkpoint_is_bounded_and_cleared() {
    let store = SessionStore::open_in_memory().expect("store");
    let runtime = start_runtime_with_store(
        ScriptedProvider::new(vec![
            ScriptedMessage::tool("bash", json!({"command": "sleep 1 && echo done"})),
            ScriptedMessage::text("finished"),
        ]),
        ToolRegistry::default(),
        store.clone(),
    );
    let session = runtime.session();
    let (_snapshot, mut events) = session.subscribe().await.expect("subscribe");
    session.submit_if_idle("run").await.expect("submit");
    loop {
        let event = timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("event")
            .expect("recv");
        if matches!(event, RuntimeEvent::ToolStarted { .. }) {
            break;
        }
    }
    sleep(Duration::from_millis(100)).await;
    let loaded = store.load(runtime.session_id()).await.expect("load");
    assert_eq!(loaded.tool_progress.len(), 1);
    assert!(loaded.tool_progress[0].output.len() <= 16 * 1024);
    collect_until_terminal(&mut events).await.expect("collect");
    assert!(
        store
            .load(runtime.session_id())
            .await
            .expect("load after settle")
            .tool_progress
            .is_empty()
    );
    session.close().await.expect("close");
    runtime.join().await.expect("join");
}

#[tokio::test]
async fn bash_cancel_kills_long_running_command() {
    let registry = ToolRegistry::default();
    let cancel = tokio_util::sync::CancellationToken::new();
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
