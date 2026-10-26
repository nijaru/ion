//! Client custody/projection checks. Host's focused tests own queue, expansion,
//! stop/acceptance races and decoder lifetimes; do not duplicate those internals.
use super::activity::OperationKind;
use super::*;
use ion_ai::ModelRef;
use ion_host::{ControlUpdate, Host, OperationOutcome, SavedSelection, SessionControl, Wire};
use std::sync::Arc;

#[test]
fn settlement_program_status_distinguishes_done_and_cancelled() {
    assert_eq!(
        settlement_program_status(&OperationOutcome::Turn(Ok("answer".into()))),
        ProgramStatus::Done
    );
    assert_eq!(
        settlement_program_status(&OperationOutcome::Turn(Err(
            ion_core::CodingAgentError::Cancelled
        ))),
        ProgramStatus::Idle
    );
    assert_eq!(
        settlement_program_status(&OperationOutcome::Compact(Err(
            ion_core::CodingAgentError::Cancelled
        ))),
        ProgramStatus::Idle
    );
    assert_eq!(
        settlement_program_status(&OperationOutcome::Turn(Err(
            ion_core::CodingAgentError::EmptyPrompt
        ))),
        ProgramStatus::Error
    );
    for (is_error, cancelled, expected) in [
        (false, false, ProgramStatus::Done),
        (true, false, ProgramStatus::Error),
        (true, true, ProgramStatus::Idle),
    ] {
        let output = ion_core::CodingToolOutput {
            value: serde_json::json!({"cancelled": cancelled}),
            images: vec![],
            is_error,
        };
        assert_eq!(
            settlement_program_status(&OperationOutcome::Shell(Ok(output))),
            expected
        );
    }
}

#[tokio::test]
async fn preparation_without_an_operation_restores_the_retained_status() {
    let mut fixture = Fixture::new();
    for resting in [
        ProgramStatus::Idle,
        ProgramStatus::Done,
        ProgramStatus::Error,
    ] {
        let mut program = ProgramStatusState::default();
        program.settle(resting);
        let mut ui = Frontend {
            images: vec![ImageSource::Path(fixture.root.join("missing.png"))],
            ..Frontend::default()
        };
        submit_editor(
            &mut fixture.control,
            &mut ui,
            Delivery::Start,
            EditorDraft::at_end("prepare image".into()),
        );
        assert_eq!(
            program.status(fixture.control.is_busy()),
            ProgramStatus::Working
        );
        let mut activity = Activity::default();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let update = fixture.control.next().await;
                assert!(
                    !matches!(update, ControlUpdate::Settled(_)),
                    "no operation should start"
                );
                let idle = matches!(update, ControlUpdate::Idle);
                program.observe(&update);
                assert!(
                    activity
                        .update(&mut ui, &fixture.control, update)
                        .unwrap()
                        .is_none()
                );
                if idle {
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(!fixture.control.is_busy());
        assert_eq!(program.status(fixture.control.is_busy()), resting);
        program.blocked_auth = true;
        assert_eq!(program.status(false), ProgramStatus::BlockedAuth);
        program.blocked_auth = false;
        assert_eq!(program.status(false), resting);
        assert_eq!(ui.draft, "prepare image");
        assert_eq!(ui.status, "Draft returned");
    }
    fixture.control.shutdown().await;
}

pub(super) struct Fixture {
    pub(super) root: PathBuf,
    pub(super) control: SessionControl<EditorDraft>,
    pub(super) host: Arc<Host>,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("ion-terminal-control-{}", uuid::Uuid::now_v7()));
        fs::create_dir_all(root.join(".git")).unwrap();
        let host = Arc::new(Host::new(root.join("config"), root.join("state")));
        let selected = host
            .models()
            .save_default(&SavedSelection {
                provider: "fixture".into(),
                model: "fixture".into(),
                endpoint: Some("http://127.0.0.1:1".into()),
                wire: Some(Wire::ChatCompletions),
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: true,
            })
            .unwrap();
        let session =
            Arc::new(CodingSession::create(root.join("session.sqlite"), Some(&root)).unwrap());
        let setup = ion_host::CodingApplication::new(host.clone(), None)
            .setup(
                session,
                selected,
                None,
                None,
                host.resources(&root).unwrap(),
            )
            .unwrap();
        Self {
            root,
            control: setup.into_control(),
            host,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn command_actions_own_exact_editor_sources() {
    for (literal, prefix) in [
        (" \t/model missing🦀  ", " \t/model missing🦀"),
        (
            "  /command:check 'two words' 🦀  ",
            "  /command:check 'two ",
        ),
        (" /quit argument ", " /quit ar"),
    ] {
        let cursor = prefix.len();
        let mut ui = Frontend {
            draft: literal.into(),
            cursor,
            images: vec![image("attached")],
            ..Frontend::default()
        };
        let Action::Command(CommandInput::Editor(source)) =
            ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None)
        else {
            panic!("entered command must carry its original editor source");
        };
        assert_eq!(source.literal, literal);
        assert_eq!(source.cursor, cursor);
        assert!(ui.draft.is_empty());
        assert_eq!(ui.cursor, 0);
        assert_eq!(ui.images.len(), 1);
        assert_note(&ui.images[0], "attached");
        ui.restore_editor(source);
        assert_eq!(ui.draft, literal);
        assert_eq!(ui.cursor, cursor);
    }
    for literal in ["/quit", "  /exit  "] {
        let mut ui = Frontend {
            draft: literal.into(),
            cursor: literal.len(),
            ..Frontend::default()
        };
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
            Action::Quit
        ));
    }
}

