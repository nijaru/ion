//! Publication, not layout, makes a retained interaction actionable.
use super::*;

#[test]
fn interrupt_requires_confirmation_and_does_not_escalate_after_clear() {
    let mut ui = Frontend::default();
    ui.insert("unsent α\n🦀");
    let interrupt = KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL);
    assert!(matches!(ui.key(interrupt, None, None), Action::None));
    assert_eq!(
        ui.editor.draft, "unsent α\n🦀",
        "first press only arms clear"
    );
    assert!(visible_status(&ui, None).unwrap().contains("Clear draft?"));
    assert!(matches!(ui.key(interrupt, None, None), Action::None));
    assert!(ui.editor.draft.is_empty());
    for _ in 0..3 {
        assert!(matches!(ui.key(interrupt, None, None), Action::None));
    }
    ui.key(
        KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL),
        None,
        None,
    );
    assert_eq!(ui.editor.draft, "unsent α\n🦀");
}

#[test]
fn clear_restores_rich_draft_cursor_and_survives_an_undo_baseline_change() {
    let mut ui = Frontend::default();
    ui.insert("α\n🦀 end");
    ui.editor.cursor = "α\n".len();
    ui.attach_image(ImageSource::Path("retained.png".into()));
    ui.editor.files.push(ion_app::prompt_files::TextFile {
        name: "context.txt".into(),
        text: "literal α\r\n".into(),
    });
    let escape = KeyEvent::new(KeyCode::Esc, Modifiers::NONE);
    let undo = KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL);
    let redo = KeyEvent::new(KeyCode::Char('_'), Modifiers::CONTROL | Modifiers::ALT);
    ui.key(escape, None, None);
    assert_eq!(ui.editor.images.len(), 1);
    assert_eq!(ui.editor.files.len(), 1);
    ui.key(escape, None, None);
    assert!(
        ui.editor.draft.is_empty() && ui.editor.images.is_empty() && ui.editor.files.is_empty()
    );
    ui.key(undo, None, None);
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("α\n🦀 end", "α\n".len())
    );
    assert!(
        matches!(&ui.editor.images[..], [ImageSource::Path(path)] if path == std::path::Path::new("retained.png"))
    );
    assert_eq!(ui.editor.files[0].text, "literal α\r\n");
    ui.key(redo, None, None);
    assert!(ui.editor.images.is_empty() && ui.editor.files.is_empty());
    // A later attachment ends text undo history, but cannot discard the
    // cleared draft. It moves into the existing local recovery custody.
    ui.attach_image(ImageSource::Path("new.png".into()));
    assert_eq!(ui.editor.recovered.len(), 1);
    ui.recover_draft();
    assert_eq!(
        (ui.editor.draft.as_str(), ui.editor.cursor),
        ("α\n🦀 end", "α\n".len())
    );
    assert_eq!(ui.editor.files[0].text, "literal α\r\n");
    assert!(
        matches!(&ui.editor.images[..], [ImageSource::Path(path)] if path == std::path::Path::new("retained.png"))
    );
    assert_eq!(
        ui.editor.recovered.len(),
        1,
        "new attachment remains recoverable too"
    );
}

#[test]
fn stale_clear_and_different_bound_shortcuts_never_confirm_each_other() {
    let mut ui = Frontend {
        keys: KeyBindings::parse(br#"{"app.interrupt":["ctrl+q","ctrl+c"]}"#).unwrap(),
        ..Frontend::default()
    };
    ui.insert("old draft");
    let q = KeyEvent::new(KeyCode::Char('q'), Modifiers::CONTROL);
    let c = KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL);
    ui.key(q, None, None);
    assert_eq!(ui.gestures.prompt(), Some("Clear draft? Ctrl-Q again"));
    ui.key(c, None, None);
    assert_eq!(ui.editor.draft, "old draft");
    assert_eq!(ui.gestures.prompt(), Some("Clear draft? Ctrl-C again"));
    // History refresh can replace a baseline without a keyboard event or a
    // Frontend wrapper. The editor's identity must still reject the old arm.
    ui.editor
        .restore_editor(EditorDraft::at_end("new draft".into()));
    ui.key(c, None, None);
    assert_eq!(ui.editor.draft, "new draft");
    assert!(ui.gestures.prompt().is_none());
}

