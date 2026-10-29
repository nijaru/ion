use super::*;

const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
fn image_prompt() -> UserInput {
    UserInput::Prompt {
        files: Vec::new(),
        text: "  original image\n".into(),
        images: vec![ImageSource::Encoded {
            mime_type: "image/png".into(),
            data: PNG.into(),
        }],
    }
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap()
}
async fn block_decoder() -> (std::sync::mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
    let (release, held) = std::sync::mpsc::channel();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let worker = tokio::task::spawn_blocking(move || {
        entered.send(()).unwrap();
        held.recv().unwrap();
    });
    waiting.await.unwrap();
    (release, worker)
}

#[tokio::test]
async fn literal_files_survive_expansion_image_preparation_and_rejected_input() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), true);
    let prompts = fixture.root.join(".ion/prompts");
    fs::create_dir_all(&prompts).unwrap();
    fs::write(prompts.join("next.md"), "EXPANDED $1").unwrap();
    fixture.control.reload_resources().unwrap();
    let file = || crate::prompt_files::TextFile {
        name: "source.md".into(),
        text: "/next dangerous\n{{ARG}}\n".into(),
    };
    let mut input = image_prompt();
    let UserInput::Prompt { text, files, .. } = &mut input else {
        unreachable!()
    };
    *text = "/next one".into();
    files.push(file());
    fixture
        .control
        .submit(Delivery::Start, input, meta("/next one"))
        .unwrap();
    model.release();
    let (_, settlement) = settle(&mut fixture.control).await;
    assert!(matches!(settlement.outcome, OperationOutcome::Turn(Ok(_))));
    let request = model.requests.lock().unwrap()[0].clone();
    let user = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == Role::User)
        .unwrap();
    let Content::Text(text) = &user.content[0] else {
        panic!("text")
    };
    assert!(text.contains("EXPANDED one"));
    assert!(text.contains("/next dangerous"));
    assert!(!text.contains("EXPANDED dangerous"));
    assert_eq!(
        user.content
            .iter()
            .filter(|part| matches!(part, Content::Image(_)))
            .count(),
        1
    );
    let rejected = fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                text: "/next '".into(),
                images: Vec::new(),
                files: vec![file()],
            },
            meta("/next '"),
        )
        .unwrap_err();
    let UserInput::Prompt { text, files, .. } = rejected.input else {
        panic!("original")
    };
    assert_eq!(text, "/next '");
    assert_eq!(files[0].text, file().text);
    assert_eq!(rejected.metadata.literal, "/next '");
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn failed_initial_preparation_returns_queued_work_without_starting_it() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), true);
    let root = fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                files: Vec::new(),
                text: "root".into(),
                images: vec![ImageSource::Path("absent.png".into())],
            },
            meta("original root"),
        )
        .unwrap();
    let queued = fixture
        .control
        .submit(
            Delivery::FollowUp,
            prompt("do not run"),
            meta("queued literal"),
        )
        .unwrap();
    let mut returned = Vec::new();
    let mut admitted = 0;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match fixture.control.next().await {
                ControlUpdate::Returned(input) => returned.push(input.id),
                ControlUpdate::Accepted { .. } => {
                    admitted += 1;
                    fixture.control.request_stop();
                }
                ControlUpdate::Idle => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        admitted, 0,
        "failed root preparation cannot start another Turn"
    );
    assert_eq!(returned, [root, queued]);
    assert!(fixture.control.session().view().unwrap().turns().is_empty());
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn normalized_attachments_keep_notes_while_prompt_resources_expand() {
    use base64::Engine as _;
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), true);
    fs::create_dir_all(fixture.root.join(".ion/prompts")).unwrap();
    fs::write(fixture.root.join(".ion/prompts/check.md"), "expanded $1").unwrap();
    fixture.control.reload_resources().unwrap();
    let mut image = ion_ai::normalize_image(
        &base64::engine::general_purpose::STANDARD
            .decode(PNG)
            .unwrap(),
    )
    .unwrap();
    image.note = Some("coordinates refer to this exact image".into());
    let expected = Message::user_input("expanded ARG".into(), [image.clone()]);
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                files: Vec::new(),
                text: "/check ARG".into(),
                images: vec![image.into()],
            },
            meta("  literal /check ARG\n"),
        )
        .unwrap();
    loop {
        if let ControlUpdate::Accepted { .. } = fixture.control.next().await {
            break;
        }
    }
    model.wait_calls(1).await;
    assert_eq!(
        model.requests.lock().unwrap()[0].messages.last(),
        Some(&expected)
    );
    model.release();
    let (returned, settlement) = settle(&mut fixture.control).await;
    assert!(returned.is_empty());
    assert_eq!(
        settlement.accepted[0].metadata.literal,
        "  literal /check ARG\n"
    );
    assert!(fixture.control.session().view().unwrap().entries.iter().any(|entry| matches!(entry, ion_core::SessionEntry::TurnStarted { input, .. } if input == &expected)));
}