#[test]
fn command_shortcuts_leave_editor_text_cursor_and_attachments_owned_by_editor() {
    for (key, builtin) in [
        ('x', Builtin::Copy),
        ('g', Builtin::Editor),
        ('l', Builtin::Model),
    ] {
        let mut ui = Frontend {
            draft: "unsent 🦀 draft".into(),
            cursor: 3,
            images: vec![image("unsent")],
            ..Frontend::default()
        };
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Char(key), Modifiers::CONTROL), None, None),
            Action::Command(CommandInput::Shortcut(command)) if command == builtin
        ));
        assert_eq!(ui.draft, "unsent 🦀 draft");
        assert_eq!(ui.cursor, 3);
        assert_eq!(ui.images.len(), 1);
        assert_note(&ui.images[0], "unsent");
    }
}

#[tokio::test]
async fn command_namespace_completes_and_never_becomes_a_model_prompt_or_queue() {
    let mut fixture = Fixture::new();
    fs::create_dir_all(fixture.host.config_root()).unwrap();
    fs::write(
        fixture.host.config_root().join("commands.json"),
        r#"{"commands":{"check":{"description":"Run checks","program":"true","args":[],"share_with_model":false}}}"#,
    )
    .unwrap();
    fixture.control.reload_resources().unwrap();
    let mut ui = Frontend::default();
    ui.insert("/command:ch");
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
            None,
            Some(fixture.control.commands())
        ),
        Action::None
    ));
    assert_eq!(ui.draft, "/command:check ");
    ui.insert("a b");
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
            None,
            Some(fixture.control.commands())
        ),
        Action::Command(CommandInput::Editor(source))
            if source.literal == "/command:check a b" && source.cursor == source.literal.len()
    ));
    assert!(!fixture.control.is_busy());

    let source = EditorDraft::at_end("/command:check a b".into());
    ui.draft = source.literal.clone();
    ui.cursor = source.cursor;
    submit_editor(&mut fixture.control, &mut ui, Delivery::FollowUp, source);
    assert_eq!(ui.draft, "/command:check a b");
    assert!(ui.status.contains("cannot be queued"));
    assert!(!fixture.control.is_busy());
    fixture.control.shutdown().await;
}

#[test]
fn command_overview_defers_argument_grammar_to_detailed_help() {
    let overview = Builtin::help(&KeyBindings::default());
    assert!(
        !overview.contains('['),
        "the overview must not dump argument grammars"
    );
    assert!(overview.contains("/help COMMAND"));
    assert!(overview.contains("/help keys"));
}

#[tokio::test]
async fn detailed_help_and_binding_orientation_are_presentation_not_input() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend {
        draft: "unsent 🦀".into(),
        cursor: 3,
        ..Frontend::default()
    };
    ui.refresh_session(fixture.control.session()).unwrap();
    assert!(
        fixture
            .control
            .session()
            .view()
            .unwrap()
            .last_model
            .is_none()
    );
    let footer = orientation::rows(&ui, Some(fixture.control.agent().model()), 100, 2);
    assert_eq!(footer[1].to_string(), "fixture/fixture · /help");
    command::handle_builtin(
        &mut fixture.control,
        &fixture.host,
        &mut ui,
        Builtin::Name,
        "Roadmap",
    )
    .unwrap();
    assert!(
        orientation::rows(&ui, Some(fixture.control.agent().model()), 100, 2)[0]
            .to_string()
            .contains("Roadmap")
    );
    for topic in ["model", "/reasoning", "keys"] {
        let expected = notice_rows(
            &Builtin::help_for(topic, &KeyBindings::default()).unwrap(),
            79,
        );
        command::handle_builtin(
            &mut fixture.control,
            &fixture.host,
            &mut ui,
            Builtin::Help,
            topic,
        )
        .unwrap();
        assert!(
            ui.pending_history_rows(80)
                .windows(expected.len())
                .any(|rows| rows == expected)
        );
        ui.finish_history_commit();
        assert_eq!((ui.draft.as_str(), ui.cursor), ("unsent 🦀", 3));
        assert!(!fixture.control.is_busy());
        assert!(fixture.control.session().view().unwrap().entries.is_empty());
    }
    assert!(
        Builtin::help_for("model", &KeyBindings::default())
            .unwrap()
            .contains("/model [MODEL]")
    );
    assert!(
        command::handle_builtin(
            &mut fixture.control,
            &fixture.host,
            &mut ui,
            Builtin::Help,
            "missing"
        )
        .is_err()
    );
    command::handle_builtin(
        &mut fixture.control,
        &fixture.host,
        &mut ui,
        Builtin::New,
        "",
    )
    .unwrap();
    assert!(!ui.session_label.contains("Roadmap"));
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn help_output_is_complete_publication_not_clipped_composer_chrome() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    ui.refresh_session(fixture.control.session()).unwrap();
    let expected = notice_rows(&Builtin::help(&KeyBindings::default()), 39);
    assert!(expected.len() > LIVE_REGION_MAX_ROWS);
    for _ in 0..2 {
        command::handle_builtin(
            &mut fixture.control,
            &fixture.host,
            &mut ui,
            Builtin::Help,
            "",
        )
        .unwrap();
        let pending = ui.pending_history_rows(40);
        assert!(
            pending.windows(expected.len()).any(|rows| rows == expected),
            "every help row must reach native publication, even for repeated explicit requests"
        );
        // Only successful physical publication acknowledges these rows.
        assert_eq!(ui.pending_history_rows(40), pending);
        ui.finish_history_commit();
        assert!(ui.pending_history_rows(40).is_empty());
    }
    fixture.control.shutdown().await;
}

async fn drain(
    control: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    activity: &mut Activity,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let update = control.next().await;
            let idle = matches!(update, ControlUpdate::Idle);
            assert!(activity.update(ui, control, update).unwrap().is_none());
            if idle {
                break;
            }
        }
    })
    .await
    .unwrap();
}

