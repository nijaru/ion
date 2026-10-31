//! Rebinding must change real consumers, not just registry/help output.
use super::*;

#[test]
fn wrapped_editor_navigation_keeps_history_and_sticky_column_at_visual_boundaries() {
    let mut ui = Frontend {
        editor: Editor {
            draft: "one two three".into(),
            cursor: 4,
            prompt_history: vec!["older input".into()],
            ..Editor::default()
        },
        ..Frontend::default()
    };
    // No physical editor geometry means no guessed row or history transition.
    ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None, None);
    assert_eq!(ui.editor.cursor, 4);
    ui.editor.publish_editor_width(10);
    ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None, None);
    assert_eq!(ui.editor.cursor, 12);
    assert_eq!(ui.editor.draft, "one two three");
    ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None, None);
    assert_eq!(ui.editor.cursor, 4);
    ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None, None);
    assert_eq!(ui.editor.draft, "older input");
    ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None, None);
    assert_eq!(ui.editor.draft, "one two three");
    assert_eq!(ui.editor.cursor, 4);

    ui.restore_editor(EditorDraft {
        literal: "abcdef\nx\nabcdef".into(),
        cursor: 5,
    });
    ui.editor.publish_editor_width(20);
    for expected in ["abcdef\nx".len(), "abcdef\nx\nabcde".len()] {
        ui.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), None, None);
        assert_eq!(ui.editor.cursor, expected);
    }
    ui.editor.publish_editor_width(19);
    assert_eq!(ui.editor.preferred_column, None);
    ui.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), None, None);
    ui.key(KeyEvent::new(KeyCode::Left, Modifiers::NONE), None, None);
    assert_eq!(ui.editor.preferred_column, None);
}

#[test]
fn model_cycle_keys_and_rebinding_retain_the_editor_and_attachments() {
    use ion_host::ModelCycleDirection::{Backward, Forward};

    for (keys, events) in [
        (
            KeyBindings::default(),
            vec![
                (
                    KeyEvent::new(KeyCode::Char('p'), Modifiers::CONTROL),
                    Some(Forward),
                ),
                (
                    KeyEvent::new(KeyCode::Char('p'), Modifiers::CONTROL | Modifiers::SHIFT),
                    Some(Backward),
                ),
            ],
        ),
        (
            KeyBindings::parse(
                br#"{"app.model.cycleForward":"alt+m","app.model.cycleBackward":[]}"#,
            )
            .unwrap(),
            vec![
                (KeyEvent::new(KeyCode::Char('p'), Modifiers::CONTROL), None),
                (
                    KeyEvent::new(KeyCode::Char('p'), Modifiers::CONTROL | Modifiers::SHIFT),
                    None,
                ),
                (
                    KeyEvent::new(KeyCode::Char('m'), Modifiers::ALT),
                    Some(Forward),
                ),
            ],
        ),
    ] {
        let mut ui = Frontend {
            editor: Editor {
                draft: "left 🦀 right".into(),
                cursor: "left ".len(),
                images: vec![ImageSource::Path(PathBuf::from("retained.png"))],
                ..Editor::default()
            },
            keys,
            ..Frontend::default()
        };
        for (event, direction) in events {
            let action = ui.key(event, None, None);
            match direction {
                Some(expected) => assert!(matches!(
                    action,
                    Action::Command(CommandInput::CycleModel(actual)) if actual == expected
                )),
                None => assert!(matches!(action, Action::None)),
            }
            assert_eq!(ui.editor.draft, "left 🦀 right");
            assert_eq!(ui.editor.cursor, "left ".len());
            assert!(
                matches!(&ui.editor.images[..], [ImageSource::Path(path)] if path == std::path::Path::new("retained.png"))
            );
        }
    }
}

