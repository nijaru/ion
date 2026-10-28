use super::{
    tests::{enter, modal, press},
    *,
};
use crate::presentation_style::Theme;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, Modifiers::NONE)
}

#[test]
fn review_requires_confirmation_and_preserves_unsent_checkbox_edits() {
    let keys = KeyBindings::default();
    let mut modal = modal();
    modal.questions[0].multi_select = true;
    // Go straight to review: it must not manufacture answers from row zero.
    press(&mut modal, key(KeyCode::Left), &keys);
    assert!(matches!(
        press(&mut modal, enter(), &keys),
        QuestionAction::None
    ));
    assert!(modal.status.contains("every question"));
    press(&mut modal, key(KeyCode::Tab), &keys);
    for _ in 0..3 {
        press(&mut modal, key(KeyCode::Down), &keys);
    }
    press(&mut modal, enter(), &keys); // Explicit none, unlike unanswered.
    press(&mut modal, enter(), &keys); // Question 2.
    press(&mut modal, key(KeyCode::Tab), &keys); // Revisit multi-select, still on Next.
    for _ in 0..3 {
        press(&mut modal, key(KeyCode::Up), &keys);
    }
    press(&mut modal, key(KeyCode::Char(' ')), &keys);
    press(&mut modal, key(KeyCode::Left), &keys); // Review without Next.
    assert!(matches!(
        press(&mut modal, enter(), &keys),
        QuestionAction::None
    ));
    press(&mut modal, key(KeyCode::Tab), &keys);
    assert_eq!(
        modal.draft().checked,
        vec![0],
        "unsent edits were discarded on tab entry"
    );
    for _ in 0..3 {
        press(&mut modal, key(KeyCode::Down), &keys);
    }
    press(&mut modal, enter(), &keys);
    press(&mut modal, key(KeyCode::Right), &keys);
    assert!(
        matches!(press(&mut modal, enter(), &keys), QuestionAction::Answer(_, ref answers)
        if answers == &vec![QuestionAnswer::Options { indices: vec![0] }, QuestionAnswer::Option {index: 0}])
    );
}

#[test]
fn custom_text_survives_revisits_and_text_keys_do_not_switch_tabs() {
    let keys = KeyBindings::default();
    let mut modal = modal();
    for _ in 0..2 {
        press(&mut modal, key(KeyCode::Down), &keys);
    }
    press(&mut modal, enter(), &keys);
    modal.paste("café🦀");
    press(&mut modal, key(KeyCode::Tab), &keys);
    assert_eq!(modal.index, 0);
    assert!(modal.editing);
    press(&mut modal, key(KeyCode::Left), &keys);
    modal.paste("!");
    press(&mut modal, enter(), &keys);
    press(&mut modal, enter(), &keys);
    press(&mut modal, key(KeyCode::Tab), &keys);
    assert!(
        !modal.editing,
        "tab entry must not hijack navigation with an editor"
    );
    assert_eq!(modal.draft().selected, 2);
    press(&mut modal, enter(), &keys);
    assert_eq!(modal.draft().text.text, "café🦀!\t");
    press(&mut modal, key(KeyCode::Backspace), &keys);
    press(&mut modal, enter(), &keys);
    press(&mut modal, key(KeyCode::Right), &keys);
    assert!(
        matches!(press(&mut modal, enter(), &keys), QuestionAction::Answer(_, ref answers)
        if answers == &vec![QuestionAnswer::Text {text: "café🦀\t".into()}, QuestionAnswer::Option {index: 0}])
    );
}

#[test]
fn review_paint_resize_navigation_and_remapped_keys_keep_authority_scoped() {
    let keys =
        KeyBindings::parse(br#"{"tui.question.next":"alt+n","tui.question.previous":"alt+p"}"#)
            .unwrap();
    let theme = Theme::default();
    let mut modal = modal();
    press(&mut modal, enter(), &keys);
    press(&mut modal, enter(), &keys);
    // BackTab is normalized by the registry and wraps through the review tab.
    let defaults = KeyBindings::default();
    press(
        &mut modal,
        KeyEvent::new(KeyCode::BackTab, Modifiers::SHIFT),
        &defaults,
    );
    assert!(modal.page == Page::Question && modal.index == 1);
    press(&mut modal, key(KeyCode::Right), &defaults);
    // Returning to review revoked its previous paint.
    assert!(matches!(modal.key(enter(), &keys), QuestionAction::None));
    for (width, height) in [(1, 1), (8, 2), (15, 20), (80, 4)] {
        let frame = modal.render(width, height, &keys, &theme);
        assert!(
            frame
                .rows
                .iter()
                .all(|row| row.width() <= usize::from(width))
        );
        assert!(matches!(modal.key(enter(), &keys), QuestionAction::None));
    }
    modal.render(80, 24, &keys, &theme);
    modal.invalidate_visibility(); // Rejected publication/resize before the next key.
    assert!(matches!(modal.key(enter(), &keys), QuestionAction::None));
    press(&mut modal, key(KeyCode::Tab), &keys); // Remapped default must be inactive.
    assert!(modal.page == Page::Review);
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Char('n'), Modifiers::ALT),
        &keys,
    );
    assert!(modal.page == Page::Question);
    assert!(matches!(modal.key(enter(), &keys), QuestionAction::None));
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Char('p'), Modifiers::ALT),
        &keys,
    );
    assert!(matches!(
        press(&mut modal, enter(), &keys),
        QuestionAction::Answer(_, _)
    ));
    assert!(
        matches!(modal.key(enter(), &keys), QuestionAction::None),
        "one paint authorized two submissions"
    );
}

#[test]
fn review_safely_pages_long_answers_and_cancel_is_not_a_partial_answer() {
    let keys = KeyBindings::default();
    let theme = Theme::default();
    let mut modal = modal();
    modal.questions[0].options[0].label = "line\n".repeat(40) + "\u{1b}]52;c;untrusted\u{7}tail";
    // Long choice must be confirmed in a viewport showing the whole label.
    modal.render(80, 80, &keys, &theme);
    modal.key(enter(), &keys);
    press(&mut modal, enter(), &keys);
    let first = modal.render(40, 10, &keys, &theme);
    assert!(
        first
            .rows
            .iter()
            .any(|row| row.to_string().contains("Submit answers"))
    );
    for _ in 0..10 {
        press(&mut modal, key(KeyCode::PageDown), &keys);
    }
    let frame = modal.render(40, 10, &keys, &theme);
    assert!(
        frame
            .rows
            .iter()
            .all(|row| !row.to_string().contains('\u{1b}') && row.width() <= 40)
    );
    assert!(modal.review_offset > 0);
    assert!(matches!(
        press(&mut modal, key(KeyCode::Esc), &keys),
        QuestionAction::Cancel(_)
    ));
    press(&mut modal, key(KeyCode::Down), &keys);
    assert!(matches!(
        press(&mut modal, enter(), &keys),
        QuestionAction::Cancel(_)
    ));
}
