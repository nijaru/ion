use super::*;
use ion_host::{Question, QuestionOption};

pub(super) fn modal() -> QuestionModal {
    QuestionModal::new(
        QuestionRequestId(uuid::Uuid::now_v7()),
        vec![
            Question {
                question: "Choose a route".into(),
                header: None,
                multi_select: false,
                options: vec![
                    QuestionOption {
                        label: "Alpha".into(),
                        description: Some("First choice".into()),
                        preview: None,
                    },
                    QuestionOption {
                        label: "Beta".into(),
                        description: None,
                        preview: None,
                    },
                ],
            },
            Question {
                question: "Custom?".into(),
                header: None,
                multi_select: false,
                options: vec![
                    QuestionOption {
                        label: "Yes".into(),
                        description: None,
                        preview: None,
                    },
                    QuestionOption {
                        label: "No".into(),
                        description: None,
                        preview: None,
                    },
                ],
            },
        ],
    )
}

// The real caller paints between input events; use its actual renderer here,
// rather than authorizing a choice by mutating presentation flags in fixtures.
pub(super) fn press(
    modal: &mut QuestionModal,
    key: KeyEvent,
    keys: &KeyBindings,
) -> QuestionAction {
    modal.render(80, 24, keys, &crate::presentation_style::Theme::default());
    modal.key(key, keys)
}

pub(super) fn answered(modal: &QuestionModal) -> Vec<QuestionAnswer> {
    modal
        .drafts
        .iter()
        .filter_map(|draft| draft.confirmed.clone())
        .collect()
}

pub(super) fn enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, Modifiers::NONE)
}

#[test]
fn collapsing_keeps_hidden_input_inert_and_reopening_requires_paint() {
    let keys = KeyBindings::default();
    let theme = crate::presentation_style::Theme::default();
    let collapse = KeyEvent::new(KeyCode::Char('\x1d'), Modifiers::NONE);
    let batch = modal();
    let request = batch.request();
    let mut modal = QuestionModal::new(request, batch.questions.into_iter().take(1).collect());
    press(&mut modal, collapse, &keys);
    let frame = modal.render(80, 24, &keys, &theme);
    assert!(
        !frame
            .rows
            .iter()
            .any(|row| row.to_string().contains("Choose a route")),
        "collapsed question still exposes its choices"
    );
    assert!(frame.cursor.is_none());
    for key in [
        enter(),
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        KeyEvent::new(KeyCode::Char(' '), Modifiers::NONE),
        KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
        KeyEvent::new(KeyCode::Char('x'), Modifiers::NONE),
    ] {
        assert!(matches!(modal.key(key, &keys), QuestionAction::None));
    }
    assert!(matches!(modal.paste("hidden paste"), QuestionAction::None));
    modal.key(collapse, &keys);
    assert!(matches!(modal.key(enter(), &keys), QuestionAction::None));
    assert!(
        matches!(press(&mut modal, enter(), &keys), QuestionAction::Answer(id, answers)
        if id == request && answers == vec![QuestionAnswer::Option { index: 0 }])
    );
}

#[test]
fn unseen_or_clipped_choices_do_not_answer() {
    let keys = KeyBindings::default();
    let theme = crate::presentation_style::Theme::default();
    let mut modal = modal();
    modal.key(enter(), &keys);
    assert!(
        super::tests::answered(&modal).is_empty(),
        "an unseen choice was accepted"
    );
    modal.questions[0].options[0].label = "An option that spans several rows".into();
    modal.render(8, 1, &keys, &theme);
    modal.key(enter(), &keys);
    assert!(
        super::tests::answered(&modal).is_empty(),
        "a clipped choice was accepted"
    );
    modal.render(80, 24, &keys, &theme);
    modal.key(enter(), &keys);
    assert_eq!(
        super::tests::answered(&modal),
        vec![QuestionAnswer::Option { index: 0 }]
    );
}

#[test]
fn a_batch_can_revise_an_earlier_answer_before_one_final_submission() {
    let keys = KeyBindings::default();
    let mut modal = modal();
    assert!(matches!(
        press(&mut modal, enter(), &keys),
        QuestionAction::None
    ));
    assert!(
        matches!(press(&mut modal, enter(), &keys), QuestionAction::None),
        "the last answer must open review, not submit the batch"
    );
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Tab, Modifiers::NONE),
        &keys,
    );
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &keys,
    );
    press(&mut modal, enter(), &keys);
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Right, Modifiers::NONE),
        &keys,
    );
    assert!(
        matches!(press(&mut modal, enter(), &keys), QuestionAction::Answer(_, ref answers)
        if answers == &vec![QuestionAnswer::Option { index: 1 }, QuestionAnswer::Option { index: 0 }])
    );
}