fn image(note: &str) -> ImageSource {
    let mut image = ion_ai::normalize_rgba(1, 1, vec![255, 0, 0, 255]).unwrap();
    image.note = Some(note.into());
    image.into()
}
fn assert_note(image: &ImageSource, expected: &str) {
    assert!(matches!(image, ImageSource::Normalized { note: Some(note), .. } if note == expected));
}

#[test]
fn returned_drafts_never_merge_identical_text_or_attachment_associations() {
    let file = |name: &str| ion_host::prompt_files::TextFile {
        name: name.into(),
        text: format!("context for {name}"),
    };
    let mut ui = Frontend {
        draft: "unsent".into(),
        cursor: 2,
        images: vec![image("unsent")],
        files: vec![file("unsent")],
        ..Frontend::default()
    };
    ui.return_draft(
        EditorDraft {
            literal: "  same\n".into(),
            cursor: 1,
        },
        vec![image("first")],
        vec![file("first")],
    );
    ui.return_draft(
        EditorDraft {
            literal: "  same\n".into(),
            cursor: 4,
        },
        vec![image("second")],
        vec![file("second")],
    );
    assert_eq!(
        (ui.draft.as_str(), ui.cursor, ui.recovered.len()),
        ("unsent", 2, 2)
    );
    for (text, cursor, note) in [
        ("  same\n", 1, "first"),
        ("  same\n", 4, "second"),
        ("unsent", 2, "unsent"),
    ] {
        ui.recover_draft();
        assert_eq!((ui.draft.as_str(), ui.cursor), (text, cursor));
        assert_eq!(ui.images.len(), 1);
        assert_note(&ui.images[0], note);
        assert_eq!(ui.files.len(), 1);
        assert_eq!(ui.files[0].name, note);
        assert_eq!(ui.files[0].text, file(note).text);
    }
    assert_eq!(ui.recovered.len(), 2);
}

#[test]
fn oversized_history_preserves_the_draft_and_can_be_traversed_without_loading_it() {
    let mut ui = Frontend {
        draft: "unsent 🦀".into(),
        cursor: 3,
        prompt_history: vec!["older".into(), "x".repeat(MAX_DRAFT + 1)],
        ..Frontend::default()
    };
    ui.history_previous();
    assert_eq!((ui.draft.as_str(), ui.cursor), ("unsent 🦀", 3));
    assert!(ui.status.contains("exceeds editor limit"));
    ui.history_previous();
    assert_eq!(ui.draft, "older");
    ui.history_next();
    assert_eq!(ui.draft, "older");
    ui.history_next();
    assert_eq!((ui.draft.as_str(), ui.cursor), ("unsent 🦀", 3));
}

#[test]
fn returned_draft_ends_history_browsing_without_losing_unsent_input() {
    let mut ui = Frontend {
        prompt_history: vec!["older".into()],
        draft: "unsent".into(),
        cursor: 2,
        ..Frontend::default()
    };
    ui.history_previous();
    ui.return_draft(EditorDraft::at_end("returned".into()), vec![], Vec::new());
    ui.history_next();
    assert_eq!((ui.draft.as_str(), ui.cursor), ("unsent", 2));
    ui.recover_draft();
    ui.history_next();
    assert_eq!(ui.draft, "returned");
}

#[test]
fn submit_and_queue_handoff_discard_unsent_text_history() {
    for modifiers in [Modifiers::NONE, Modifiers::ALT] {
        let mut ui = Frontend::default();
        ui.insert("sent input");
        let action = ui.key(KeyEvent::new(KeyCode::Enter, modifiers), None, None);
        assert!(match modifiers {
            Modifiers::NONE => matches!(action, Action::Submit(prompt) if prompt == "sent input"),
            _ => matches!(action, Action::Queue(prompt) if prompt == "sent input"),
        });
        assert!(ui.draft.is_empty());
        ui.key(
            KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL),
            None,
            None,
        );
        assert!(
            ui.draft.is_empty(),
            "handoff history resurrected sent input"
        );
        ui.insert("next input");
        ui.key(
            KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL),
            None,
            None,
        );
        assert_eq!(ui.draft, "", "undo crossed a completed handoff");
    }
}

#[test]
fn mouse_scroll_breaks_edit_affinity_without_resetting_history_or_text() {
    let mut ui = Frontend::default();
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('a'), Modifiers::NONE),
            None,
            None
        ),
        Action::None
    ));
    ui.mouse(MouseKind::ScrollUp);
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('b'), Modifiers::NONE),
            None,
            None
        ),
        Action::None
    ));
    assert_eq!(ui.draft, "ab");
    assert_eq!(ui.scroll, 3);
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL),
            None,
            None
        ),
        Action::None
    ));
    assert_eq!(ui.draft, "a", "scroll must break typed-word coalescing");
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL),
            None,
            None
        ),
        Action::None
    ));
    assert_eq!(ui.draft, "", "scroll must not reset undo history");

    for text in ["first", "second"] {
        ui.insert(text);
        ui.cursor = ui.draft.len();
        assert!(matches!(
            ui.key(
                KeyEvent::new(KeyCode::Char('w'), Modifiers::CONTROL),
                None,
                None
            ),
            Action::None
        ));
    }
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('y'), Modifiers::CONTROL),
            None,
            None
        ),
        Action::None
    ));
    assert_eq!(ui.draft, "second");
    ui.mouse(MouseKind::ScrollDown);
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('y'), Modifiers::ALT),
            None,
            None
        ),
        Action::None
    ));
    assert_eq!(
        ui.draft, "second",
        "scroll must invalidate direct yank-pop affinity"
    );
}

