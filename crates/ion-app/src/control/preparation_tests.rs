use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
fn image_prompt() -> UserInput {
    UserInput::Prompt {
        text: "  original image\n".into(),
        attachments: vec![AttachmentSource::ImageBytes {
            mime_type: Some(ion_ai::ImageMime::Png),
            bytes: STANDARD.decode(PNG).unwrap(),
        }],
    }
}

// Returned inputs own prepared text; the source path is not a recovery locator.
const FILE_SNAPSHOT: &str = "const note = \"keep 🦀\";\r\n/next literal $ARGUMENTS\n";

async fn file_image_prompt(root: &std::path::Path) -> UserInput {
    let path = root.join("snapshot.ts");
    fs::write(&path, FILE_SNAPSHOT).unwrap();
    let prepared = crate::attachments::prepare_attachments(
        &ion_ai::ModelRef {
            provider: "fixture".into(),
            model: "fixture".into(),
        },
        true,
        vec![AttachmentSource::File(path.clone())],
        1024 * 1024,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    fs::remove_file(path).unwrap();
    let mut input = image_prompt();
    let UserInput::Prompt { attachments, .. } = &mut input else {
        unreachable!()
    };
    attachments.extend(prepared.into_sources());
    input
}

fn assert_file_snapshot(input: &UserInput, root: &std::path::Path) {
    let UserInput::Prompt { attachments, .. } = input else {
        panic!("prompt snapshot")
    };
    let [
        AttachmentSource::ImageBytes { .. },
        AttachmentSource::Text(file),
    ] = &attachments[..]
    else {
        panic!("image and file snapshot")
    };
    assert_eq!(file.name, root.join("snapshot.ts").to_str().unwrap());
    assert_eq!(file.text, FILE_SNAPSHOT);
    assert!(!root.join("snapshot.ts").exists());
}

#[tokio::test]
async fn local_files_use_session_cwd_and_text_only_batches_fail_atomically() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture.control.setup.runtime.allow_file_attachments = true;
    fs::write(fixture.root.join("context.md"), "/not-a-command literal 🦀").unwrap();
    fs::create_dir(fixture.root.join("~")).unwrap();
    fs::write(
        fixture.root.join("~/ion-sdk-literal.md"),
        "SESSION_LITERAL_TILDE",
    )
    .unwrap();
    fs::write(
        fixture.root.join("picture.png"),
        STANDARD.decode(PNG).unwrap(),
    )
    .unwrap();
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                text: "prompt".into(),
                attachments: vec![
                    AttachmentSource::File("context.md".into()),
                    AttachmentSource::File("~/ion-sdk-literal.md".into()),
                ],
            },
            meta("prompt"),
        )
        .unwrap();
    model.release();
    let (_, result) = settle(&mut fixture.control).await;
    assert!(matches!(result.outcome, OperationOutcome::Turn(Ok(_))));
    {
        let requests = model.requests.lock().unwrap();
        assert!(requests[0].messages.iter().any(|message| message.content.iter().any(|part|
            matches!(part, Content::Text(text) if text.contains("/not-a-command literal 🦀") && text.contains("context.md")))));
    }
    assert!(
        model.requests.lock().unwrap()[0]
            .messages
            .iter()
            .any(|message| message.content.iter().any(
                |part| matches!(part, Content::Text(text) if text.contains("SESSION_LITERAL_TILDE"))
            ))
    );
    fs::remove_file(fixture.root.join("context.md")).unwrap();
    let entries = fixture.control.session().view().unwrap().entries;
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                text: "never partly accepted".into(),
                attachments: vec![
                    AttachmentSource::Text(crate::attachments::TextFile {
                        name: "memory.md".into(),
                        text: "retained literal".into(),
                    }),
                    AttachmentSource::File("picture.png".into()),
                ],
            },
            meta("original draft"),
        )
        .unwrap();
    let mut returned = None;
    loop {
        match fixture.control.next().await {
            ControlUpdate::Returned(input) => returned = Some(input),
            ControlUpdate::Idle => break,
            _ => {}
        }
    }
    let returned = returned.expect("unsupported detected image returns the whole input");
    assert_eq!(returned.metadata.literal, "original draft");
    assert!(
        matches!(returned.input, UserInput::Prompt { text, attachments }
        if text == "never partly accepted" && matches!(&attachments[..],
            [AttachmentSource::Text(file), AttachmentSource::File(path)]
            if file.text == "retained literal" && path == std::path::Path::new("picture.png")))
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.control.session().view().unwrap().entries, entries);
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn prepared_snapshots_exhausting_custody_budget_return_the_original_file_source() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), false);
    fixture.control.setup.runtime.allow_file_attachments = true;
    fixture.control.budget = ion_core::InputBudget::new(2048);
    fs::write(fixture.root.join("large.md"), "x".repeat(2048)).unwrap();
    let entries = fixture.control.session().view().unwrap().entries;
    fixture
        .control
        .submit(
            Delivery::Start,
            UserInput::Prompt {
                text: "retain literal".into(),
                attachments: vec![AttachmentSource::File("large.md".into())],
            },
            meta("caret and draft"),
        )
        .unwrap();
    let mut returned = None;
    loop {
        match fixture.control.next().await {
            ControlUpdate::Returned(input) => returned = Some(input),
            ControlUpdate::Idle => break,
            _ => {}
        }
    }
    let returned = returned.expect("prepared custody must fit alongside the original");
    assert!(
        matches!(returned.reason, ReturnReason::Preparation(ref error)
        if matches!(error.downcast_ref::<ion_core::AgentError>(), Some(ion_core::AgentError::InputQueueFull {max_encoded_bytes:2048})))
    );
    assert_eq!(returned.metadata.literal, "caret and draft");
    assert!(
        matches!(returned.input, UserInput::Prompt { text, attachments }
        if text == "retain literal" && matches!(&attachments[..], [AttachmentSource::File(path)] if path == std::path::Path::new("large.md")))
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.control.session().view().unwrap().entries, entries);
    fixture.control.shutdown().await;
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

