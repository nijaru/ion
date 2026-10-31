use super::tests::{enter, modal, press};
use super::*;
use crate::presentation_style::Theme;

fn custom(source: &str, cursor: usize) -> QuestionModal {
    let batch = modal();
    let mut modal = QuestionModal::new(
        batch.request(),
        batch.questions.into_iter().take(1).collect(),
    );
    let keys = KeyBindings::default();
    for _ in 0..2 {
        press(
            &mut modal,
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &keys,
        );
    }
    press(&mut modal, enter(), &keys);
    modal.paste(source);
    modal.draft_mut().text.cursor = cursor;
    modal
}

#[test]
fn custom_vertical_movement_uses_only_the_actual_painted_visual_width() {
    let keys = KeyBindings::default();
    let theme = Theme::default();
    let mut modal = custom("one two three", 4);
    let layout_only = modal.render(12, 24, &keys, &theme);
    assert_eq!(modal.draft().text.width, None);
    modal.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), &keys);
    assert_eq!(modal.draft().text.cursor, 4);
    modal.observe_paint(layout_only.candidate, layout_only.editor_width);
    assert_eq!(
        modal.draft().text.width,
        None,
        "a cursor action invalidates the prepared editor"
    );
    modal.painted_render(12, 24, &keys, &theme);
    assert_eq!(modal.draft().text.width, Some(10));
    modal.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), &keys);
    assert_eq!(modal.draft().text.cursor, 12);
    modal.painted_render(12, 24, &keys, &theme);
    modal.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), &keys);
    assert_eq!(modal.draft().text.cursor, 4);
    assert_eq!(modal.draft().text.text, "one two three");
}

#[test]
fn custom_sticky_columns_survive_repaint_and_reset_on_width_or_nonvertical_edit() {
    let keys = KeyBindings::default();
    let theme = Theme::default();
    let source = "abcdef\n界\nabcdef";
    let mut modal = custom(source, 5);
    modal.painted_render(24, 24, &keys, &theme);
    modal.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), &keys);
    assert_eq!(modal.draft().text.cursor, "abcdef\n界".len());
    assert_eq!(modal.draft().text.preferred_column, Some(5));
    modal.painted_render(24, 24, &keys, &theme);
    modal.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), &keys);
    assert_eq!(modal.draft().text.cursor, source.len() - 1);
    modal.painted_render(23, 24, &keys, &theme);
    assert_eq!(modal.draft().text.preferred_column, None);
    modal.key(KeyEvent::new(KeyCode::Up, Modifiers::NONE), &keys);
    assert_eq!(modal.draft().text.preferred_column, Some(5));
    modal.key(KeyEvent::new(KeyCode::Left, Modifiers::NONE), &keys);
    assert_eq!(modal.draft().text.preferred_column, None);
    assert_eq!(modal.draft().text.text, source);
}

#[test]
fn custom_editor_grows_with_the_question_frame_instead_of_a_four_row_cap() {
    let keys = KeyBindings::default();
    let source = "first\nsecond\nthird\nfourth\nfifth\nsixth\nseventh\neighth";
    let mut modal = custom(source, source.len());
    let frame = modal.painted_render(40, 24, &keys, &Theme::default());
    for word in source.lines() {
        assert!(
            frame
                .rows
                .iter()
                .any(|line| line.to_string().ends_with(word)),
            "hidden editor row: {word}"
        );
    }
    assert!(frame.cursor.is_some());
    assert!(frame.rows.len() < 24);
    assert_eq!(modal.draft().text.text, source);
}