#[tokio::test]
async fn completion_dismissal_is_separate_from_both_stop_confirmation_presses() {
    let mut fixture = tests::Fixture::new();
    fixture
        .control
        .compact(None, EditorDraft::at_end(String::new()))
        .unwrap();
    let target = fixture.control.stop_target();
    let mut ui = Frontend::default();
    ui.insert("/he");
    ui.refresh_completion(None, None, false);
    assert!(
        !ui.editor
            .completion
            .painted_rows(80, 6, &ui.keys)
            .is_empty()
    );
    let escape = KeyEvent::new(KeyCode::Esc, Modifiers::NONE);
    assert_eq!(ui.stop_key(escape, target.clone()), None);
    ui.key(escape, None, None);
    assert!(!ui.editor.completion.active());
    assert!(ui.gestures.prompt().is_none());
    assert_eq!(ui.stop_key(escape, target.clone()), Some(false));
    assert_eq!(ui.stop_key(escape, target), Some(true));
    assert_eq!(ui.editor.draft, "/he");
    fixture.control.shutdown().await;
}

#[test]
fn exit_is_guarded_but_forward_delete_and_confirmation_dismissal_are_not() {
    let exit = KeyEvent::new(KeyCode::Char('d'), Modifiers::CONTROL);
    let escape = KeyEvent::new(KeyCode::Esc, Modifiers::NONE);
    let mut ui = Frontend::default();
    assert!(matches!(ui.key(exit, None, None), Action::None));
    assert!(
        visible_status(&ui, None)
            .unwrap()
            .contains("Exit Ion? Ctrl-D again")
    );
    assert!(matches!(ui.key(escape, None, None), Action::None));
    assert!(
        ui.gestures.prompt().is_none(),
        "Esc dismisses another key's confirmation"
    );
    ui.insert("α🦀");
    ui.editor.cursor = 0;
    ui.key(exit, None, None);
    assert_eq!(ui.editor.draft, "🦀");
    ui.key(exit, None, None);
    assert!(ui.editor.draft.is_empty());
    ui.attach_image(ImageSource::Path("image-only.png".into()));
    assert!(matches!(ui.key(exit, None, None), Action::None));
    assert!(matches!(ui.key(exit, None, None), Action::None));
    assert_eq!(ui.editor.images.len(), 1);
    ui.editor.take_attachments();
    assert!(matches!(ui.key(exit, None, None), Action::None));
    assert!(matches!(ui.key(exit, None, None), Action::Quit));
}