// Gate the preparation result, not the whole shared blocking pool: Core now
// legitimately needs that pool to admit and settle the concurrently running Turn.
async fn hold_preparation(
    control: &mut SessionControl<Metadata>,
) -> tokio::sync::oneshot::Sender<()> {
    let (id, mut preparation) = control.preparing.take().expect("image preparation");
    let result = preparation.join().await;
    let (release, waiting) = tokio::sync::oneshot::channel();
    control.preparing = Some((
        id,
        crate::preparation::Preparation::start(CancellationToken::new(), move |_| async move {
            waiting
                .await
                .context("release retained image preparation")?;
            result
        }),
    ));
    release
}

#[tokio::test]
async fn literal_files_survive_expansion_image_preparation_and_rejected_input() {
    let model = Model::new(1);
    let mut fixture = Fixture::new(model.clone(), true);
    let prompts = fixture.root.join(".ion/prompts");
    fs::create_dir_all(&prompts).unwrap();
    fs::write(prompts.join("next.md"), "EXPANDED $1").unwrap();
    fixture.control.reload_resources().await.unwrap();
    let file = || crate::attachments::TextFile {
        name: "source.md".into(),
        text: "/next dangerous\n{{ARG}}\n".into(),
    };
    let mut input = image_prompt();
    let UserInput::Prompt {
        text, attachments, ..
    } = &mut input
    else {
        unreachable!()
    };
    *text = "/next one".into();
    attachments.push(AttachmentSource::Text(file()));
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
                attachments: vec![AttachmentSource::Text(file())],
            },
            meta("/next '"),
        )
        .unwrap_err();
    let UserInput::Prompt {
        text, attachments, ..
    } = rejected.input
    else {
        panic!("original")
    };
    assert_eq!(text, "/next '");
    assert!(
        matches!(&attachments[0], AttachmentSource::Text(retained) if retained.text == file().text)
    );
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
                text: "root".into(),
                attachments: vec![AttachmentSource::ImageFile("absent.png".into())],
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
                ControlUpdate::Returned(input) => returned.push(input),
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
    assert_eq!(
        returned.iter().map(|input| input.id).collect::<Vec<_>>(),
        [root, queued]
    );
    assert!(matches!(returned[0].reason, ReturnReason::Preparation(_)));
    assert!(matches!(
        returned[1].reason,
        ReturnReason::PredecessorFailed
    ));
    assert!(!fixture.control.state().stop_requested);
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
    fixture.control.reload_resources().await.unwrap();
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
                text: "/check ARG".into(),
                attachments: vec![image.into()],
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
                text: "literal".into(),
                attachments: vec![AttachmentSource::ImageFile("missing image.png".into())],
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
        fixture.control.new_session().await.is_err(),
        "unreturned input cannot move to another binding"
    );
    match fixture.control.next().await {
        ControlUpdate::Returned(input) => {
            assert_eq!(input.id, id);
            assert_eq!(input.metadata.literal, "original");
            assert!(matches!(input.reason, ReturnReason::Preparation(_)));
            assert!(
                matches!(input.input, UserInput::Prompt { text, attachments, .. } if text == "literal" && matches!(&attachments[..], [AttachmentSource::ImageFile(path)] if path == std::path::Path::new("missing image.png")))
            );
        }
        _ => panic!("preparation refusal must return input"),
    }
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}

