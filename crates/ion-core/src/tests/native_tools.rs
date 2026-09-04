//! Native tools tests.

use super::support::*;

// ---- Tool execution (read/write/edit/search/find/bash) unit tests ----

#[tokio::test]
async fn bash_timeout_kills_the_owned_process_group() {
    let registry = ToolRegistry::with_cwd(".");
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        registry.execute(
            "bash",
            &json!({"command": "trap '' TERM; sleep 30", "timeout": 0.05}),
            tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await
    .expect("timed-out bash must settle");
    assert!(outcome.is_error, "{outcome:?}");
    assert!(
        outcome.output.contains("timed out after 0.05 seconds"),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn bash_timeout_is_optional_and_invalid_values_are_rejected() {
    let registry = ToolRegistry::with_cwd(".");
    let cancel = tokio_util::sync::CancellationToken::new();

    for timeout in [json!(0), json!(-1), json!("fast")] {
        let outcome = registry
            .execute(
                "bash",
                &json!({"command": "true", "timeout": timeout}),
                cancel.clone(),
            )
            .await;
        assert!(
            outcome.is_error,
            "invalid timeout accepted: {timeout}: {outcome:?}"
        );
        assert!(
            outcome
                .output
                .contains("timeout must be a finite positive number"),
            "{outcome:?}"
        );
    }

    let outcome = registry
        .execute("bash", &json!({"command": "printf no-timeout"}), cancel)
        .await;
    assert!(!outcome.is_error, "{outcome:?}");
    assert_eq!(outcome.output, "no-timeout");
}

#[tokio::test]
async fn read_returns_images_for_image_files_and_notes_the_size_cap() {
    let tmp = std::env::temp_dir().join(format!("ion-tool-img-{}-{}", std::process::id(), 2));
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::create_dir_all(&tmp);
    let registry = ToolRegistry::with_cwd(&tmp);
    let cancel = tokio_util::sync::CancellationToken::new();

    // A minimal valid PNG (1x1 transparent).
    let png: &[u8] = &[
        0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R', 0,
        0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89, 0, 0, 0, 0x0D, b'I', b'D',
        b'A', b'T', 0x78, 0xDA, 0x63, 0x64, 0x60, 0xF8, 0x5F, 0x0F, 0x00, 0x02, 0x87, 0x01, 0x80,
        0xEB, 0x47, 0xBA, 0x92, 0, 0, 0, 0, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    std::fs::write(tmp.join("shot.png"), png).expect("write png");

    // Relative path inside the project root.
    let out = registry
        .execute("read", &json!({"path":"shot.png"}), cancel.clone())
        .await;
    assert!(!out.is_error, "{out:?}");
    assert_eq!(out.output, "Read image file [image/png]");
    let [image] = &out.images[..] else {
        panic!("expected exactly one image, got {out:?}");
    };
    assert_eq!(image.mime_type, "image/png");
    let decoded_len = out
        .images
        .iter()
        .map(|image| crate::tool::base64_decode_len(&image.data))
        .sum::<usize>();
    assert_eq!(decoded_len, png.len(), "base64 must round-trip the bytes");

    // An absolute path outside the root is readable: the pasted
    // clipboard image lives in the system temp directory.
    let abs = tmp.join("shot.png");
    let out = registry
        .execute(
            "read",
            &json!({"path": abs.to_string_lossy()}),
            cancel.clone(),
        )
        .await;
    assert!(!out.is_error, "absolute image read failed: {out:?}");
    assert_eq!(out.images.len(), 1);

    // A truncated/garbage file with an image header is not silently
    // misread: the magic bytes still classify it, size caps apply at
    // the outcome level (too-large is refused).
    let mut big = vec![0u8; 9 * 1024 * 1024];
    big[..4].copy_from_slice(&[0x89, b'P', b'N', b'G']);
    std::fs::write(tmp.join("big.png"), &big).expect("write big png");
    let out = registry
        .execute("read", &json!({"path":"big.png"}), cancel)
        .await;
    assert!(out.is_error, "oversized image must be refused: {out:?}");
    assert!(out.output.contains("image too large"), "{out:?}");

    let _ = std::fs::remove_dir_all(&tmp);
}

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
    let progress_event = timeout(Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.expect("event");
            if let RuntimeEvent::ToolProgress { output, .. } = event {
                break output;
            }
        }
    })
    .await
    .expect("tool progress event");
    assert!(progress_event.len() <= 16 * 1024);
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

// ---- Edit diff hunks (display payload + model verification) ----

#[test]
fn edit_diff_hunk_positions_context_and_headers() {
    let original = "one\ntwo\nthree\nfour\nfive\n";
    let diff = crate::tool::edit_diff_hunk("f.txt", original, "three", "THREE", 1, 60)
        .expect("hunk resolves");
    let lines: Vec<&str> = diff.lines().collect();
    assert_eq!(lines[0], "--- f.txt");
    assert_eq!(lines[1], "+++ f.txt");
    assert_eq!(lines[2], "@@ -2,3 +2,3 @@");
    assert_eq!(&lines[3..], &[" two", "-three", "+THREE", " four"]);
}

#[test]
fn edit_diff_hunk_clamps_context_at_file_edges() {
    let original = "one\ntwo\n";
    let diff =
        crate::tool::edit_diff_hunk("f.txt", original, "one", "ONE", 3, 60).expect("hunk resolves");
    let lines: Vec<&str> = diff.lines().collect();
    assert_eq!(lines[2], "@@ -1,2 +1,2 @@");
    assert_eq!(&lines[3..], &["-one", "+ONE", " two"]);
}

#[test]
fn edit_diff_hunk_truncates_with_a_truthful_marker() {
    let original = (1..=20)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let diff = crate::tool::edit_diff_hunk("f.txt", &original, "line 10", "LINE", 0, 1)
        .expect("hunk resolves");
    let lines: Vec<&str> = diff.lines().collect();
    // 3 headers + 1 kept body line + 1 truncation marker.
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[3], "-line 10");
    assert_eq!(lines[4], "… (1 more line)", "got: {}", lines[4]);
}

#[test]
fn edit_diff_hunk_is_none_when_old_str_is_absent() {
    assert!(
        crate::tool::edit_diff_hunk("f.txt", "abc", "zzz", "x", 3, 60).is_none(),
        "absent old_str must not produce an empty diff"
    );
}

#[tokio::test]
async fn edit_outcome_carries_the_hunk_instead_of_a_constant() {
    let root = tempfile::tempdir().expect("root tempdir");
    std::fs::write(root.path().join("note.txt"), "hello world\n").expect("seed file");
    let registry = ToolRegistry::with_cwd(root.path());
    let out = registry
        .execute(
            "edit",
            &json!({"path":"note.txt","old_str":"world","new_str":"ion"}),
            CancellationToken::new(),
        )
        .await;
    assert!(!out.is_error, "edit failed: {out:?}");
    assert!(
        out.output.starts_with("--- note.txt\n+++ note.txt\n@@ "),
        "got: {}",
        out.output
    );
    assert!(out.output.contains("-hello world"), "got: {}", out.output);
    assert!(out.output.contains("+hello ion"), "got: {}", out.output);
}