#[tokio::test]
async fn session_sync_preserves_text_recovery_but_session_replacement_resets_it() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend::default();
    ui.refresh_session(fixture.control.session()).unwrap();
    ui.insert("same session");
    ui.refresh_session(fixture.control.session()).unwrap();
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL),
            None,
            None
        ),
        Action::None
    ));
    assert_eq!(ui.draft, "", "same-Session sync discarded unsent edits");

    ui.insert("kept across replacement");
    fixture.control.new_session().unwrap();
    ui.refresh_session(fixture.control.session()).unwrap();
    assert_eq!(ui.draft, "kept across replacement");
    ui.key(
        KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL),
        None,
        None,
    );
    assert_eq!(
        ui.draft, "kept across replacement",
        "Session replacement kept the old edit history"
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn host_rejection_restores_exact_editor_and_images() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend {
        images: vec![image("coordinates")],
        ..Frontend::default()
    };
    let literal = "  !!shell stays literal\n";
    // No coding operation is targeted by this direct compaction ID.
    let operation = fixture
        .control
        .compact(EditorDraft::at_end("compact".into()))
        .unwrap();
    submit_editor(
        &mut fixture.control,
        &mut ui,
        Delivery::Steer(operation),
        EditorDraft {
            literal: literal.into(),
            cursor: 3,
        },
    );
    assert_eq!((ui.draft.as_str(), ui.cursor), (literal, 3));
    assert_note(&ui.images[0], "coordinates");
    assert!(ui.recovered.is_empty());
    assert!(ui.status.contains("not queued"));
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn rejected_launch_batch_keeps_empty_attached_first_and_ordered_literal_tail() {
    let mut fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".ion/prompts")).unwrap();
    fs::write(fixture.root.join(".ion/prompts/check.md"), "Expanded $1").unwrap();
    fixture.control.reload_resources().unwrap();
    let before = fixture.control.session().view().unwrap().entries.len();
    let mut ui = Frontend {
        images: vec![image("first image")],
        files: vec![ion_host::prompt_files::TextFile {
            name: "frozen.md".into(),
            text: "/check literal 🦀".into(),
        }],
        ..Frontend::default()
    };
    let malformed = "/check '🦀";
    interaction::submit_initial_prompts(
        &mut fixture.control,
        &mut ui,
        vec![String::new(), malformed.into(), "/help".into()],
    );
    assert_eq!((ui.draft.as_str(), ui.cursor), ("", 0));
    assert_note(&ui.images[0], "first image");
    assert_eq!(ui.files[0].text, "/check literal 🦀");
    assert_eq!(
        ui.recovered
            .iter()
            .map(|draft| draft.source.literal.as_str())
            .collect::<Vec<_>>(),
        [malformed, "/help"]
    );
    assert_eq!(ui.recovered[0].source.cursor, malformed.len());
    assert!(
        ui.recovered
            .iter()
            .all(|draft| draft.images.is_empty() && draft.files.is_empty())
    );
    assert!(ui.status.contains("not queued"));
    assert!(!fixture.control.is_busy());
    assert!(matches!(fixture.control.next().await, ControlUpdate::Idle));
    assert_eq!(
        fixture.control.session().view().unwrap().entries.len(),
        before
    );
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn asynchronous_preparation_return_retains_raw_source_and_other_edits() {
    let mut fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".ion/prompts")).unwrap();
    fs::write(fixture.root.join(".ion/prompts/check.md"), "Expanded $1").unwrap();
    fixture.control.reload_resources().unwrap();
    let literal = "  /check café 🦀\n";
    let mut ui = Frontend {
        images: vec![ImageSource::Path(fixture.root.join("missing.png"))],
        ..Frontend::default()
    };
    submit_editor(
        &mut fixture.control,
        &mut ui,
        Delivery::Start,
        EditorDraft {
            literal: literal.into(),
            cursor: 2,
        },
    );
    assert!(ui.images.is_empty());
    ui.insert("typed while preparing");
    let mut activity = Activity::default();
    drain(&mut fixture.control, &mut ui, &mut activity).await;
    assert_eq!(ui.draft, "typed while preparing");
    ui.recover_draft();
    assert_eq!((ui.draft.as_str(), ui.cursor), (literal, 2));
    assert!(matches!(&ui.images[0], ImageSource::Path(path) if path.ends_with("missing.png")));
    // A path may cross a display-row boundary (notably with /tmp on Linux).
    let notice = ui
        .pending_history_rows(120)
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    assert!(
        notice.contains("preparation failed") && notice.contains("missing.png"),
        "{notice}"
    );
    // Recovery is passive: an empty Host stays empty until explicit resubmission.
    assert!(matches!(fixture.control.next().await, ControlUpdate::Idle));
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn typed_at_signs_remain_literal_without_accepting_suggestions() {
    let root = std::env::temp_dir().join(format!("ion-literal-files-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&root).unwrap();
    for literal in [
        "nick@example.com",
        "user@host:repo",
        "Read @src/main.rs next",
        "@",
    ] {
        let mut ui = Frontend {
            cwd: root.clone(),
            ..Frontend::default()
        };
        for ch in literal.chars() {
            ui.key(
                KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE),
                None,
                None,
            );
        }
        assert_eq!((ui.draft.as_str(), ui.cursor), (literal, literal.len()));
        assert!(ui.pickers.active().is_none());
        // Suggestions may discover asynchronously; no unseen candidate can
        // consume this Enter or change the literal prompt.
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
            Action::Submit(text) if text == literal
        ));
        ui.pickers.shutdown().await.unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn ordinary_paths_complete_in_place_without_submitting() {
    let root = std::env::temp_dir().join(format!("ion-paths-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(root.join("space dir")).unwrap();
    fs::write(root.join("main.rs"), "owned fixture").unwrap();
    fs::write(root.join("space dir/main.rs"), "owned fixture").unwrap();
    for (mut literal, expected, force) in [
        ("Read m".to_owned(), "Read main.rs ".to_owned(), true),
        ("Read ./ma".to_owned(), "Read ./main.rs ".to_owned(), false),
        (
            format!("Read {}/ma", root.display()),
            format!("Read {}/main.rs ", root.display()),
            false,
        ),
        (
            format!("@{}/ma", root.display()),
            format!("@{}/main.rs ", root.display()),
            false,
        ),
        (
            format!("\"{}/ma", root.display()),
            format!("\"{}/main.rs\" ", root.display()),
            false,
        ),
        (
            "Read \"space dir/ma".to_owned(),
            "Read \"space dir/main.rs\" ".to_owned(),
            false,
        ),
    ] {
        let mut ui = Frontend {
            cwd: root.clone(),
            ..Frontend::default()
        };
        ui.insert(&literal);
        ui.refresh_completion(None, None, false);
        if force {
            assert!(
                !ui.completion.active(),
                "ordinary prose must not open a file menu"
            );
            ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None, None);
            ui.key(
                KeyEvent::new(KeyCode::Char('a'), Modifiers::NONE),
                None,
                None,
            );
            literal.push('a');
        }
        assert!(ui.completion.wants_files(), "{literal}");
        assert_eq!(ui.draft, literal);
        ui.pickers.complete().await;
        ui.refresh_completion(None, None, false);
        assert!(
            !ui.completion.rows(200, 6, &ui.keys).is_empty(),
            "{literal}"
        );
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
            Action::None
        ));
        assert_eq!(ui.draft, expected);
        ui.key(
            KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL),
            None,
            None,
        );
        assert_eq!(ui.draft, literal, "acceptance must be one undoable edit");
        ui.pickers.shutdown().await.unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn withdrawn_preparation_replaces_draft_and_invalidates_file_completion() {
    for literal in ["é queued input", "ASCII queued input"] {
        let mut fixture = Fixture::new();
        fs::write(fixture.root.join("file.rs"), "owned fixture").unwrap();
        let mut ui = Frontend {
            cwd: fixture.root.clone(),
            images: vec![ImageSource::Path(fixture.root.join("absent.png"))],
            ..Frontend::default()
        };
        submit_editor(
            &mut fixture.control,
            &mut ui,
            Delivery::FollowUp,
            EditorDraft::at_end(literal.into()),
        );
        assert!(fixture.control.state().preparing_input);
        assert!(fixture.control.withdraw_last_follow_up().is_some());
        ui.insert("x @");
        ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None, None);
        assert!(ui.completion.active());
        assert!(ui.pickers.active().is_none());

        let mut activity = Activity::default();
        drain(&mut fixture.control, &mut ui, &mut activity).await;
        assert_eq!((ui.draft.as_str(), ui.cursor), (literal, literal.len()));
        assert!(
            matches!(&ui.images[..], [ImageSource::Path(path)] if path.ends_with("absent.png"))
        );
        assert!(
            !ui.completion.active(),
            "returned input must invalidate the old draft's completion range"
        );
        while ui.pickers.has_job() {
            ui.pickers.complete().await;
        }
        assert!(
            !ui.completion.active(),
            "late discovery must not reopen the old completion"
        );

        ui.recover_draft();
        assert_eq!((ui.draft.as_str(), ui.cursor), ("x @", 3));
        assert!(ui.images.is_empty());
        assert!(
            matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None), Action::Submit(text) if text == "x @")
        );
        fixture.control.shutdown().await;
        ui.pickers.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn settled_shell_is_reconciled_and_shutdown_errors_remain_visible() {
    let mut fixture = Fixture::new();
    fixture
        .control
        .shell(
            "printf terminal-control".into(),
            false,
            EditorDraft::at_end("  !printf terminal-control  ".into()),
        )
        .unwrap();
    let mut ui = Frontend::default();
    let mut activity = Activity::default();
    drain(&mut fixture.control, &mut ui, &mut activity).await;
    assert!(
        matches!(&ui.history.items[..], [TranscriptItem::UserShell(shell)] if shell.command == "printf terminal-control")
    );
    fixture
        .control
        .compact(EditorDraft::at_end("compact".into()))
        .unwrap();
    let report = fixture.control.shutdown().await;
    let mut outcome = Err(anyhow::anyhow!("terminal output disconnected"));
    for mut update in report.updates {
        if let ControlUpdate::Settled(settlement) = &mut update {
            // Inject a concurrent settlement failure at the actual UI boundary.
            settlement.outcome = OperationOutcome::Compact(Err(
                ion_core::CodingAgentError::InvalidSummary("observation commit failed"),
            ));
        }
        activity.cleanup_update(&mut ui, &fixture.control, update, &mut outcome);
    }
    let error = format!("{:#}", outcome.unwrap_err());
    assert!(error.contains("terminal output disconnected"));
    assert!(error.contains("observation commit failed"));
    assert!(ui.recovered.is_empty());
    assert!(
        ui.draft.is_empty(),
        "a settled command must not revive in the composer"
    );
}

