//! Publication, not layout, makes a retained interaction actionable.
use super::*;

#[test]
fn narrow_busy_dock_keeps_the_stop_action_before_secondary_hints() {
    let mut ui = Frontend::default();
    let operation = ActiveOperation {
        kind: activity::OperationKind::Coding,
        stop_requested: false,
        pending_follow_ups: 0,
    };
    let dock = Dock::prepare(&mut ui, Some(operation), None, 24, 8);
    assert!(
        dock.rows
            .iter()
            .any(|row| row.to_string().contains("Ctrl-C cancels"))
    );
    assert!(dock.rows.len() + dock.body_rows <= 8);
    assert!(dock.cursor.is_some());
}
use super::{dock::Dock, interaction::InputTarget, picker::PickerItem};
use ion_host::{Question, QuestionOption, QuestionRequestId};
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
    assert_eq!(exact.rows.len(), 5);
    assert_eq!(exact.body_rows, 14);
    assert!(exact.cursor.is_none());
    publish_to(&mut ui, exact, (80, 20), &mut Vec::new()).unwrap();
    assert!(
        matches!(ui.question.as_mut().unwrap().key(enter(), &ui.keys), QuestionAction::Answer(_, answers) if answers == vec![ion_host::QuestionAnswer::Option { index: 0 }])
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
