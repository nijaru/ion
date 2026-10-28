//! Native editing changes a nonce-bound draft, never answer authority.
use super::tests::{enter, modal, press};
use super::*;

fn editing_modal() -> QuestionModal {
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
    modal.paste("café 🦀");
    modal
}

#[test]
fn external_editor_binding_only_opens_from_expanded_custom_text() {
    let external = KeyEvent::new(KeyCode::Char('g'), Modifiers::CONTROL);
    let keys = KeyBindings::default();
    assert_eq!(keys.action(TEXT, external), Some(KeyAction::External));
    let mut choice = modal();
    assert!(matches!(
        press(&mut choice, external, &keys),
        QuestionAction::None
    ));
    choice.page = Page::Review;
    assert!(matches!(
        press(&mut choice, external, &keys),
        QuestionAction::None
    ));

    let mut modal = editing_modal();
    assert!(matches!(press(&mut modal, external, &keys),
        QuestionAction::Edit { request, index: 0, text }
        if request == modal.request() && text == "café 🦀"));
    let collapse = KeyEvent::new(KeyCode::Char(']'), Modifiers::CONTROL);
    press(&mut modal, collapse, &keys);
    assert!(matches!(
        press(&mut modal, external, &keys),
        QuestionAction::None
    ));
    press(&mut modal, collapse, &keys);

    let remapped = KeyBindings::parse(br#"{"app.editor.external":"alt+g"}"#).unwrap();
    assert!(matches!(
        press(&mut modal, external, &remapped),
        QuestionAction::None
    ));
    assert!(matches!(
        press(
            &mut modal,
            KeyEvent::new(KeyCode::Char('g'), Modifiers::ALT),
            &remapped
        ),
        QuestionAction::Edit { .. }
    ));
    let disabled = KeyBindings::parse(br#"{"app.editor.external":[]}"#).unwrap();
    assert!(matches!(
        press(&mut modal, external, &disabled),
        QuestionAction::None
    ));
}

#[test]
fn editor_replacement_requires_new_paint_and_explicit_confirmation() {
    let mut modal = editing_modal();
    let keys = KeyBindings::default();
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Left, Modifiers::NONE),
        &keys,
    );
    let request = modal.request();
    modal.finish_external_edit(request, 0, Ok("  changed\r\n🦀\u{1b}  ".into()));
    assert!(modal.editing);
    assert_eq!(modal.draft().text.text, "  changed\n🦀  ");
    assert_eq!(modal.draft().text.cursor, modal.draft().text.text.len());
    assert!(modal.draft().confirmed.is_none());
    assert!(matches!(modal.key(enter(), &keys), QuestionAction::None));
    assert!(
        matches!(press(&mut modal, enter(), &keys), QuestionAction::Answer(id, answers)
        if id == request && answers == vec![QuestionAnswer::Text { text: "  changed\n🦀  ".into() }])
    );
}

#[test]
fn editor_failure_and_stale_results_preserve_the_original_target() {
    let mut modal = modal();
    let keys = KeyBindings::default();
    press(&mut modal, enter(), &keys); // Retain the first answer during editing.
    for _ in 0..2 {
        press(
            &mut modal,
            KeyEvent::new(KeyCode::Down, Modifiers::NONE),
            &keys,
        );
    }
    press(&mut modal, enter(), &keys);
    modal.paste("café 🦀");
    press(
        &mut modal,
        KeyEvent::new(KeyCode::Left, Modifiers::NONE),
        &keys,
    );
    let request = modal.request();
    let cursor = modal.draft().text.cursor;
    modal.finish_external_edit(
        request,
        1,
        Err(anyhow::anyhow!("editor exited with status 7")),
    );
    assert_eq!(modal.draft().text.text, "café 🦀");
    assert_eq!(modal.draft().text.cursor, cursor);
    assert!(modal.status.contains("original draft retained"));
    let status = modal.status.clone();
    for (id, index) in [(QuestionRequestId(uuid::Uuid::now_v7()), 1), (request, 0)] {
        modal.finish_external_edit(id, index, Ok("wrong target".into()));
        assert_eq!(modal.draft().text.text, "café 🦀");
        assert_eq!(modal.draft().text.cursor, cursor);
        assert_eq!(modal.status, status);
    }
    modal.finish_external_edit(request, 1, Ok("replacement".into()));
    assert_eq!(
        modal.drafts[0].confirmed,
        Some(QuestionAnswer::Option { index: 0 })
    );
    assert!(modal.drafts[1].confirmed.is_none());
    assert_eq!(modal.drafts[1].text.text, "replacement");
}