#[tokio::test]
async fn unfinished_history_anchors_are_stable_on_initial_load_and_reinspection() {
    let mut fixture = Fixture::new();
    let started = |turn, text: &str| SessionEntry::TurnStarted {
        completion: Vec::new(),
        turn,
        input: Message::user_input(text.into(), []),
        model: ModelRef {
            provider: "fixture".into(),
            model: "fixture".into(),
        },
        request: None,
        recovery: None,
    };
    for shell in [false, true] {
        let mut view = fixture.control.session().view().unwrap();
        view.entries = vec![
            started(1, "closed"),
            SessionEntry::TurnEnded {
                turn: 1,
                reason: TurnEndReason::Cancelled,
            },
        ];
        if shell {
            view.unfinished_user_shell = Some(ion_core::UnobservedUserShell { admission_entry: 3 });
            view.entries.push(SessionEntry::UserShellAdmitted {
                command: "UNFINISHED_VISIBLE".into(),
                exclude_from_context: false,
            });
        } else {
            view.unfinished_turn = Some(2);
            view.entries.push(started(2, "UNFINISHED_VISIBLE"));
        }
        view.context_path = (1..=view.entries.len() as u64).collect();
        let mut ui = Frontend::default();
        // Initial refresh and later reconciliation must use the same stable
        // prefix, even before anything has been physically published.
        for _ in 0..2 {
            ui.load_history(fixture.control.session(), &view);
            assert_eq!(ui.history.items.len(), 2);
            assert_eq!(ui.pending_history_target, 1);
            ui.print(Builtin::help(&KeyBindings::default()));
        }
        let pending = ui.pending_history_rows(80);
        assert!(
            !pending
                .iter()
                .any(|row| row.to_string().contains("UNFINISHED_VISIBLE"))
        );
        let fullscreen = ui.output.rows(
            &ui.history.items,
            0..2,
            false,
            80,
            &crate::tool_output::OutputOptions::new(ui.output_detail, &ui.keys),
            ui.thinking_visibility,
        );
        assert!(
            fullscreen
                .iter()
                .any(|row| row.to_string().contains("UNFINISHED_VISIBLE"))
        );
        ui.finish_history_commit();
        assert!(ui.pending_history_rows(80).is_empty());
        // Do not rewind an unknown-effect row already in physical history.
        ui.history_published_items = 2;
        ui.load_history(fixture.control.session(), &view);
        assert_eq!(ui.pending_history_target, 2);
    }
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn clipboard_image_keeps_typed_coordinates_and_refuses_unsupported_routes() {
    let mut fixture = Fixture::new();
    let mut loaded = ion_ai::normalize_rgba(1, 1, vec![255, 0, 0, 255]).unwrap();
    loaded.note = Some("clipboard coordinates".into());
    let prepared = prepare_clipboard(
        fixture.control.agent().model(),
        fixture.control.agent().limits().image_input,
        crate::clipboard_reader::PasteContent::Image {
            content: loaded.content.clone(),
            note: loaded.note.clone(),
        },
    )
    .unwrap();
    let mut ui = Frontend::default();
    apply_clipboard(&mut ui, prepared).unwrap();
    assert!(
        matches!(&ui.images[..], [ImageSource::Normalized { content, note: Some(note) }] if content == &loaded.content && note == "clipboard coordinates")
    );
    assert!(
        prepare_clipboard(
            fixture.control.agent().model(),
            false,
            crate::clipboard_reader::PasteContent::Image {
                content: loaded.content,
                note: loaded.note,
            }
        )
        .is_err()
    );
    assert_eq!(ui.images.len(), 1);
    fixture.control.shutdown().await;
}

#[tokio::test]
async fn context_selection_restarts_publication_without_losing_an_unsent_draft() {
    let mut fixture = Fixture::new();
    let model = fixture.control.agent().model().clone();
    let mut selected_image = ion_ai::normalize_rgba(1, 1, vec![0, 255, 0, 255]).unwrap();
    selected_image.note = Some("selected tree image".into());
    let input = Message::user_input("!literal historical input".into(), [selected_image]);
    let mut view = fixture.control.session().view().unwrap();
    view.entries = vec![
        SessionEntry::TurnStarted {
            completion: Vec::new(),
            turn: 1,
            input,
            model: model.clone(),
            request: None,
            recovery: None,
        },
        SessionEntry::Assistant {
            turn: 1,
            message: Message {
                role: ion_ai::Role::Assistant,
                content: vec![Content::Text("ORIGINAL_TREE_ANSWER".into())],
                provider_replay: None,
            },
            tool_activities: Vec::new(),
            execution: ion_ai::ModelExecution {
                route: ion_ai::ModelRoute::direct(model, ion_ai::ModelRouteReason::UserRequest),
                returned_model: None,
            },
            usage: ion_ai::Usage::unknown(),
            termination: ion_ai::ResponseTermination::Completed,
        },
        SessionEntry::TurnEnded {
            turn: 1,
            reason: TurnEndReason::Completed,
        },
    ];
    view.context_path = vec![1, 2, 3];
    let mut ui = Frontend::default();
    ui.load_history(fixture.control.session(), &view);
    ui.finish_history_commit();
    assert_eq!(ui.history_published_items, 2);
    ui.draft = "unsent draft".into();
    ui.cursor = 4;
    ui.images = vec![image("unsent tree image")];
    view.entries
        .push(SessionEntry::ContextSelected { head_entry: 0 });
    view.context_path.clear();
    ui.load_history(fixture.control.session(), &view);
    assert_eq!(ui.history_published_items, 0);
    assert!(ui.history.items.is_empty());
    assert!(
        ui.pending_history_banner
            .as_ref()
            .unwrap()
            .contains("files unchanged")
    );
    let _rows = ui.pending_history_rows(80);
    assert_eq!((ui.draft.as_str(), ui.cursor), ("unsent draft", 4));
    assert_note(&ui.images[0], "unsent tree image");
    assert!(
        last_committed_answer(&view).is_err(),
        "copy cannot leak an unselected answer"
    );
    view.entries
        .push(SessionEntry::ContextSelected { head_entry: 2 });
    view.context_path = vec![1, 2];
    ui.load_history(fixture.control.session(), &view);
    assert_eq!(
        ui.draft, "unsent draft",
        "assistant selection must not overwrite an unsent draft"
    );
    assert_eq!(ui.history.items.len(), 2);
    assert_eq!(
        last_committed_answer(&view).unwrap(),
        "ORIGINAL_TREE_ANSWER"
    );
    fixture.control.shutdown().await;
}

#[test]
fn fork_selection_preserves_unsent_literal_input_and_typed_images() {
    let mut ui = Frontend {
        draft: "  unsent\n".into(),
        cursor: 3,
        images: vec![image("unsent")],
        ..Frontend::default()
    };
    let mut selected = ion_ai::normalize_rgba(1, 1, vec![0, 255, 0, 255]).unwrap();
    selected.note = Some("selected".into());
    ui.restore_input(Message::user_input("selected input".into(), [selected]))
        .unwrap();
    assert_eq!(ui.draft, "selected input");
    assert_note(&ui.images[0], "selected");
    ui.recover_draft();
    assert_eq!((ui.draft.as_str(), ui.cursor), ("  unsent\n", 3));
    assert_note(&ui.images[0], "unsent");
}

#[test]
fn operation_status_keeps_controls_and_stop_intent_visible() {
    let ui = Frontend {
        status: "Details closed".into(),
        ..Frontend::default()
    };
    for (operation, label) in [
        (OperationKind::Coding, "Working"),
        (OperationKind::Shell, "Running shell"),
        (OperationKind::Compaction, "Summarizing context"),
    ] {
        let status = visible_status(
            &ui,
            Some(ActiveOperation {
                kind: operation,
                stop_requested: false,
                pending_follow_ups: 0,
            }),
        )
        .unwrap();
        assert!(status.starts_with(label));
        assert!(status.contains("Ctrl-C cancels") && status.contains("Details closed"));
    }
    let status = visible_status(
        &ui,
        Some(ActiveOperation {
            kind: OperationKind::Coding,
            stop_requested: true,
            pending_follow_ups: 0,
        }),
    )
    .unwrap();
    assert!(status.starts_with("Cancelling turn…") && !status.contains("Ctrl-C cancels"));
}

#[test]
fn shell_syntax_and_output_settings_remain_editor_operations() {
    for (literal, excluded) in [("  !echo ok  ", false), ("  !!echo 🦀  ", true)] {
        let mut ui = Frontend {
            draft: literal.into(),
            cursor: 1,
            ..Frontend::default()
        };
        assert!(
            matches!(ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None), Action::Shell { exclude_from_context, source, .. } if exclude_from_context == excluded && source.literal == literal && source.cursor == 1)
        );
    }
    let mut ui = Frontend::default();
    ui.output_settings("expanded", ion_core::CompletionDelivery::Inbox)
        .unwrap();
    ui.output_settings("thinking show", ion_core::CompletionDelivery::Inbox)
        .unwrap();
    assert_eq!(ui.output_detail, crate::tool_output::OutputDetail::Expanded);
    assert_eq!(
        ui.thinking_visibility,
        crate::transcript_render::ThinkingVisibility::Visible
    );
    assert!(
        ui.output_settings("thinking sometimes", ion_core::CompletionDelivery::Inbox)
            .is_err()
    );
}