#[tokio::test]
async fn fresh_interrupt_stops_once_without_a_confirmation_or_clear_exit_cascade() {
    for (keys, character) in [
        (KeyBindings::default(), 'c'),
        (
            KeyBindings::parse(br#"{"app.interrupt":"ctrl+q"}"#).unwrap(),
            'q',
        ),
    ] {
        for (literal, cursor) in [("unsent 🦀", "unsent ".len()), ("", 0)] {
            let mut fixture = tests::Fixture::new();
            fixture
                .control
                .compact(None, EditorDraft::at_end(String::new()))
                .unwrap();
            let target = fixture.control.stop_target().unwrap();
            let interrupt = KeyEvent::new(KeyCode::Char(character), Modifiers::CONTROL);
            let repeat = KeyEvent {
                repeat: true,
                ..interrupt
            };
            let mut ui = Frontend {
                keys: keys.clone(),
                ..Frontend::default()
            };
            ui.insert(literal);
            ui.editor.cursor = cursor;
            assert_eq!(ui.stop_key(repeat, Some(target.clone())), Some(false));
            assert!(ui.gestures.prompt().is_none());
            assert_eq!(ui.stop_key(interrupt, Some(target.clone())), Some(true));
            assert!(ui.gestures.prompt().is_none());
            assert_eq!(ui.stop_key(interrupt, Some(target)), Some(false));
            fixture.control.request_stop();
            assert_eq!(ui.stop_key(interrupt, None), None);
            for key in [interrupt, repeat, interrupt] {
                assert!(matches!(ui.key(key, None, None), Action::None));
            }
            assert_eq!(
                (ui.editor.draft.as_str(), ui.editor.cursor),
                (literal, cursor)
            );
            assert!(ui.gestures.prompt().is_none());
            fixture.control.shutdown().await;
        }
    }
}

#[tokio::test]
async fn escape_stop_confirmation_is_scoped_to_work_and_extra_taps_cannot_clear_or_exit() {
    let mut fixture = tests::Fixture::new();
    fixture
        .control
        .compact(None, EditorDraft::at_end(String::new()))
        .unwrap();
    let target = fixture.control.stop_target().unwrap();
    let interrupt = KeyEvent::new(KeyCode::Esc, Modifiers::NONE);
    let mut ui = Frontend::default();
    ui.insert("unsent 🦀");
    assert_eq!(ui.stop_key(interrupt, Some(target.clone())), Some(false));
    assert!(!fixture.control.state().stop_requested);
    // An additional queued input changes the cancellation domain even though
    // the current operation is unchanged.
    assert!(
        fixture
            .control
            .submit(
                Delivery::FollowUp,
                ion_app::UserInput::Prompt {
                    text: "queued".into(),
                    images: vec![],
                    files: vec![]
                },
                EditorDraft::at_end("queued".into())
            )
            .is_ok()
    );
    let changed = fixture.control.stop_target().unwrap();
    assert_ne!(target, changed);
    assert_eq!(ui.stop_key(interrupt, Some(changed.clone())), Some(false));
    assert!(ui.gestures.prompt().is_none());
    ui.begin_key(KeyEvent::new(KeyCode::Left, Modifiers::NONE));
    assert_eq!(ui.stop_key(interrupt, Some(changed.clone())), Some(false));
    for mode in [TuiMode::Inline, TuiMode::Fullscreen] {
        ui.mode = mode;
        let dock = Dock::prepare(&mut ui, None, None, 24, 2);
        assert!(
            dock.rows
                .iter()
                .any(|row| row.to_string().contains("Stop work? Esc again"))
        );
        assert!(dock.cursor.is_some());
    }
    assert_eq!(ui.stop_key(interrupt, Some(changed)), Some(true));
    fixture.control.request_stop();
    assert_eq!(ui.stop_key(interrupt, None), None);
    for _ in 0..3 {
        assert!(matches!(ui.key(interrupt, None, None), Action::None));
    }
    assert_eq!(ui.editor.draft, "unsent 🦀");
    fixture.control.shutdown().await;
}

#[test]
fn question_cancel_and_its_hint_use_the_same_request_and_preserve_the_composer() {
    let mut ui = Frontend {
        question: Some(question()),
        ..Frontend::default()
    };
    ui.insert("composer 🦀");
    let escape = KeyEvent::new(KeyCode::Esc, Modifiers::NONE);
    assert!(matches!(ui.question_key(escape), QuestionAction::None));
    for mode in [TuiMode::Inline, TuiMode::Fullscreen] {
        ui.mode = mode;
        let dock = Dock::prepare(&mut ui, None, None, 24, 2);
        assert!(
            dock.rows
                .iter()
                .any(|row| row.to_string().contains("Cancel question? Esc"))
        );
        assert!(dock.rows.len() + dock.body_rows < 3);
    }
    let request = ui.question.as_ref().unwrap().request();
    assert!(matches!(ui.question_key(escape), QuestionAction::Cancel(id) if id == request));
    assert_eq!(ui.editor.draft, "composer 🦀");
}

#[tokio::test]
async fn narrow_busy_dock_keeps_the_stop_action_before_secondary_hints() {
    let mut fixture = tests::Fixture::new();
    let operation_id = fixture
        .control
        .compact(None, EditorDraft::at_end(String::new()))
        .unwrap();
    let operation = ActiveOperation {
        kind: activity::OperationKind::Coding,
        stop_requested: false,
        pending_follow_ups: 0,
    };
    for mode in [TuiMode::Inline, TuiMode::Fullscreen] {
        for status in [String::new(), "diagnostic ".repeat(128)] {
            let mut ui = Frontend {
                status,
                mode,
                ..Frontend::default()
            };
            ui.conversation = conversation::fixtures::document(Default::default());
            ui.conversation.observe(
                operation_id,
                &ion_core::AgentEvent::TurnAccepted {
                    entry: 1,
                    turn: 1,
                    input: ion_ai::Message::user_input("request".into(), []),
                },
            );
            for notice in [String::new(), "older notice ".repeat(128)] {
                ui.conversation.observe(
                    operation_id,
                    &ion_core::AgentEvent::ToolCatalogWarning(notice),
                );
                for height in [2, 8] {
                    let dock = Dock::prepare(&mut ui, Some(operation), None, 24, height);
                    assert!(
                        dock.rows
                            .iter()
                            .any(|row| row.to_string().contains("Ctrl-C stops"))
                    );
                    assert!(dock.rows.len() + dock.body_rows <= height);
                    assert!(dock.cursor.is_some());
                }
            }
        }
    }
    fixture.control.shutdown().await;
}
use super::{dock::Dock, interaction::InputTarget, picker::PickerItem};
use ion_app::{Question, QuestionOption, QuestionRequestId};
use std::io::{self, Write};

fn selector() -> Frontend {
    let mut ui = Frontend::default();
    ui.pickers.open(Picker {
        title: "Choose model",
        query: String::new(),
        selected: 0,
        items: ["alpha", "beta"]
            .into_iter()
            .map(|name| PickerItem {
                label: name.into(),
                detail: None,
                value: PickerValue::Login(name.into()),
            })
            .collect(),
    });
    ui
}

fn question() -> QuestionModal {
    QuestionModal::new(
        QuestionRequestId(uuid::Uuid::now_v7()),
        vec![Question {
            question: "Choose a route".into(),
            header: None,
            multi_select: false,
            options: vec![QuestionOption {
                label: "Alpha".into(),
                description: None,
                preview: None,
            }],
        }],
    )
}

fn enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, Modifiers::NONE)
}
fn down() -> KeyEvent {
    KeyEvent::new(KeyCode::Down, Modifiers::NONE)
}