#[tokio::test]
async fn preparation_refusal_retains_exact_input_and_blocks_silent_replacement() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), true);
    let id = fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                files: Vec::new(),
                text: "literal".into(),
                images: vec![ImageSource::Path("missing image.png".into())],
            },
            meta("original"),
        )
        .unwrap();
    assert!(matches!(
        fixture.control.next().await,
        ControlUpdate::Input {
            phase: InputPhase::Preparing,
            ..
        }
    ));
    while fixture.control.preparing.is_some() {
        fixture.control.drive(ActivityRetention::Discard).await;
    }
    assert!(!fixture.control.is_busy());
    assert!(
        fixture.control.new_session().is_err(),
        "unreturned input cannot move to another binding"
    );
    match fixture.control.next().await {
        ControlUpdate::Returned(input) => {
            assert_eq!(input.id, id);
            assert_eq!(input.metadata.literal, "original");
            assert!(matches!(input.reason, ReturnReason::Preparation(_)));
            assert!(
                matches!(input.input, UserInput::Prompt { text, images, .. } if text == "literal" && matches!(&images[..], [ImageSource::Path(path)] if path == std::path::Path::new("missing image.png")))
            );
        }
        _ => panic!("preparation refusal must return input"),
    }
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn preparation_overlaps_turns_but_a_late_steer_never_retargets() {
    runtime().block_on(async {
        let (release, blocker) = block_decoder().await;
        let model = Model::new(1);
        let mut fixture = Fixture::new(model.clone(), true);
        fixture.control.submit(Delivery::Start, prompt("root"), meta("root")).unwrap();
        model.wait_calls(1).await;
        let target = fixture.control.current_operation().unwrap();
        let steer = fixture.control.submit(Delivery::Steer(target), image_prompt(), meta("  literal 🦀\n")).unwrap();
        let snapshot = fixture.control.snapshot(|_| ()).unwrap();
        let input = snapshot.inputs.iter().find(|input| input.id == steer).unwrap();
        assert_eq!(input.phase, InputCustodyPhase::PendingPreparation);
        assert!(matches!(&input.original, Some(UserInput::Prompt { text, images, .. })
            if text == "  original image\n" && matches!(&images[..], [ImageSource::Encoded { data, .. }] if data == PNG)));
        fixture.control.submit(Delivery::FollowUp, prompt("explicit next"), meta("followup")).unwrap();
        model.release();
        let (_, first) = settle(&mut fixture.control).await;
        assert!(matches!(first.outcome, OperationOutcome::Turn(Ok(_))));
        model.wait_calls(2).await;
        assert_ne!(fixture.control.current_operation(), Some(target));
        assert!(fixture.control.preparing.is_some(), "decoder must remain in retained custody while another Turn starts");
        assert!(fixture.control.new_session().is_err());
        fixture.control.reload_resources().unwrap();
        model.release();
        let (_, second) = settle(&mut fixture.control).await;
        assert!(matches!(second.outcome, OperationOutcome::Turn(Ok(_))));
        release.send(()).unwrap();
        blocker.await.unwrap();
        let returned = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match fixture.control.next().await {
                    ControlUpdate::Returned(input) => break input,
                    ControlUpdate::Idle => panic!("late steering must be returned"),
                    _ => {}
                }
            }
        }).await.unwrap();
        assert_eq!(returned.id, steer);
        assert_eq!(returned.delivery, Delivery::Steer(target));
        assert_eq!(returned.metadata.literal, "  literal 🦀\n");
        assert!(matches!(returned.reason, ReturnReason::TurnEnded));
        assert!(matches!(returned.input, UserInput::Prompt { text, images, .. } if text == "  original image\n" && matches!(&images[..], [ImageSource::Encoded { mime_type, data }] if mime_type == "image/png" && data == PNG)));
        assert!(model.requests.lock().unwrap().iter().flat_map(|request| &request.messages).flat_map(|message| &message.content).all(|part| !matches!(part, Content::Image(_))));
        assert_eq!(model.calls.load(Ordering::SeqCst), 2);
    });
}

