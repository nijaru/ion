//! Model, admission and observation cuts driven by a file-backed wakeup.
use super::*;
use ion_app::{WakeRecovery, WakeTarget};

pub(super) async fn worker(root: &Path, mode: &str, endpoint: String) {
    let starting = matches!(mode, "wake-arm" | "wake-effect" | "wake-stream");
    let path = root.join("session.db");
    let session = Arc::new(
        if starting {
            Session::create(&path, None)
        } else {
            Session::open(&path)
        }
        .unwrap(),
    );
    let scripts = if starting {
        vec![response(vec![Content::ToolCall(ToolCall {
            id: "host-effect".into(),
            name: "effect".into(),
            arguments: serde_json::json!({}),
            raw_arguments: None,
        })])]
    } else {
        vec![response(vec![Content::Text("recovered".into())])]
    };
    let service = Arc::new(FixtureService {
        inner: ScriptedModelService::new(scripts),
        calls: AtomicUsize::new(0),
        provisional_stream: mode == "wake-stream",
        hold_at: match mode {
            "wake-effect" => Some(1),
            "wake-interrupt" => Some(0),
            _ => None,
        },
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
    });
    let effect = Arc::new(Effect {
        endpoint,
        client: reqwest::Client::builder().no_proxy().build().unwrap(),
        ready: tokio::sync::Semaphore::new(usize::from(!starting)),
    });
    let app = Arc::new(Fixture {
        service: service.clone(),
        effect: effect.clone(),
    });
    let mut control = SessionSetup::new(
        session.clone(),
        SessionCatalog::new(root.to_path_buf(), None),
        app,
        None,
        None,
        None,
    )
    .unwrap()
    .into_control::<()>();
    let key = RequestKey::new("authorized-inbox".into(), "wake-delivery".into()).unwrap();
    if starting {
        assert!(
            control
                .submit_with_wake(&key, message(), |_| async {
                    anyhow::bail!("unavailable alarm")
                })
                .await
                .is_err()
        );
        assert!(session.request_receipt(&key).unwrap().is_none());
        assert!(service.requests().is_empty());
        let accepted = control
            .submit_with_wake(&key, message(), |target| {
                let key = &key;
                let session = &session;
                async move {
                    assert!(
                        session.request_receipt(key).unwrap().is_none(),
                        "wake armed after acceptance"
                    );
                    persist_wake(root, &target)?;
                    if mode == "wake-arm" {
                        println!("ARMED_BEFORE_ADMISSION");
                        std::future::pending::<()>().await;
                    }
                    Ok(())
                }
            })
            .await
            .unwrap();
        let RequestAdmission::Started { receipt, .. } = accepted else {
            panic!("new scheduled admission")
        };
        assert_eq!(receipt.turn, 1);
        assert!(
            matches!(control.submit_with_wake(&key, message(), never_arm).await.unwrap(), RequestAdmission::Existing { receipt: duplicate, .. } if duplicate == receipt)
        );
        let later = RequestKey::new("authorized-inbox".into(), "wake-later".into()).unwrap();
        assert!(
            control
                .submit_with_wake(&later, message(), never_arm)
                .await
                .is_err()
        );
        if mode == "wake-stream" {
            loop {
                match control.next().await {
                    ControlUpdate::Activity(ion_app::Observation::Event { event, .. }) if matches!(&*event, ion_core::AgentEvent::TextDelta { text, .. } if text == "uncommitted draft") =>
                    {
                        break;
                    }
                    ControlUpdate::Settled(_) => panic!("incomplete stream unexpectedly settled"),
                    _ => {}
                }
            }
            println!("PROVISIONAL_MODEL_STREAM");
        } else {
            effect.ready.add_permits(1);
        }
        std::future::pending::<()>().await;
        unreachable!("the parent kills this generation");
    }
    let target: WakeTarget =
        serde_json::from_slice(&fs::read(root.join("fired.json")).unwrap()).unwrap();
    let receipt = session.request_receipt(&key).unwrap();
    if mode == "wake-empty" {
        assert!(matches!(
            control.recover_from_wake(target, never_arm).await.unwrap(),
            WakeRecovery::Quiescent
        ));
        assert!(receipt.is_none());
        assert!(service.requests().is_empty());
        assert!(session.view().unwrap().entries.is_empty());
    } else {
        let receipt = receipt.unwrap();
        assert_eq!(receipt.turn, 1);
        assert!(receipt.end.is_none());
        let wrong = WakeTarget {
            session: target.session,
            key: RequestKey::new("authorized-inbox".into(), "other".into()).unwrap(),
        };
        assert!(matches!(
            control.recover_from_wake(wrong, never_arm).await.unwrap(),
            WakeRecovery::Quiescent
        ));
        let saved = session.recovery_state().unwrap().unwrap().point;
        let before = session.view().unwrap().entries;
        assert!(
            control
                .recover_from_wake(target.clone(), |_| async {
                    anyhow::bail!("unavailable alarm")
                })
                .await
                .is_err()
        );
        assert_eq!(session.view().unwrap().entries, before);
        assert!(service.requests().is_empty());
        let WakeRecovery::Resumed(resumed) = control
            .recover_from_wake(target.clone(), |target| {
                let session = &session;
                async move {
                    assert_eq!(session.recovery_state().unwrap().unwrap().point, saved);
                    persist_wake(root, &target)
                }
            })
            .await
            .unwrap()
        else {
            panic!("expected scheduled recovery")
        };
        assert_eq!(resumed.point.turn, receipt.turn);
        assert!(resumed.point.checkpoint > saved.checkpoint);
        if mode == "wake-interrupt" {
            tokio::time::timeout(Duration::from_secs(5), service.entered.notified())
                .await
                .unwrap();
            // The next fire in the same instance must also leave a wakeup.
            fs::remove_file(root.join("wake.json")).unwrap();
            assert!(matches!(
                control
                    .recover_from_wake(target, |target| async move { persist_wake(root, &target) })
                    .await
                    .unwrap(),
                WakeRecovery::AlreadyOwned
            ));
            println!("RECOVERY_REARMED");
            std::future::pending::<()>().await;
            unreachable!("the parent kills this generation");
        }
        loop {
            if let ControlUpdate::Settled(settlement) = control.next().await {
                let OperationOutcome::Turn(result) = settlement.outcome else {
                    panic!("expected a Turn")
                };
                assert_eq!(result.unwrap(), "recovered");
                break;
            }
        }
        assert!(matches!(
            control.recover_from_wake(target, never_arm).await.unwrap(),
            WakeRecovery::Quiescent
        ));
        let completed = session.request_receipt(&key).unwrap().unwrap();
        assert_eq!(
            completed,
            ion_core::RequestReceipt {
                end: Some(TurnEndReason::Completed),
                ..receipt
            }
        );
        let view = session.view().unwrap();
        assert_eq!(
            view.entries
                .iter()
                .filter(|e| matches!(e, SessionEntry::TurnResumed { .. }))
                .count(),
            if mode == "wake-finish-stream" { 1 } else { 2 }
        );
        let requests = service.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].route.reason, ion_ai::ModelRouteReason::Recovery);
        let content = || requests[0].messages.iter().flat_map(|m| &m.content);
        if mode == "wake-finish-stream" {
            assert!(!content().any(|part| matches!(part, Content::ToolResult(_))
                || matches!(part, Content::Text(text) if text == "uncommitted draft")));
        } else {
            assert!(content().any(|part| matches!(part, Content::ToolResult(result) if if mode == "wake-finish-unknown" { result.is_error } else { !result.is_error && result.result == REMOTE_RESULT })));
        }
    }
    assert!(control.shutdown().await.cleanup_errors.is_empty());
}

