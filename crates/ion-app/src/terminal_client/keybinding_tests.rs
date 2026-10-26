//! Rebinding must change real consumers, not just registry/help output.
use super::*;

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
    assert_eq!(ui.draft, "/he");
    assert!(!ui.completion.rows(80, 6, &ui.keys).is_empty());
    ui.key(
        KeyEvent::new(KeyCode::Char('y'), Modifiers::ALT),
        None,
        None,
    );
    assert!(!ui.completion.visible());
    ui.key(
        KeyEvent::new(KeyCode::Char('y'), Modifiers::ALT),
        None,
        None,
    );
    assert_eq!(
        ui.draft, "/he",
        "dismissal is not a directly preceding yank"
    );
}

#[tokio::test]
async fn reload_publishes_prepared_keys_and_discards_only_obsolete_status_hints() {
    let mut fixture = super::tests::Fixture::new();
    let mut ui = Frontend::default();
    ui.replace_draft_atomic("literal 🦀 draft".into());
    ui.cursor = "literal ".len();
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
    command::handle_builtin(
        &mut fixture.control,
        &fixture.host,
        &mut ui,
        Builtin::Reload,
        "",
    )
    .unwrap();
    assert_eq!(ui.keys.label(KeyAction::Model), "Alt-M");
    assert!(
        ui.status.is_empty(),
        "a retained status still advertises the previous keymap"
    );
    assert_eq!(ui.draft, "literal 🦀 draft");
    assert_eq!(ui.cursor, "literal ".len());
    fixture.control.shutdown().await;
}

#[test]
fn custom_actions_preserve_literal_case_recovery_and_disabled_shortcuts() {
    let mut ui = Frontend {
        keys: KeyBindings::parse(
            br#"{
            "app.model.select":"alt+m",
            "tui.editor.undo":"u",
            "tui.editor.redo":"alt+r",
            "app.message.copy":[]
        }"#,
        )
        .unwrap(),
        images: vec![ImageSource::Path(PathBuf::from("retained.png"))],
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
    assert_eq!(ui.draft, "Ab🦀");
    for event in [
        KeyEvent::new(KeyCode::Char('l'), Modifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('x'), Modifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('\x1f'), Modifiers::NONE),
    ] {
        assert!(matches!(ui.key(event, None, None), Action::None));
        assert_eq!(ui.draft, "Ab🦀");
    }
    assert!(matches!(
        ui.key(
            KeyEvent::new(KeyCode::Char('m'), Modifiers::ALT),
            None,
            None
        ),
        Action::Command(CommandInput::Shortcut(Builtin::Model))
    ));
    assert_eq!(ui.cursor, ui.draft.len());
    ui.key(
        KeyEvent::new(KeyCode::Char('u'), Modifiers::NONE),
        None,
        None,
    );
    assert!(
        ui.draft.is_empty(),
        "a bound printable key is not typing/coalescing"
    );
    ui.key(
        KeyEvent::new(KeyCode::Char('r'), Modifiers::ALT),
        None,
        None,
    );
    assert_eq!(ui.draft, "Ab🦀");
    assert_eq!(ui.images.len(), 1);
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
        ui.completion
            .rows(100, 6, &ui.keys)
            .iter()
            .any(|row| row.contains("file.rs"))
    );
    for code in [KeyCode::Tab, KeyCode::Enter, KeyCode::Esc] {
        assert!(matches!(
            ui.key(KeyEvent::new(code, Modifiers::NONE), None, None),
            Action::None
        ));
        assert_eq!(ui.draft, "fi");
        assert!(ui.completion.visible());
    }
    assert!(matches!(
        ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::ALT), None, None),
        Action::None
    ));
    assert_eq!(ui.draft, "file.rs ");
    ui.key(
        KeyEvent::new(KeyCode::Char('\x1f'), Modifiers::NONE),
        None,
        None,
    );
    assert_eq!(
        ui.draft, "fi",
        "one undo must restore the entire replacement"
    );
    assert!(
        matches!(ui.key(KeyEvent::new(KeyCode::Char('s'), Modifiers::CONTROL), None, None), Action::Submit(text) if text == "fi")
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
        "tui.input.newLine":"ctrl+r"
    }"#,
    )
    .unwrap();
    let request = ion_host::QuestionRequestId(uuid::Uuid::now_v7());
    let questions = vec![ion_host::Question {
        question: "Choose".into(),
        multi_select: false,
        options: vec![ion_host::QuestionOption {
            label: "Option".into(),
            description: None,
        }],
    }];
    let mut question = QuestionModal::new(request, questions.clone());
    question.render(3, 1, &keys);
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
    question.key(KeyEvent::new(KeyCode::Enter, Modifiers::ALT), &keys);
    for ch in "Exact 🦀".chars() {
        question.key(KeyEvent::new(KeyCode::Char(ch), Modifiers::NONE), &keys);
    }
    question.key(KeyEvent::new(KeyCode::Char('r'), Modifiers::CONTROL), &keys);
    question.paste("next");
    assert!(matches!(
        question.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &keys),
        QuestionAction::None
    ));
    assert!(
        matches!(question.key(KeyEvent::new(KeyCode::Char('s'), Modifiers::ALT), &keys), QuestionAction::Answer(id, answers) if id == request && answers == vec![ion_host::QuestionAnswer::Text { text: "Exact 🦀\nnext".into() }])
    );
}