#[test]
fn selection_advances_across_batch_and_text_row() {
    let mut modal = modal();
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &KeyBindings::default(),
    );
    assert!(matches!(
        press(&mut modal, enter(), &KeyBindings::default()),
        QuestionAction::None
    ));
    assert_eq!(
        super::tests::answered(&modal),
        vec![QuestionAnswer::Option { index: 1 }]
    );
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &KeyBindings::default(),
    );
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &KeyBindings::default(),
    );
    assert_eq!(modal.draft().selected, 2);
    assert!(matches!(
        press(&mut modal, enter(), &KeyBindings::default()),
        QuestionAction::None
    ));
    assert!(modal.editing);
    modal.paste("café 🦀");
    assert!(matches!(
        press(&mut modal, enter(), &KeyBindings::default()),
        QuestionAction::None
    ));
    assert!(
        matches!(press(&mut modal,enter(), &KeyBindings::default()), QuestionAction::Answer(_, ref answers) if answers.last() == Some(&QuestionAnswer::Text { text: "café 🦀".into() }))
    );
}

#[test]
fn multi_select_toggles_without_advancing_until_explicit_next() {
    let mut modal = modal();
    modal.questions[0].multi_select = true;
    let down = KeyEvent::new(KeyCode::Down, Modifiers::NONE);
    let space = KeyEvent::new(KeyCode::Char(' '), Modifiers::NONE);
    press(&mut modal, down, &KeyBindings::default());
    press(&mut modal, space, &KeyBindings::default()); // Beta first, output must still follow request order.
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Up, Modifiers::NONE),
        &KeyBindings::default(),
    );
    press(&mut modal, enter(), &KeyBindings::default());
    press(&mut modal, enter(), &KeyBindings::default()); // Undo Alpha, then select it again.
    press(&mut modal, space, &KeyBindings::default());
    assert!(super::tests::answered(&modal).is_empty());
    assert_eq!(modal.index, 0);
    let rows = modal
        .render(
            80,
            16,
            &KeyBindings::default(),
            &crate::presentation_style::Theme::default(),
        )
        .rows;
    assert!(rows.iter().any(|row| row.to_string().contains("[x] Alpha")));
    assert!(rows.iter().any(|row| row.to_string().contains("[x] Beta")));
    for _ in 0..3 {
        press(&mut modal, down, &KeyBindings::default());
    }
    assert_eq!(modal.draft().selected, 3);
    press(&mut modal, space, &KeyBindings::default()); // Next is not a checkable option.
    let rows = modal
        .render(
            24,
            2,
            &KeyBindings::default(),
            &crate::presentation_style::Theme::default(),
        )
        .rows;
    assert!(rows.iter().any(|row| row.to_string().contains("› Next")));
    assert!(matches!(
        press(&mut modal, enter(), &KeyBindings::default()),
        QuestionAction::None
    ));
    assert_eq!(
        super::tests::answered(&modal),
        vec![QuestionAnswer::Options {
            indices: vec![0, 1]
        }]
    );
    assert_eq!(modal.index, 1);
    assert!(modal.draft().checked.is_empty());
    assert!(matches!(
        press(&mut modal, enter(), &KeyBindings::default()),
        QuestionAction::None
    ));
    assert!(
        matches!(press(&mut modal,enter(), &KeyBindings::default()), QuestionAction::Answer(_, ref answers)
        if answers == &vec![QuestionAnswer::Options { indices: vec![0,1] }, QuestionAnswer::Option { index: 0 }])
    );
}

#[test]
fn multi_select_empty_is_explicit_and_custom_text_replaces_checkboxes() {
    for custom in [false, true] {
        let modal = modal();
        let mut questions = modal.questions;
        questions.truncate(1);
        let mut modal = QuestionModal::new(modal.request, questions);
        modal.questions[0].multi_select = true;
        if custom {
            press(&mut modal, enter(), &KeyBindings::default());
        }
        for _ in 0..2 {
            press(
                &mut modal,
                KeyEvent::new(KeyCode::Down, Modifiers::NONE),
                &KeyBindings::default(),
            );
        }
        let expected = if custom {
            press(&mut modal, enter(), &KeyBindings::default());
            modal.paste("  different 🦀  ");
            QuestionAnswer::Text {
                text: "  different 🦀  ".into(),
            }
        } else {
            press(
                &mut modal,
                KeyEvent::new(KeyCode::Down, Modifiers::NONE),
                &KeyBindings::default(),
            );
            QuestionAnswer::Options { indices: vec![] }
        };
        assert!(
            matches!(press(&mut modal,enter(), &KeyBindings::default()), QuestionAction::Answer(_, ref answers) if answers == &vec![expected])
        );
        assert!(modal.draft().checked.is_empty());
    }
}