#[tokio::test]
async fn configuration_selectors_preserve_unsent_input_without_admission() {
    let mut fixture = Fixture::new();
    let mut ui = Frontend {
        draft: "unsent 🦀".into(),
        cursor: 3,
        images: vec![image("attached")],
        ..Frontend::default()
    };
    for builtin in [
        Builtin::Settings,
        Builtin::Reasoning,
        Builtin::Login,
        Builtin::Model,
    ] {
        command::handle_builtin(&mut fixture.control, &fixture.host, &mut ui, builtin, "").unwrap();
        assert!(
            ui.pickers.active().is_some(),
            "{builtin:?} must offer choices"
        );
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Esc, Modifiers::NONE), None, None),
            Action::None
        ));
        assert!(ui.pickers.active().is_none());
        assert_eq!((ui.draft.as_str(), ui.cursor), ("unsent 🦀", 3));
        assert_note(&ui.images[0], "attached");
        assert!(fixture.control.session().view().unwrap().entries.is_empty());
        assert!(!fixture.control.is_busy());
    }
    for policy in ion_core::CompletionDelivery::ALL {
        command::handle_builtin(
            &mut fixture.control,
            &fixture.host,
            &mut ui,
            Builtin::Settings,
            &format!("completion {policy}"),
        )
        .unwrap();
        assert_eq!(fixture.control.completion_delivery(), policy);
        command::handle_builtin(
            &mut fixture.control,
            &fixture.host,
            &mut ui,
            Builtin::Settings,
            "",
        )
        .unwrap();
        assert!(
            ui.pickers
                .active()
                .unwrap()
                .items
                .iter()
                .any(
                    |item| matches!(item.value, PickerValue::CompletionDelivery(_))
                        && item.label.ends_with(policy.as_str())
                )
        );
        assert_eq!((ui.draft.as_str(), ui.cursor), ("unsent 🦀", 3));
        assert_note(&ui.images[0], "attached");
        assert!(fixture.control.session().view().unwrap().entries.is_empty());
    }
    fixture.control.shutdown().await;
}

