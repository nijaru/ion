//! Kill after durable file publication, on either side of SQLite evidence commit.
use super::*;
use std::process::Stdio;

#[tokio::test]
async fn owner_child() {
    let Some(home) = std::env::var_os("ION_ARTIFACT_PROCESS_LOSS_CHILD") else {
        return;
    };
    let home = PathBuf::from(home);
    let committed = std::env::var_os("ION_ARTIFACT_COMMIT_FIRST").is_some();
    let (session, path, turn) = setup(config()).await;
    fs::write(
        home.join("database.json"),
        serde_json::to_vec(&(path, session.session_id())).unwrap(),
    )
    .unwrap();
    let mut tool = ArtifactTool::new(Output::Publish);
    if !committed {
        Arc::get_mut(&mut tool).unwrap().crash_marker = Some(home.join("ready.json"));
    }
    let records = run(&session, turn, &tool).await;
    let reference = tool.saved.lock().unwrap().as_ref().unwrap().1.clone();
    fs::write(
        home.join("ready.json"),
        serde_json::to_vec(&(records.attempts[0].id, reference)).unwrap(),
    )
    .unwrap();
    std::future::pending::<()>().await;
}

#[tokio::test]
async fn artifact_process_loss_before_and_after_evidence_commit() {
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for committed in [false, true] {
        let home = std::env::temp_dir().join(format!("ion-artifact-kill-{}", SessionId::new()));
        fs::create_dir(&home).unwrap();
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "artifacts::process_loss::owner_child",
                "--nocapture",
            ])
            .env("ION_ARTIFACT_PROCESS_LOSS_CHILD", &home)
            .stdout(Stdio::null());
        if committed {
            command.env("ION_ARTIFACT_COMMIT_FIRST", "1");
        }
        let mut child = Child(command.spawn().unwrap());
        let (attempt, reference) = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(ready) = fs::read(home.join("ready.json"))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<(AttemptId, BlobRef)>(&bytes).ok())
                {
                    break ready;
                }
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "child exited before boundary"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("publication/commit boundary deadline");
        child.0.kill().unwrap();
        assert!(!child.0.wait().unwrap().success());
        let (path, id): (PathBuf, SessionId) =
            serde_json::from_slice(&fs::read(home.join("database.json")).unwrap()).unwrap();
        let root = namespace(&path, id);
        assert_eq!(
            fs::read(root.join("objects").join(reference.digest.to_string())).unwrap(),
            CONTENT
        );
        let session = Session::open(&path).await.unwrap();
        let records = session
            .handle()
            .tool_records(tool_step(&session).await)
            .await
            .unwrap();
        let read = session.handle().read_artifact(attempt, 0, 1024).await;
        if committed {
            assert!(matches!(read.unwrap(), ArtifactRead::Page { bytes, .. } if bytes == CONTENT));
            assert!(matches!(
                result(&records.attempts[0]).capture,
                OutputCapture::CompleteArtifact { .. }
            ));
            assert_eq!(
                session
                    .handle()
                    .collect_artifacts()
                    .await
                    .unwrap()
                    .object_count,
                0
            );
        } else {
            assert!(matches!(read, Err(SessionError::NotFound { .. })));
            assert!(matches!(
                records.attempts[0].state,
                ToolAttemptState::IntentCommitted { .. }
            ));
            assert_eq!(
                session
                    .handle()
                    .collect_artifacts()
                    .await
                    .unwrap()
                    .object_count,
                1
            );
        }
        session.close().await.unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir_all(home).unwrap();
    }
}