#[test]
fn custom_editor_is_bounded_and_preserves_exact_unicode() {
    let mut modal = modal();
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &KeyBindings::default(),
    );
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &KeyBindings::default(),
    );
    press(&mut modal, enter(), &KeyBindings::default());
    modal.paste("  ");
    assert!(matches!(
        press(&mut modal, enter(), &KeyBindings::default()),
        QuestionAction::None
    ));
    assert!(modal.status.contains("must not be empty"));
    modal.paste("  spaced ✓  ");
    let before = modal.draft().text.text.clone();
    modal.paste(&"x".repeat(MAX_ANSWER_BYTES));
    assert_eq!(modal.draft().text.text, before);
    assert!(modal.status.contains("limited"));
    assert!(matches!(
        press(&mut modal, enter(), &KeyBindings::default()),
        QuestionAction::None
    ));
    assert_eq!(
        super::tests::answered(&modal)[0],
        QuestionAnswer::Text {
            text: "    spaced ✓  ".into()
        }
    );
}

#[test]
fn tiny_width_disables_submission_and_never_claims_invisible_selection() {
    for (width, multi_select) in [(1, false), (2, false), (1, true), (2, true)] {
        let mut modal = modal();
        modal.questions[0].multi_select = multi_select;
        let frame = modal.render(
            width,
            1,
            &KeyBindings::default(),
            &crate::presentation_style::Theme::default(),
        );
        assert_eq!(frame.rows.len(), 1);
        assert!(frame.cursor.is_none());
        assert!(!frame.rows[0].to_string().is_empty());
        assert!(matches!(
            modal.key(enter(), &KeyBindings::default()),
            QuestionAction::None
        ));
        assert!(super::tests::answered(&modal).is_empty());
        modal.paste("invisible");
        assert!(super::tests::answered(&modal).is_empty());
    }
    let mut modal = modal();
    modal.render(
        20,
        1,
        &KeyBindings::default(),
        &crate::presentation_style::Theme::default(),
    );
    assert!(!modal.too_small);
    assert!(matches!(
        modal.key(enter(), &KeyBindings::default()),
        QuestionAction::None
    ));
    assert_eq!(
        super::tests::answered(&modal),
        vec![QuestionAnswer::Option { index: 0 }]
    );
}

#[test]
fn one_row_editor_keeps_cursor_and_text_visible_at_narrow_geometry() {
    let mut modal = modal();
    modal.render(
        20,
        1,
        &KeyBindings::default(),
        &crate::presentation_style::Theme::default(),
    );
    modal.key(
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &KeyBindings::default(),
    );
    modal.key(
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &KeyBindings::default(),
    );
    modal.render(
        20,
        1,
        &KeyBindings::default(),
        &crate::presentation_style::Theme::default(),
    );
    modal.key(enter(), &KeyBindings::default());
    modal.paste("x");
    let frame = modal.render(
        8,
        1,
        &KeyBindings::default(),
        &crate::presentation_style::Theme::default(),
    );
    assert!(frame.cursor.is_some());
    let (row, _) = frame.cursor.unwrap();
    assert!(frame.rows[row].to_string().contains('x'));
    assert!(!modal.too_small);
}

#[test]
fn short_viewport_keeps_selected_row_and_editor_cursor_visible() {
    let mut modal = modal();
    let frame = modal.render(
        30,
        4,
        &KeyBindings::default(),
        &crate::presentation_style::Theme::default(),
    );
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.to_string().contains("Alpha"))
    );
    modal.key(
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &KeyBindings::default(),
    );
    let frame = modal.render(
        30,
        3,
        &KeyBindings::default(),
        &crate::presentation_style::Theme::default(),
    );
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.to_string().contains("› Beta")),
        "{:?}",
        frame
            .rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    modal.key(
        KeyEvent::new(KeyCode::Down, Modifiers::NONE),
        &KeyBindings::default(),
    );
    modal.render(
        30,
        3,
        &KeyBindings::default(),
        &crate::presentation_style::Theme::default(),
    );
    modal.key(enter(), &KeyBindings::default());
    modal.paste("cursor");
    let frame = modal.render(
        30,
        3,
        &KeyBindings::default(),
        &crate::presentation_style::Theme::default(),
    );
    assert!(frame.cursor.is_some());
    assert!(frame.cursor.unwrap().0 < frame.rows.len());
}