#[test]
fn completion_consumed_keys_interrupt_composer_yank_affinity() {
    let mut ui = Frontend {
        keys: KeyBindings::parse(br#"{"tui.select.cancel":"alt+y"}"#).unwrap(),
        ..Frontend::default()
    };
    for text in ["older", "/he"] {
        ui.replace_draft_atomic(text.into());
        ui.key(
            KeyEvent::new(KeyCode::Char('u'), Modifiers::CONTROL),
            None,
            None,
        );
    }
    ui.key(
        KeyEvent::new(KeyCode::Char('y'), Modifiers::CONTROL),
        None,
        None,
    );
    assert_eq!(ui.editor.draft, "/he");
    assert!(
        !ui.editor
            .completion
            .painted_rows(80, 6, &ui.keys)
            .is_empty()
    );
    ui.key(
        KeyEvent::new(KeyCode::Char('y'), Modifiers::ALT),
        None,
        None,
    );
    assert!(!ui.editor.completion.visible());
    ui.key(
        KeyEvent::new(KeyCode::Char('y'), Modifiers::ALT),
        None,
        None,
    );
    assert_eq!(
        ui.editor.draft, "/he",
        "dismissal is not a directly preceding yank"
    );
}

#[tokio::test]
async fn reload_publishes_prepared_keys_and_discards_only_obsolete_status_hints() {
    let mut fixture = super::tests::Fixture::new();
    let mut ui = Frontend {
        themes: crate::themes::Themes::load(fixture.host.config_root(), None).unwrap(),
        ..Frontend::default()
    };
    ui.replace_draft_atomic("literal 🦀 draft".into());
    ui.editor.cursor = "literal ".len();
    command::handle_builtin(
        &mut fixture.control,
        &fixture.host,
        &mut ui,
        Builtin::Tui,
        "fullscreen",
    )
    .unwrap();
    let old_status = ui.status.clone();
    let path = fixture.host.config_root().join("keybindings.json");
    fs::write(&path, br#"{"app.model.select":true}"#).unwrap();
    assert!(
        command::handle_builtin(
            &mut fixture.control,
            &fixture.host,
            &mut ui,
            Builtin::Reload,
            ""
        )
        .is_err()
    );
    assert_eq!(ui.keys.label(KeyAction::Model), "Ctrl-L");
    assert_eq!(ui.status, old_status);
    fs::write(
        &path,
        br#"{"app.model.select":"alt+m","tui.editor.pageUp":"f7"}"#,
    )
    .unwrap();
    let settings = fixture.host.config_root().join("settings.json");
    fs::write(&settings, br#"{"theme":"missing"}"#).unwrap();
    let original_theme = ui.themes.theme().clone();
    assert!(
        command::handle_builtin(
            &mut fixture.control,
            &fixture.host,
            &mut ui,
            Builtin::Reload,
            ""
        )
        .is_err()
    );
    assert_eq!(ui.keys.label(KeyAction::Model), "Ctrl-L");
    assert_eq!(ui.themes.theme(), &original_theme);
    fs::write(&settings, br#"{"theme":"light"}"#).unwrap();
    command::handle_builtin(
        &mut fixture.control,
        &fixture.host,
        &mut ui,
        Builtin::Reload,
        "",
    )
    .unwrap();
    assert_eq!(
        ui.themes.theme(),
        &crate::presentation_style::Theme::fixed(false)
    );
    assert_eq!(ui.keys.label(KeyAction::Model), "Alt-M");
    assert!(
        ui.status.is_empty(),
        "a retained status still advertises the previous keymap"
    );
    assert_eq!(ui.editor.draft, "literal 🦀 draft");
    assert_eq!(ui.editor.cursor, "literal ".len());
    fixture.control.shutdown().await;
}

#[test]
fn custom_actions_preserve_literal_case_recovery_and_disabled_shortcuts() {
    let mut ui = Frontend {
        editor: Editor {
            images: vec![ImageSource::Path(PathBuf::from("retained.png"))],
            ..Editor::default()
        },
        keys: KeyBindings::parse(
            br#"{
            "app.model.select":"alt+m",
            "tui.editor.undo":"u",
            "tui.editor.redo":"alt+r",
            "app.message.copy":[]
        }"#,
        )
        .unwrap(),
        ..Frontend::default()
    };
    for ch in "Ab🦀".chars() {
        assert!(matches!(
            ui.key(
                KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE),
                None,
                None
            ),
            Action::None
        ));
    }
    assert_eq!(ui.editor.draft, "Ab🦀");
    for event in [
        KeyEvent::new(KeyCode::Char('l'), Modifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('x'), Modifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('\x1f'), Modifiers::NONE),
    ] {
        assert!(matches!(ui.key(event, None, None), Action::None));
        assert_eq!(ui.editor.draft, "Ab🦀");
    }
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('m'), Modifiers::ALT),
            None,
            None
        ),
        Action::Command(CommandInput::Shortcut(Builtin::Model))
    ));
    assert_eq!(ui.editor.cursor, ui.editor.draft.len());
    ui.key(
        KeyEvent::new(KeyCode::Char('u'), Modifiers::NONE),
        None,
        None,
    );
    assert!(
        ui.editor.draft.is_empty(),
        "a bound printable key is not typing/coalescing"
    );
    ui.key(
        KeyEvent::new(KeyCode::Char('r'), Modifiers::ALT),
        None,
        None,
    );
    assert_eq!(ui.editor.draft, "Ab🦀");
    assert_eq!(ui.editor.images.len(), 1);
}