#[test]
fn clear_preserves_initial_preparation_and_abandoned_shutdown_joins_it() {
    runtime().block_on(async {
        let (release, blocker) = block_decoder().await;
        let model = Model::new(1);
        let mut fixture = Fixture::new(model.clone(), true);
        let initial = fixture
            .control
            .submit(Delivery::Start, image_prompt(), meta("initial"))
            .unwrap();
        let queued = fixture
            .control
            .submit(Delivery::FollowUp, prompt("clear only me"), meta("queued"))
            .unwrap();
        fixture.control.clear_queue();
        let mut returned_queue = false;
        while !returned_queue {
            if let ControlUpdate::Returned(input) = fixture.control.next().await {
                assert_eq!(input.id, queued);
                assert!(matches!(input.reason, ReturnReason::Cleared));
                returned_queue = true;
            }
        }
        // The initial decoder has been scheduled into the bounded blocking pool.
        tokio::task::yield_now().await;
        assert_eq!(fixture.control.starting.map(|(id, _)| id), Some(initial));
        assert!(futures_util::poll!(Box::pin(fixture.control.next())).is_pending());
        assert!(futures_util::poll!(Box::pin(fixture.control.shutdown())).is_pending());
        assert!(fixture.control.closed);
        assert!(fixture.control.preparing.is_some());
        release.send(()).unwrap();
        blocker.await.unwrap();
        let report = fixture.control.shutdown().await;
        let returned: Vec<_> = report
            .updates
            .into_iter()
            .filter_map(|update| match update {
                ControlUpdate::Returned(input) => Some(input),
                _ => None,
            })
            .collect();
        assert_eq!(returned.len(), 1);
        assert_eq!(returned[0].id, initial);
        assert_eq!(returned[0].metadata.literal, "initial");
        assert!(matches!(returned[0].reason, ReturnReason::Stopped));
        assert!(
            matches!(&returned[0].input, UserInput::Prompt { images, .. } if images.len() == 1)
        );
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        assert!(report.cleanup_errors.is_empty());
    });
}

#[test]
fn stop_wins_over_successful_unobserved_preparation() {
    runtime().block_on(async {
        let model = Model::new(1);
        let mut fixture = Fixture::new(model.clone(), true);
        let initial = fixture
            .control
            .submit(Delivery::Start, image_prompt(), meta("exact initial"))
            .unwrap();
        // Exercise the exact completed-worker / stop-intent boundary without
        // relying on decoder timing. The normal driver runs this same transition.
        let (_, preparation) = fixture.control.preparing.as_mut().unwrap();
        let result = preparation.join().await;
        assert!(result.is_ok());
        fixture.control.request_stop();
        fixture.control.finish_preparation(result);
        let report = fixture.control.shutdown().await;
        let returned: Vec<_> = report
            .updates
            .into_iter()
            .filter_map(|update| match update {
                ControlUpdate::Returned(input) => Some(input),
                _ => None,
            })
            .collect();
        assert_eq!(returned.len(), 1);
        assert_eq!(returned[0].id, initial);
        assert!(matches!(returned[0].reason, ReturnReason::Stopped));
        assert_eq!(returned[0].metadata.literal, "exact initial");
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    });
}