#[test]
fn preparation_overlaps_turns_but_a_late_steer_never_retargets() {
    runtime().block_on(async {
        let model = Model::new(1);
        let mut fixture = Fixture::new(model.clone(), true);
        fixture.control.submit(Delivery::Start, prompt("root"), meta("root")).unwrap();
        model.wait_calls(1).await;
        let target = fixture.control.current_operation().unwrap();
        let original = file_image_prompt(&fixture.root).await;
        let steer = fixture.control.submit(Delivery::Steer(target), original, meta("  literal 🦀\n")).unwrap();
        let release = hold_preparation(&mut fixture.control).await;
        let snapshot = fixture.control.snapshot(|_| ()).await.unwrap();
        let input = snapshot.inputs.iter().find(|input| input.id == steer).unwrap();
        assert_eq!(input.phase, InputCustodyPhase::PendingPreparation);
        assert_file_snapshot(input.original.as_ref().unwrap(), &fixture.root);
        assert!(matches!(&input.original, Some(UserInput::Prompt { text, attachments, .. })
            if text == "  original image\n" && matches!(&attachments[..], [AttachmentSource::ImageBytes { bytes, .. }, AttachmentSource::Text(_)] if bytes == &STANDARD.decode(PNG).unwrap())));
        fixture.control.submit(Delivery::FollowUp, prompt("explicit next"), meta("followup")).unwrap();
        model.release();
        let (_, first) = settle(&mut fixture.control).await;
        assert!(matches!(first.outcome, OperationOutcome::Turn(Ok(_))));
        model.wait_calls(2).await;
        assert_ne!(fixture.control.current_operation(), Some(target));
        assert!(fixture.control.preparing.is_some(), "preparation must remain in retained custody while another Turn starts");
        assert!(fixture.control.new_session().await.is_err());
        fixture.control.reload_resources().await.unwrap();
        model.release();
        let (_, second) = settle(&mut fixture.control).await;
        assert!(matches!(second.outcome, OperationOutcome::Turn(Ok(_))));
        release.send(()).unwrap();
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
        assert_file_snapshot(&returned.input, &fixture.root);
        assert!(matches!(returned.input, UserInput::Prompt { text, attachments, .. } if text == "  original image\n" && matches!(&attachments[..], [AttachmentSource::ImageBytes { mime_type: Some(ion_ai::ImageMime::Png), bytes }, AttachmentSource::Text(_)] if bytes == &STANDARD.decode(PNG).unwrap())));
        assert!(model.requests.lock().unwrap().iter().flat_map(|request| &request.messages).flat_map(|message| &message.content).all(|part| !matches!(part, Content::Image(_)) && !matches!(part, Content::Text(text) if text.contains("/next literal $ARGUMENTS"))));
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
            matches!(&returned[0].input, UserInput::Prompt { attachments, .. } if attachments.len() == 1)
        );
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        assert!(report.cleanup_errors.is_empty());
    });
}

#[test]
fn joining_preparation_preserves_the_first_withdrawal_cause() {
    runtime().block_on(async {
        for cause in ["failure", "clear", "stop"] {
            let model = Model::new(1);
            let mut fixture = Fixture::new(model.clone(), true);
            fixture.fault("json_extract(NEW.body, '$.kind') = 'assistant'");
            fixture
                .control
                .submit(Delivery::Start, prompt("root"), meta("root"))
                .unwrap();
            model.wait_calls(1).await;
            let original = file_image_prompt(&fixture.root).await;
            let queued = fixture
                .control
                .submit(Delivery::FollowUp, original, meta("original draft"))
                .unwrap();
            let release = hold_preparation(&mut fixture.control).await;
            match cause {
                "clear" => fixture.control.clear_queue(),
                "stop" => fixture.control.request_stop(),
                _ => {}
            }
            model.release();
            let (returned, settlement) = settle(&mut fixture.control).await;
            assert!(
                returned.is_empty(),
                "preparation must join before returning the image"
            );
            assert!(matches!(settlement.outcome, OperationOutcome::Turn(Err(_))));
            assert_eq!(settlement.stop_requested, cause == "stop");
            let snapshot = fixture.control.snapshot(|_| ()).await.unwrap();
            let withdrawal = &snapshot
                .inputs
                .iter()
                .find(|input| input.id == queued)
                .unwrap()
                .withdrawal;
            assert!(matches!(
                (cause, withdrawal),
                ("failure", Some(ReturnSnapshot::PredecessorFailed))
                    | ("clear", Some(ReturnSnapshot::Cleared))
                    | ("stop", Some(ReturnSnapshot::Stopped))
            ));
            // Later cleanup and queue controls may stop the worker, not rewrite
            // the cause already published in the pull snapshot.
            fixture.control.request_stop();
            fixture.control.clear_queue();
            release.send(()).unwrap();
            let report = fixture.control.shutdown().await;
            let returned = report
                .updates
                .into_iter()
                .filter_map(|update| match update {
                    ControlUpdate::Returned(input) => Some(input),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(returned.len(), 1);
            let input = &returned[0];
            assert_eq!(input.id, queued);
            assert_eq!(input.metadata.literal, "original draft");
            assert_file_snapshot(&input.input, &fixture.root);
            assert!(
                matches!(&input.input, UserInput::Prompt { text, attachments, .. }
                if text == "  original image\n" && attachments.len() == 2)
            );
            assert!(matches!(
                (cause, &input.reason),
                ("failure", ReturnReason::PredecessorFailed)
                    | ("clear", ReturnReason::Cleared)
                    | ("stop", ReturnReason::Stopped)
            ));
            assert_eq!(model.calls.load(Ordering::SeqCst), 1);
            assert_eq!(fixture.control.session().view().unwrap().turns().len(), 1);
        }
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