#[tokio::test]
async fn custom_file_confirmation_is_a_visible_atomic_edit_not_follow_up_or_submit() {
    let root = std::env::temp_dir().join(format!("ion-key-files-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("file.rs"), "not read by completion").unwrap();
    let mut ui = Frontend {
        keys: KeyBindings::parse(
            br#"{
            "tui.input.tab":"ctrl+t",
            "tui.input.submit":"ctrl+s",
            "tui.select.confirm":"alt+enter",
            "tui.select.cancel":"alt+x"
        }"#,
        )
        .unwrap(),
        cwd: root.clone(),
        ..Frontend::default()
    };
    ui.replace_draft_atomic("fi".into());
    ui.key(
        KeyEvent::new(KeyCode::Char('t'), Modifiers::CONTROL),
        None,
        None,
    );
    while ui.pickers.has_job() {
        ui.pickers.complete().await;
    }
    ui.refresh_completion(None, None, false);
    assert!(
        ui.editor
            .completion
            .painted_rows(100, 6, &ui.keys)
            .iter()
            .any(|row| row.contains("file.rs"))
    );
    for code in [KeyCode::Tab, KeyCode::Enter, KeyCode::Esc] {
        assert!(matches!(
            ui.key(KeyEvent::new(code, Modifiers::NONE), None, None),
            Action::None
        ));
        assert_eq!(ui.editor.draft, "fi");
        assert!(ui.editor.completion.visible());
    }
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::ALT), None, None),
        Action::None
    ));
    assert_eq!(ui.editor.draft, "file.rs ");
    ui.key(
        KeyEvent::new(KeyCode::Char('\x1f'), Modifiers::NONE),
        None,
        None,
    );
    assert_eq!(
        ui.editor.draft, "fi",
        "one undo must restore the entire replacement"
    );
    assert!(
        matches!(ui.key(KeyEvent::new(KeyCode::Char('s'), Modifiers::CONTROL), None, None), Action::Submit(text) if text.literal == "fi")
    );
    ui.pickers.shutdown().await.unwrap();
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn custom_question_controls_share_editing_without_accepting_clipped_answers() {
    let keys = KeyBindings::parse(
        br#"{
        "tui.select.confirm":"alt+enter",
        "tui.select.down":"ctrl+n",
        "app.session.toggleNamedFilter":[],
        "tui.select.cancel":"alt+x",
        "tui.input.submit":"alt+s",
        "tui.input.newLine":"ctrl+r",
        "tui.question.toggleCollapsed":"alt+q"
    }"#,
    )
    .unwrap();
    let request = ion_host::QuestionRequestId(uuid::Uuid::now_v7());
    let questions = vec![ion_host::Question {
        question: "Choose".into(),
        header: None,
        multi_select: false,
        options: vec![ion_host::QuestionOption {
            label: "Option".into(),
            description: None,
            preview: None,
        }],
    }];
    let mut question = QuestionModal::new(request, questions.clone());
    question.painted_render(3, 1, &keys, &crate::presentation_style::Theme::default());
    assert!(matches!(
        question.key(KeyEvent::new(KeyCode::Enter, Modifiers::ALT), &keys),
        QuestionAction::None
    ));
    assert!(matches!(
        question.key(KeyEvent::new(KeyCode::Esc, Modifiers::NONE), &keys),
        QuestionAction::None
    ));
    assert!(
        matches!(question.key(KeyEvent::new(KeyCode::Char('x'), Modifiers::ALT), &keys), QuestionAction::Cancel(id) if id == request)
    );
    let mut question = QuestionModal::new(request, questions);
    question.key(KeyEvent::new(KeyCode::Char('n'), Modifiers::CONTROL), &keys);
    question.painted_render(80, 24, &keys, &crate::presentation_style::Theme::default());
    question.key(KeyEvent::new(KeyCode::Enter, Modifiers::ALT), &keys);
    for ch in "Exact 🦀".chars() {
        question.key(KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE), &keys);
    }
    question.key(KeyEvent::new(KeyCode::Char('r'), Modifiers::CONTROL), &keys);
    question.paste("next");
    question.key(KeyEvent::new(KeyCode::Char(']'), Modifiers::CONTROL), &keys);
    assert!(
        !question.is_collapsed(),
        "replaced default binding remained active"
    );
    question.key(KeyEvent::new(KeyCode::Char('q'), Modifiers::ALT), &keys);
    assert!(question.is_collapsed());
    assert!(matches!(
        question.key(KeyEvent::new(KeyCode::Char('s'), Modifiers::ALT), &keys),
        QuestionAction::None
    ));
    question.key(KeyEvent::new(KeyCode::Char('q'), Modifiers::ALT), &keys);
    assert!(matches!(
        question.key(KeyEvent::new(KeyCode::Char('s'), Modifiers::ALT), &keys),
        QuestionAction::None
    ));
    question.painted_render(80, 24, &keys, &crate::presentation_style::Theme::default());
    assert!(matches!(
        question.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &keys),
        QuestionAction::None
    ));
    assert!(
        matches!(question.key(KeyEvent::new(KeyCode::Char('s'), Modifiers::ALT), &keys), QuestionAction::Answer(id, answers) if id == request && answers == vec![ion_host::QuestionAnswer::Text { text: "Exact 🦀\nnext".into() }])
    );
}