#[test]
fn settings_cycle_only_rendered_choices_and_keep_search_and_draft() {
    let mut ui = Frontend {
        draft: "unsent 🦀".into(),
        cursor: 3,
        thinking_visibility: crate::transcript_render::ThinkingVisibility::Visible,
        ..Frontend::default()
    };
    ui.output_settings("", ion_core::CompletionDelivery::Inbox)
        .unwrap();
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
        Action::None
    ));
    for code in [KeyCode::Char(' '), KeyCode::Enter] {
        modal_rows(&mut ui, None, None, 100, 20);
        let Action::Pick(PickerValue::Setting(setting)) =
            ui.key(KeyEvent::new(code, Modifiers::NONE), None, None)
        else {
            panic!("rendered settings can cycle");
        };
        ui.apply_setting(setting);
        ui.open_settings(ion_core::CompletionDelivery::Inbox);
        assert!(ui.pickers.active().is_some());
        assert!(
            matches!(
                ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
                Action::None
            ),
            "updated choices require another render"
        );
    }
    assert_eq!(ui.output_detail, crate::tool_output::OutputDetail::Compact);
    ui.pickers.insert("Thinking");
    modal_rows(&mut ui, None, None, 100, 20);
    let Action::Pick(PickerValue::Setting(setting)) =
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None)
    else {
        panic!("filtered setting is selectable");
    };
    ui.apply_setting(setting);
    ui.open_settings(ion_core::CompletionDelivery::Inbox);
    assert_eq!(ui.pickers.active().unwrap().query, "Thinking");
    assert_eq!(
        ui.thinking_visibility,
        crate::transcript_render::ThinkingVisibility::Hidden
    );
    ui.pickers.close();
    ui.open_settings(ion_core::CompletionDelivery::Inbox);
    ui.pickers.insert("Managed-work");
    modal_rows(&mut ui, None, None, 100, 20);
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
        Action::Pick(PickerValue::CompletionDelivery(
            ion_core::CompletionDelivery::Continue
        ))
    ));
    assert_eq!(ui.pickers.active().unwrap().query, "Managed-work");
    ui.open_settings(ion_core::CompletionDelivery::Continue);
    assert!(
        matches!(
            ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
            Action::None
        ),
        "a changed Host policy choice requires another paint"
    );
    assert_eq!((ui.draft.as_str(), ui.cursor), ("unsent 🦀", 3));
}