fn publish_to(
    ui: &mut Frontend,
    dock: Dock,
    geometry: (u16, u16),
    out: &mut impl Write,
) -> io::Result<()> {
    let frame = Frame {
        live: &dock.rows,
        cursor: dock.cursor,
    };
    let mut screen =
        Screen::with_live_height(geometry.0, 0, geometry.1, frame.required_rows(geometry.0));
    screen.draw(out, &frame)?;
    // The terminal session supplies this observation after its geometry fence.
    dock.paint
        .observe_simulated(ui, geometry, ui.mode == TuiMode::Fullscreen);
    Ok(())
}

struct BrokenOutput;
impl Write for BrokenOutput {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed output"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("closed output"))
    }
}

#[test]
fn held_confirmation_cannot_accept_a_painted_picker() {
    for mode in [TuiMode::Inline, TuiMode::Fullscreen] {
        let mut ui = selector();
        ui.mode = mode;
        let dock = Dock::prepare(&mut ui, None, None, 80, 20);
        publish_to(&mut ui, dock, (80, 20), &mut Vec::new()).unwrap();
        assert!(matches!(
            ui.key(
                KeyEvent {
                    repeat: true,
                    ..enter()
                },
                None,
                None
            ),
            Action::None
        ));
        assert_eq!(ui.pickers.active().unwrap().selected, 0);
        assert!(
            matches!(ui.key(enter(), None, None), Action::Pick(PickerValue::Login(name)) if name == "alpha")
        );
    }
}

