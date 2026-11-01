use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Write;

fn fixture() -> (PathBuf, PathBuf, Arc<Session>) {
    let root = std::env::temp_dir().join(format!("ion-artifacts-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let path = root.join("source.sqlite");
    let session = Arc::new(Session::create(&path, Some(&root)).unwrap());
    (root, path, session)
}
fn model() -> ModelRef {
    ModelRef {
        provider: "test".into(),
        model: "test".into(),
    }
}
fn bytes(session: &Session, reference: &ArtifactRef) -> Vec<u8> {
    let mut result = Vec::new();
    let mut offset = 0;
    loop {
        let page = session.read_artifact(reference, offset, 16384).unwrap();
        result.extend(page.bytes);
        match page.next_offset {
            Some(next) => offset = next,
            None => return result,
        }
    }
}

#[tokio::test]
async fn originals_survive_reopen_and_prefix_copies_without_the_source_database() {
    let (root, path, session) = fixture();
    let payload = (0..524_311)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let expected = payload.clone();
    let output = session
        .run_user_shell(
            "synthetic capture".into(),
            false,
            CancellationToken::new(),
            move |_, _, sink| async move {
                let mut writer = sink.start("🦀".repeat(100)).unwrap();
                writer.write_bytes(&payload[..7]).await.unwrap();
                writer.write_bytes(&payload[7..]).await.unwrap();
                crate::ToolOutput {
                    value: json!({"stdout":"preview only"}),
                    images: Vec::new(),
                    is_error: false,
                    artifacts: vec![writer.finish_async(true).await.unwrap()],
                }
            },
        )
        .await
        .unwrap();
    let reference = output.artifacts[0].clone();
    assert_eq!(reference.sha256, format!("{:x}", Sha256::digest(&expected)));
    assert_eq!(bytes(&session, &reference), expected);
    let origin: Vec<u8> = session
        .store
        .lock()
        .unwrap()
        .connection
        .query_row(
            "SELECT source FROM artifacts WHERE id=?1",
            [reference.id.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&origin).unwrap(),
        json!({"kind":"user_shell","data":{"admission_entry":1}})
    );
    let query =
        json!({"action":"artifact","entry":1,"artifact":0,"offset":262140,"limit":13}).to_string();
    assert!(
        session
            .prepare_history(&query, 1)
            .unwrap()
            .read(&CancellationToken::new())
            .is_err()
    );
    let page = session
        .prepare_history(&query, 2)
        .unwrap()
        .read(&CancellationToken::new())
        .unwrap();
    assert_eq!(page_bytes(&page), expected[262140..262153]);
    assert_eq!(page["next_offset"], 262153);
    assert_eq!(page["label_truncated"], true);
    assert!(page["label"].as_str().unwrap().len() <= 128);
    let (turn, _) = session
        .begin_turn("finished boundary".into(), model())
        .unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::Text("done".into())],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let fork = session
        .fork_to(root.join("fork.sqlite"), ForkPoint::AfterTurn(turn))
        .unwrap();
    let unclaimed = session
        .artifact_sink()
        .capture("unclaimed", b"not an observed tool result")
        .unwrap();
    let copy = session.clone_to(root.join("copy.sqlite")).unwrap();
    assert!(copy.read_artifact(&unclaimed, 0, 1).is_err());
    assert_eq!(bytes(&fork, &reference), expected);
    drop((output, fork, copy));
    session.shutdown().await.unwrap();
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(bytes(&reopened, &reference), expected);
    drop(reopened);
    fs::remove_file(&path).unwrap();
    let copy = Session::open(root.join("copy.sqlite")).unwrap();
    assert_eq!(bytes(&copy, &reference), expected);
    drop(copy);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn artifact_pages_render_text_without_losing_raw_bytes() {
    let (root, _, session) = fixture();
    let cases = [
        (
            b"plain stdout\n".as_slice(),
            0,
            64,
            "utf8",
            b"plain stdout\n".as_slice(),
            None,
        ),
        ("ab🦀cd".as_bytes(), 0, 4, "utf8", b"ab", Some(2)),
        ("ab🦀cd".as_bytes(), 2, 4, "utf8", "🦀".as_bytes(), Some(6)),
        (
            "ab🦀cd".as_bytes(),
            3,
            4,
            "base64",
            &[0x9f, 0xa6, 0x80, b'c'],
            Some(7),
        ),
        ("ab🦀cd".as_bytes(), 2, 1, "base64", &[0xf0], Some(3)),
        (b"bad\xffdata", 0, 64, "base64", b"bad\xffdata", None),
        (b"a\0b\n", 0, 64, "base64", b"a\0b\n", None),
        (
            b"a\0\xf0\x9f\xa6\x80z",
            0,
            4,
            "base64",
            b"a\0\xf0\x9f",
            Some(4),
        ),
        (b"ok\xf0\x9f", 0, 64, "base64", b"ok\xf0\x9f", None),
        (b"", 0, 64, "utf8", b"", None),
        (b"done", 4, 64, "utf8", b"", None),
    ];
    for (payload, offset, limit, encoding, expected, next) in cases {
        let reference = session
            .artifact_sink()
            .capture("page text", payload)
            .unwrap();
        let store = session.store.lock().unwrap();
        let page = artifacts::page(&store.connection, &reference, offset, limit).unwrap();
        assert_eq!(page["encoding"], encoding, "{payload:?}, {offset}, {limit}");
        assert_eq!(page_bytes(&page), expected);
        assert_eq!(page["offset"], offset);
        assert_eq!(page["next_offset"], json!(next));

        if offset != 0 {
            continue;
        }
        for limit in [1, 2, 3, 4, 7, 8192] {
            let mut rebuilt = Vec::new();
            let mut offset = 0;
            loop {
                let page = artifacts::page(&store.connection, &reference, offset, limit).unwrap();
                let bytes = page_bytes(&page);
                assert!(bytes.len() <= limit);
                rebuilt.extend_from_slice(&bytes);
                let Some(next) = page["next_offset"].as_u64() else {
                    break;
                };
                assert_eq!(next, offset + bytes.len() as u64);
                assert!(next > offset);
                offset = next;
            }
            assert_eq!(rebuilt, payload);
        }
    }
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

fn page_bytes(page: &serde_json::Value) -> Vec<u8> {
    let data = page["data"].as_str().unwrap();
    match page["encoding"].as_str().unwrap() {
        "utf8" => data.as_bytes().to_vec(),
        "base64" => STANDARD.decode(data).unwrap(),
        encoding => panic!("unknown encoding: {encoding}"),
    }
}

#[tokio::test]
async fn private_future_and_opaque_references_do_not_grant_reads() {
    let (root, _, session) = fixture();
    let output = session
        .run_user_shell(
            "private capture".into(),
            true,
            CancellationToken::new(),
            |_, _, sink| async move {
                let reference = sink
                    .start("secret")
                    .unwrap()
                    .finish_async(true)
                    .await
                    .unwrap();
                crate::ToolOutput {
                    artifacts: vec![reference],
                    ..crate::ToolOutput::success(json!({"stdout":"private"}))
                }
            },
        )
        .await
        .unwrap();
    let query = json!({"action":"artifact","entry":1,"artifact":0}).to_string();
    assert!(
        session
            .prepare_history(&query, 0)
            .unwrap()
            .read(&CancellationToken::new())
            .is_err()
    );
    assert!(
        session
            .prepare_history(&query, 2)
            .unwrap()
            .read(&CancellationToken::new())
            .is_err()
    );
    let fake = output.artifacts[0].clone();
    session
        .run_user_shell(
            "opaque imitation".into(),
            false,
            CancellationToken::new(),
            move |_, _, _| async move { crate::ToolOutput::success(json!({"artifacts":[fake]})) },
        )
        .await
        .unwrap();
    let opaque = json!({"action":"artifact","entry":3,"artifact":0}).to_string();
    assert!(
        session
            .prepare_history(&opaque, 4)
            .unwrap()
            .read(&CancellationToken::new())
            .is_err()
    );
    assert!(
        session
            .prepare_history(
                &json!({"action":"artifact","id":output.artifacts[0].id,"artifact":0}).to_string(),
                4
            )
            .is_err()
    );
    let other = Session::create(root.join("other.sqlite"), Some(&root)).unwrap();
    assert!(other.read_artifact(&output.artifacts[0], 0, 1).is_err());
    drop((other, output, session));
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn failed_flush_and_corrupt_chunks_never_become_successful_evidence() {
    let (root, _, session) = fixture();
    session.store.lock().unwrap().connection.execute_batch("CREATE TRIGGER fail_capture BEFORE INSERT ON artifact_chunks BEGIN SELECT RAISE(ABORT,'injected capture failure'); END;").unwrap();
    let mut writer = session.artifact_sink().start("failed").unwrap();
    writer
        .write_bytes(b"buffered, not committed yet")
        .await
        .unwrap();
    assert!(
        writer
            .finish_async(true)
            .await
            .unwrap_err()
            .to_string()
            .contains("injected capture failure")
    );
    let count: i64 = session
        .store
        .lock()
        .unwrap()
        .connection
        .query_row("SELECT count(*) FROM artifacts", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0, "chunk failure rolls back its manifest too");
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute_batch("DROP TRIGGER fail_capture")
        .unwrap();
    let mut writer = session.artifact_sink().start("partial").unwrap();
    writer.write_bytes(b"prefix").await.unwrap();
    let reference = writer.finish_async(false).await.unwrap();
    assert!(!reference.complete);
    assert_eq!(bytes(&session, &reference), b"prefix");
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute(
            "UPDATE artifact_chunks SET body=?1 WHERE artifact=?2",
            params![b"BROKEN".as_slice(), reference.id.to_string()],
        )
        .unwrap();
    assert!(session.read_artifact(&reference, 0, 1).is_err());
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn abandoned_artifact_waits_remain_joined_and_report_storage_failure() {
    use futures_util::FutureExt;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        for finalize in [false, true] {
            let (root, _, session) = fixture();
            session.store.lock().unwrap().connection.execute_batch("CREATE TRIGGER fail_capture BEFORE INSERT ON artifact_chunks BEGIN SELECT RAISE(ABORT,'abandoned capture failure'); END;").unwrap();
            let mut writer = session.artifact_sink().start("abandoned").unwrap();
            if finalize { writer.write_all(b"buffered before finalization").unwrap(); }
            let (release, gate) = std::sync::mpsc::channel::<()>();
            let (entered_tx, entered) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || { let _ = entered_tx.send(()); let _ = gate.recv(); });
            entered.await.unwrap();
            let mut work: std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<()>>>> = if finalize {
                Box::pin(async move { writer.finish_async(true).await.map(|_| ()) })
            } else {
                Box::pin(async move { writer.write_bytes(&vec![b'x';524288]).await })
            };
            assert!(work.as_mut().now_or_never().is_none());
            drop(work);
            let mut closing = Box::pin(session.shutdown());
            assert!(tokio::time::timeout(Duration::from_millis(30), closing.as_mut()).await.is_err(), "shutdown released queued storage before its actual completion");
            drop(closing);
            assert!(session.artifact_sink().start("late").is_err());
            drop(release);
            blocker.await.unwrap();
            let error = session.shutdown().await.unwrap_err();
            assert!(error.to_string().contains("abandoned capture failure"), "{error}");
            drop(session);
            fs::remove_dir_all(root).unwrap();
        }
    });
}

#[test]
fn midstream_flush_preserves_bytes_and_copy_integrity() {
    let (root, _, session) = fixture();
    let mut writer = session.artifact_sink().start("flushed").unwrap();
    writer.write_all(b"a").unwrap();
    writer.flush().unwrap();
    writer.write_all(b"b").unwrap();
    let reference = writer.finish(true).unwrap();
    assert_eq!(bytes(&session, &reference), b"ab");
    let admission = session
        .admit_user_shell("synthetic observation".into(), false)
        .unwrap();
    session
        .record_user_shell(
            admission,
            crate::ToolOutput {
                artifacts: vec![reference.clone()],
                ..crate::ToolOutput::success(json!({}))
            },
        )
        .unwrap();
    let copy = session.clone_to(root.join("flushed-copy.sqlite")).unwrap();
    assert_eq!(bytes(&copy, &reference), b"ab");
    drop((copy, session));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_copy_removes_only_its_new_database_and_preserves_lock_identity() {
    let (root, _, session) = fixture();
    let reference = session
        .artifact_sink()
        .capture("original", b"original")
        .unwrap();
    let admission = session
        .admit_user_shell("synthetic observation".into(), false)
        .unwrap();
    session
        .record_user_shell(
            admission,
            crate::ToolOutput {
                artifacts: vec![reference.clone()],
                ..crate::ToolOutput::success(json!({}))
            },
        )
        .unwrap();
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute(
            "UPDATE artifact_chunks SET body=?1 WHERE artifact=?2",
            params![b"changed!".as_slice(), reference.id.to_string()],
        )
        .unwrap();
    let target = root.join("retry.sqlite");
    assert!(session.clone_to(&target).is_err());
    assert!(
        !target.exists(),
        "failed copy published an unusable destination"
    );
    let lock_path = root.join("retry.sqlite.lock");
    let lock_inode = fs::metadata(&lock_path).unwrap().ino();
    session
        .store
        .lock()
        .unwrap()
        .connection
        .execute(
            "UPDATE artifact_chunks SET body=?1 WHERE artifact=?2",
            params![b"original".as_slice(), reference.id.to_string()],
        )
        .unwrap();
    let copy = session.clone_to(&target).unwrap();
    assert_eq!(fs::metadata(lock_path).unwrap().ino(), lock_inode);
    assert_eq!(bytes(&copy, &reference), b"original");
    assert!(session.clone_to(&target).is_err());
    assert_eq!(
        bytes(&copy, &reference),
        b"original",
        "an existing destination must never be removed"
    );
    drop((copy, session));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn current_child_artifacts_are_scoped_and_bad_offsets_are_request_errors() {
    let (root, _, session) = fixture();
    let (turn, _) = session.begin_turn("compose".into(), model()).unwrap();
    session
        .record_assistant(
            turn,
            Message {
                role: Role::Assistant,
                content: vec![Content::ToolCall(ion_ai::ToolCall {
                    id: "parent".into(),
                    name: "code_mode".into(),
                    arguments: json!({}),
                    raw_arguments: None,
                })],
                provider_replay: None,
            },
            Usage::unknown(),
            false,
        )
        .unwrap();
    let parent = session.tool_occurrence(turn, "parent").unwrap();
    session
        .record_child_intent(
            turn,
            crate::ChildIntent {
                parent,
                child: 0,
                call: ion_ai::ToolCall {
                    id: "child".into(),
                    name: "fixture".into(),
                    arguments: json!({}),
                    raw_arguments: None,
                },
                definition: ion_ai::ToolSpec {
                    name: "fixture".into(),
                    description: String::new(),
                    input_schema: json!({"type":"object"}),
                },
                activity: crate::ToolActivity::external("fixture"),
            },
        )
        .unwrap();
    let query = |offset| {
        json!({"kind":"artifact","parent":parent,"child":0,"artifact":0,"offset":offset})
            .to_string()
    };
    assert!(
        session.prepare_code_inspection(&query(0), parent).is_err(),
        "unobserved intent must not grant bytes"
    );
    let reference = session.artifact_sink().capture("child", b"x").unwrap();
    session
        .record_child_outcome(
            turn,
            parent,
            0,
            crate::ChildOutcome::Observed {
                output: crate::ToolOutput {
                    artifacts: vec![reference],
                    ..crate::ToolOutput::success(json!({}))
                },
            },
        )
        .unwrap();
    assert!(matches!(
        session
            .prepare_code_inspection(&query(2), parent)
            .unwrap()
            .read(),
        Err(super::code_inspection::InspectionError::Request(_))
    ));
    assert_eq!(
        session
            .prepare_code_inspection(&query(0), parent)
            .unwrap()
            .read()
            .unwrap()["data"],
        "x"
    );
    let sibling = crate::ToolOccurrence {
        ordinal: 1,
        ..parent
    };
    assert!(
        session.prepare_code_inspection(&query(0), sibling).is_err(),
        "a sibling cannot use current-child authority"
    );
    drop(session);
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn synchronous_capture_does_not_block_async_shutdown_or_its_deadline() {
    let (root, path, session) = fixture();
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let locker = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        let _ = entered_tx.send(());
        // Independent release bounds the negative control even if Tokio stalls.
        let _ = gate.recv_timeout(Duration::from_secs(1));
        connection.execute_batch("COMMIT").unwrap();
    });
    entered.await.unwrap();
    let sink = session.artifact_sink();
    let writer = std::thread::spawn(move || sink.capture("synchronous", b"blocked until released"));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if session.store.try_lock().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let yielded = tokio::time::timeout(Duration::from_millis(20), session.shutdown())
        .await
        .is_err();
    drop(release);
    let reference = tokio::task::spawn_blocking(move || {
        locker.join().unwrap();
        writer.join().unwrap().unwrap()
    })
    .await
    .unwrap();
    session.shutdown().await.unwrap();
    assert_eq!(bytes(&session, &reference), b"blocked until released");
    drop(session);
    fs::remove_dir_all(root).unwrap();
    assert!(
        yielded,
        "shutdown blocked its Tokio executor behind synchronous storage"
    );
}

#[test]
fn shared_shell_typed_images_remain_durable_but_not_model_or_history_text() {
    let (root, path, session) = fixture();
    let png=STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap();
    let image = ion_ai::ImageContent::from_bytes(&png).unwrap();
    let admission = session
        .admit_user_shell("shared image".into(), false)
        .unwrap();
    session
        .record_user_shell(
            admission,
            crate::ToolOutput {
                images: vec![image.clone()],
                ..crate::ToolOutput::success(json!({"opaque":{"data":"OPAQUE_VALUE_PRESERVED"}}))
            },
        )
        .unwrap();
    let check = |session: &Session| {
        let context = serde_json::to_string(&session.context_messages().unwrap()).unwrap();
        assert!(!context.contains(image.data()));
        assert!(context.contains("OPAQUE_VALUE_PRESERVED"));
        let page = session
            .prepare_history(&json!({"action":"read","entry":admission}).to_string(), 2)
            .unwrap()
            .read(&CancellationToken::new())
            .unwrap();
        let text = serde_json::to_string(&page).unwrap();
        assert!(!text.contains(image.data()));
        assert!(text.contains("OPAQUE_VALUE_PRESERVED"));
        let found = session
            .prepare_history(
                &json!({"action":"search","query":image.data()}).to_string(),
                2,
            )
            .unwrap()
            .read(&CancellationToken::new())
            .unwrap();
        assert!(found["entries"].as_array().unwrap().is_empty());
        assert!(session.view().unwrap().entries.iter().any(|entry| matches!(entry,SessionEntry::UserShellSettled {outcome:UserShellOutcome::Observed {output},..} if output.images==[image.clone()])));
    };
    check(&session);
    drop(session);
    let reopened = Session::open(path).unwrap();
    check(&reopened);
    drop(reopened);
    fs::remove_dir_all(root).unwrap();
}