async fn never_arm(_: WakeTarget) -> Result<()> {
    panic!("an unmatched, refused, or reconciled request must not replace the wakeup");
}

fn persist_wake(root: &Path, target: &WakeTarget) -> Result<()> {
    let staged = root.join("next-wake.json");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&staged)?;
    serde_json::to_writer(&mut file, target)?;
    file.sync_all()?;
    fs::rename(staged, root.join("wake.json"))?;
    Ok(())
}

pub(super) fn qualify(root: &Path) {
    let empty = root.join("wake-empty");
    fs::create_dir(&empty).unwrap();
    let http = HttpEffect::start(&empty, false);
    kill_at_marker(
        child(&empty, "wake-arm", &http, false),
        "ARMED_BEFORE_ADMISSION",
    );
    fire_wake(&empty);
    wait_worker(child(&empty, "wake-empty", &http, false));
    drop(http);
    assert!(!empty.join("wake.json").exists());
    assert!(!empty.join("effects").exists());

    let stream = root.join("wake-stream");
    fs::create_dir(&stream).unwrap();
    let http = HttpEffect::start(&stream, false);
    kill_at_marker(
        child(&stream, "wake-stream", &http, false),
        "PROVISIONAL_MODEL_STREAM",
    );
    let view = Session::inspect(stream.join("session.db")).unwrap();
    assert_eq!(view.unfinished_turn, Some(1));
    assert!(
        !view
            .entries
            .iter()
            .any(|e| matches!(e, SessionEntry::Assistant { .. }))
    );
    fire_wake(&stream);
    wait_worker(child(&stream, "wake-finish-stream", &http, false));
    drop(http);
    assert!(!stream.join("effects").exists());

    for observed in [false, true] {
        let case = root.join(format!("wake-observed-{observed}"));
        fs::create_dir(&case).unwrap();
        let http = HttpEffect::start(&case, observed);
        let mut first = child(&case, "wake-effect", &http, observed);
        http.effect
            .recv_timeout(Duration::from_secs(15))
            .expect("HTTP dispatch must follow armed admission");
        assert!(case.join("wake.json").exists());
        if observed {
            kill_at_marker(first, "AFTER_OBSERVATION");
        } else {
            first.0.kill().unwrap();
            assert!(!first.0.wait().unwrap().success());
        }
        fire_wake(&case);
        kill_at_marker(
            child(&case, "wake-interrupt", &http, observed),
            "RECOVERY_REARMED",
        );
        fire_wake(&case);
        let mode = if observed {
            "wake-finish-observed"
        } else {
            "wake-finish-unknown"
        };
        wait_worker(child(&case, mode, &http, observed));
        drop(http);
        assert_eq!(fs::read(case.join("effects")).unwrap(), b"effect\n");
        let view = Session::inspect(case.join("session.db")).unwrap();
        assert!(view.unfinished_turn.is_none());
        assert_eq!(
            view.entries
                .iter()
                .filter(|e| matches!(e, SessionEntry::ToolResult { .. }))
                .count(),
            1
        );
    }
}

fn fire_wake(root: &Path) {
    fs::rename(root.join("wake.json"), root.join("fired.json")).unwrap();
}

fn kill_at_marker(mut child: OwnedChild, marker: &'static str) {
    let stdout = child.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let hit = BufReader::new(stdout)
            .lines()
            .any(|line| line.unwrap() == marker);
        let _ = tx.send(hit);
    });
    assert!(rx.recv_timeout(Duration::from_secs(15)).expect(marker));
    child.0.kill().unwrap();
    assert!(!child.0.wait().unwrap().success());
    reader.join().unwrap();
}

fn wait_worker(mut child: OwnedChild) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            return;
        }
        assert!(Instant::now() < deadline, "worker timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