#[tokio::test]
async fn reasoning_choices_use_adapter_support_and_preselect_current() {
    let mut fixture = Fixture::new();
    for (wire, expected) in [
        (
            Wire::ChatCompletions,
            vec![
                ion_ai::Reasoning::ProviderDefault,
                ion_ai::Reasoning::Off,
                ion_ai::Reasoning::Low,
                ion_ai::Reasoning::Medium,
                ion_ai::Reasoning::High,
            ],
        ),
        (
            Wire::LlamaCppNoThinking,
            vec![ion_ai::Reasoning::ProviderDefault, ion_ai::Reasoning::Off],
        ),
        (
            Wire::AnthropicMessages,
            vec![ion_ai::Reasoning::ProviderDefault],
        ),
    ] {
        fixture
            .control
            .select_reasoning(ion_ai::Reasoning::ProviderDefault)
            .unwrap();
        let selected = fixture
            .host
            .models()
            .save_default(&SavedSelection {
                provider: "effort".into(),
                model: "model".into(),
                endpoint: Some("http://127.0.0.1:1".into()),
                wire: Some(wire),
                api_key_env: None,
                context_window_tokens: None,
                max_output_tokens: None,
                image_input: false,
            })
            .unwrap();
        fixture.control.select_model(selected.identity()).unwrap();
        let current = *expected.last().unwrap();
        fixture.control.select_reasoning(current).unwrap();
        let before = fixture.control.session().view().unwrap().entries.len();
        let picker = Picker::reasoning(&fixture.control).unwrap();
        let actual: Vec<_> = picker
            .items
            .iter()
            .map(|item| match item.value {
                PickerValue::Reasoning(value) => value,
                _ => panic!("only reasoning choices"),
            })
            .collect();
        assert_eq!(actual, expected);
        assert!(
            matches!(picker.items[picker.selected].value, PickerValue::Reasoning(value) if value == current)
        );
        assert!(picker.items[picker.selected].label.contains("current"));
        assert_eq!(
            fixture.control.session().view().unwrap().entries.len(),
            before
        );
        assert_eq!(fixture.control.session().reasoning().unwrap(), current);
    }
    let picker = Picker::models(&fixture.host, Some(fixture.control.agent().model())).unwrap();
    assert!(
        matches!(&picker.items[picker.selected].value, PickerValue::Model(value) if value == fixture.control.agent().model())
    );
    fixture.control.shutdown().await;
}

#[test]
fn question_block_status_requires_the_real_modal_and_clears() {
    let mut program = ProgramStatusState::default();
    program.settle(ProgramStatus::Done);
    assert_eq!(program.status(false), ProgramStatus::Done);
    program.blocked_question = true;
    assert_eq!(program.status(true), ProgramStatus::BlockedQuestion);
    program.blocked_auth = true;
    assert_eq!(program.status(true), ProgramStatus::BlockedAuth);
    program.blocked_auth = false;
    program.blocked_question = false;
    assert_eq!(program.status(false), ProgramStatus::Done);
}

#[test]
fn question_modal_preserves_composer_draft_images_and_defers_to_other_views() {
    let request = ion_host::QuestionRequestId(uuid::Uuid::now_v7());
    let questions = vec![ion_host::Question {
        question: "Continue?".into(),
        multi_select: false,
        options: vec![
            ion_host::QuestionOption {
                label: "Yes".into(),
                description: None,
            },
            ion_host::QuestionOption {
                label: "No".into(),
                description: None,
            },
        ],
    }];
    let mut ui = Frontend {
        draft: "literal draft".into(),
        cursor: "literal draft".len(),
        images: vec![ImageSource::Path(PathBuf::from("missing.png"))],
        question: Some(QuestionModal::new(request, questions)),
        ..Frontend::default()
    };
    assert!(ui.question_owns_input());
    let question = ui.question.as_mut().unwrap();
    question.key(
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &KeyBindings::default(),
    );
    question.key(
        KeyEvent::new(KeyCode::Enter, Modifiers::NONE),
        &KeyBindings::default(),
    );
    assert_eq!(ui.draft, "literal draft");
    assert_eq!(ui.cursor, "literal draft".len());
    assert_eq!(ui.images.len(), 1);
    ui.pickers.open(Picker {
        title: "Existing view",
        query: String::new(),
        selected: 0,
        items: Vec::new(),
    });
    assert!(!ui.question_owns_input());
}