#[test]
fn held_picker_key_cannot_submit_the_underlying_draft_after_a_new_paint() {
    for (keys, confirm) in [
        (KeyBindings::default(), enter()),
        (
            KeyBindings::parse(br#"{"tui.select.confirm":"z","tui.input.submit":"z"}"#).unwrap(),
            KeyEvent::new(KeyCode::Char('z'), Modifiers::NONE),
        ),
    ] {
        for mode in [TuiMode::Inline, TuiMode::Fullscreen] {
            for literal in ["kept α\ncaret 🦀", "!touch REPEAT_MUST_NOT_RUN"] {
                let mut ui = selector();
                ui.keys = keys.clone();
                ui.mode = mode;
                // This draft was authored before the picker took focus.
                ui.editor.insert(literal);
                ui.editor.cursor = literal.find('🦀').unwrap_or(0);
                let cursor = ui.editor.cursor;
                ui.attach_image(ImageSource::Path("retained.png".into()));
                let picker = Dock::prepare(&mut ui, None, None, 80, 20);
                publish_to(&mut ui, picker, (80, 20), &mut Vec::new()).unwrap();
                assert!(matches!(ui.key(confirm, None, None), Action::Pick(_)));
                let composer = Dock::prepare(&mut ui, None, None, 80, 20);
                publish_to(&mut ui, composer, (80, 20), &mut Vec::new()).unwrap();
                for _ in 0..3 {
                    assert!(matches!(
                        ui.key(
                            KeyEvent {
                                repeat: true,
                                ..confirm
                            },
                            None,
                            None
                        ),
                        Action::None
                    ));
                    assert_eq!(
                        (ui.editor.draft.as_str(), ui.editor.cursor),
                        (literal, cursor)
                    );
                    assert_eq!(ui.editor.images.len(), 1);
                }
                let action = ui.key(confirm, None, None);
                assert!(if let Some(command) = literal.strip_prefix('!') {
                    matches!(action, Action::Shell { command: actual, exclude_from_context: false, source }
                        if actual == command && source.literal == literal && source.cursor == cursor)
                } else {
                    matches!(action, Action::Submit(source)
                        if source.literal == literal && source.cursor == cursor)
                });
            }
        }
    }
}

#[test]
fn selector_layout_rejected_geometry_failed_write_and_stale_choice_are_inert() {
    let mut ui = selector();
    let layout_only = Dock::prepare(&mut ui, None, None, 80, 20);
    assert!(matches!(ui.key(enter(), None, None), Action::None));
    drop(layout_only); // Deferred inline reconciliation publishes nothing.

    let rejected = Dock::prepare(&mut ui, None, None, 80, 20);
    publish_to(&mut ui, rejected, (79, 20), &mut Vec::new()).unwrap();
    assert!(matches!(ui.key(enter(), None, None), Action::None));

    let failed = Dock::prepare(&mut ui, None, None, 80, 20);
    assert!(publish_to(&mut ui, failed, (80, 20), &mut BrokenOutput).is_err());
    assert!(matches!(ui.key(enter(), None, None), Action::None));

    let stale = Dock::prepare(&mut ui, None, None, 80, 20);
    ui.key(down(), None, None);
    publish_to(&mut ui, stale, (80, 20), &mut Vec::new()).unwrap();
    assert!(matches!(ui.key(enter(), None, None), Action::None));
    assert_eq!(ui.pickers.active().unwrap().selected, 1);

    let overwritten = Dock::prepare(&mut ui, None, None, 80, 20);
    let newer = Dock::prepare(&mut ui, None, None, 80, 20);
    publish_to(&mut ui, newer, (80, 20), &mut Vec::new()).unwrap();
    publish_to(&mut ui, overwritten, (80, 20), &mut Vec::new()).unwrap();
    assert!(matches!(ui.key(enter(), None, None), Action::None));

    let clipped = Dock::prepare(&mut ui, None, None, 2, 2);
    publish_to(&mut ui, clipped, (2, 2), &mut Vec::new()).unwrap();
    assert!(matches!(ui.key(enter(), None, None), Action::None));
    assert_eq!(ui.pickers.active().unwrap().selected, 1);

    let exact = Dock::prepare(&mut ui, None, None, 80, 20);
    publish_to(&mut ui, exact, (80, 20), &mut Vec::new()).unwrap();
    assert!(
        matches!(ui.key(enter(), None, None), Action::Pick(PickerValue::Login(name)) if name == "beta")
    );
}

#[test]
fn foreground_focus_revokes_question_paint_and_short_questions_retain_conversation_space() {
    let mut ui = Frontend {
        question: Some(question()),
        ..Frontend::default()
    };
    let layout_only = Dock::prepare(&mut ui, None, None, 80, 20);
    assert!(matches!(
        ui.question.as_mut().unwrap().key(enter(), &ui.keys),
        QuestionAction::None
    ));
    drop(layout_only);

    let stale = Dock::prepare(&mut ui, None, None, 80, 20);
    ui.pickers = selector().pickers;
    assert_eq!(ui.input_target(), InputTarget::Picker);
    let overlay = Dock::prepare(&mut ui, None, None, 80, 20);
    publish_to(&mut ui, overlay, (80, 20), &mut Vec::new()).unwrap();
    ui.pickers.close();
    publish_to(&mut ui, stale, (80, 20), &mut Vec::new()).unwrap();
    assert!(matches!(
        ui.question.as_mut().unwrap().key(enter(), &ui.keys),
        QuestionAction::None
    ));

    let exact = Dock::prepare(&mut ui, None, None, 80, 20);
    // A short ordinary question leaves conversation space and no composer caret.
    // Guarded cancellation controls occupy one additional physical row.
    assert_eq!(exact.rows.len(), 6);
    assert_eq!(exact.body_rows, 13);
    assert!(exact.cursor.is_none());
    publish_to(&mut ui, exact, (80, 20), &mut Vec::new()).unwrap();
    assert!(
        matches!(ui.question.as_mut().unwrap().key(enter(), &ui.keys), QuestionAction::Answer(_, answers) if answers == vec![ion_app::QuestionAnswer::Option { index: 0 }])
    );
}

#[test]
fn hidden_question_never_paints_or_enables_composer_and_preserves_literal_draft() {
    let literal = "  draft\t🦀\nnext  ";
    let mut ui = Frontend {
        editor: Editor {
            draft: literal.into(),
            cursor: 3,
            ..Editor::default()
        },
        question: Some(question()),
        ..Frontend::default()
    };
    let collapse = KeyEvent::new(KeyCode::Char(']'), Modifiers::CONTROL);
    ui.question.as_mut().unwrap().key(collapse, &ui.keys);
    assert_eq!(ui.input_target(), InputTarget::Question);
    for mode in [TuiMode::Inline, TuiMode::Fullscreen] {
        ui.mode = mode;
        let dock = Dock::prepare(&mut ui, None, None, 80, 20);
        assert!(dock.cursor.is_none());
        assert_eq!(dock.rows.len(), 1);
        assert!(dock.rows[0].to_string().starts_with("Question hidden"));
        publish_to(&mut ui, dock, (80, 20), &mut Vec::new()).unwrap();
        assert_eq!(ui.editor.editor_width, None);
        assert!(matches!(
            ui.key(
                KeyEvent::new(KeyCode::Char('x'), Modifiers::NONE),
                None,
                None
            ),
            Action::None
        ));
        assert!(matches!(
            ui.question.as_mut().unwrap().paste("hidden paste"),
            QuestionAction::None
        ));
        assert_eq!((ui.editor.draft.as_str(), ui.editor.cursor), (literal, 3));
    }
}

#[test]
fn completion_layout_does_not_authorize_and_only_normal_composer_paint_publishes_width() {
    let mut ui = Frontend::default();
    ui.insert("/he");
    ui.refresh_completion(None, None, false);
    let layout = Dock::prepare(&mut ui, None, None, 80, 20);
    assert!(!ui.editor.completion.visible());
    assert!(ui.editor.completion.replacement(&ui.editor.draft).is_none());
    assert_eq!(ui.editor.editor_width, None);
    drop(layout);
    let stale = Dock::prepare(&mut ui, None, None, 80, 20);
    ui.insert("l");
    ui.refresh_completion(None, None, false);
    publish_to(&mut ui, stale, (80, 20), &mut Vec::new()).unwrap();
    assert!(!ui.editor.completion.visible());
    for width in 1..=4 {
        let clipped = Dock::prepare(&mut ui, None, None, width, 20);
        publish_to(&mut ui, clipped, (width as u16, 20), &mut Vec::new()).unwrap();
        assert!(
            !ui.editor.completion.visible(),
            "a slash or omission marker is not a command name"
        );
        assert!(ui.editor.completion.replacement(&ui.editor.draft).is_none());
    }
    let exact = Dock::prepare(&mut ui, None, None, 80, 20);
    publish_to(&mut ui, exact, (80, 20), &mut Vec::new()).unwrap();
    assert_eq!(ui.editor.editor_width, Some(80));
    assert!(ui.editor.completion.visible());
    ui.key(KeyEvent::new(KeyCode::Tab, Modifiers::NONE), None, None);
    assert_eq!(ui.editor.draft, "/help ");

    ui.pickers = selector().pickers;
    let query = Dock::prepare(&mut ui, None, None, 40, 20);
    publish_to(&mut ui, query, (40, 20), &mut Vec::new()).unwrap();
    assert_eq!(ui.editor.editor_width, Some(80));
}
