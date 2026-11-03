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
fn mixed_line_endings_custom_paste_retains_breaks() {
    let mut modal = custom("", 0);
    modal.paste("first α\rsecond 👩‍💻\r\n\tthird e\u{301}\nlast 終\r\0\u{7}");
    let expected = "first α\nsecond 👩‍💻\n\tthird e\u{301}\nlast 終\n";
    assert_eq!(modal.draft().text.text, expected);
    assert_eq!(modal.draft().text.cursor, expected.len());
    let QuestionAction::Answer(_, answers) = press(&mut modal, enter(), &KeyBindings::default())
    else {
        panic!("custom text was not confirmed")
    };
    assert_eq!(
        answers,
        vec![QuestionAnswer::Text {
            text: expected.into()
        }]
    );
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
fn custom_editor_shows_complete_failure_feedback_at_common_widths() {
    let keys = KeyBindings::default();
    let status = "Editor failed: editor exited with exit status: 7; original draft retained";
    for width in [40, 80] {
        let mut modal = custom("draft", 5);
        modal.set_status(status.into());
        let frame = modal.painted_render(width, 24, &keys, &Theme::default());
        let painted: String = frame.rows.iter().map(ToString::to_string).collect();
        assert!(
            painted.contains(status),
            "clipped feedback at width {width}: {painted}"
        );
        assert!(frame.cursor.is_some());
        assert_eq!(modal.draft().text.text, "draft");
    }
}

#[test]
fn custom_editor_keeps_multiline_draft_and_editing_hints_visible() {
    let keys = KeyBindings::default();
    let source = "first\nsecond\nthird\nfourth\nfifth\nsixth\nseventh\neighth";
    for width in [40, 80] {
        let mut modal = custom(source, source.len());
        let frame = modal.painted_render(width, 24, &keys, &Theme::default());
        let rows: Vec<_> = frame.rows.iter().map(ToString::to_string).collect();
        for word in source.lines() {
            assert!(
                rows.iter().any(|line| line.ends_with(word)),
                "hidden editor row: {word}"
            );
        }
        for (action, description) in [
            (KeyAction::Submit, "answer"),
            (KeyAction::NewLine, "newline"),
            (KeyAction::External, "editor"),
            (KeyAction::Cancel, "cancel"),
            (KeyAction::QuestionCollapse, "hide"),
            (KeyAction::Interrupt, "stops Turn"),
        ] {
            let hint = format!("{} {description}", keys.hint(action));
            assert!(
                rows.iter().any(|line| line.contains(&hint)),
                "hidden control at width {width}: {hint}"
            );
        }
        assert!(frame.cursor.is_some());
        assert!(frame.rows.len() < 24);
        assert_eq!(modal.draft().text.text, source);
    }
}
