use super::*;
use crate::presentation_style::{Role, Theme};
use ion_host::QuestionOption;
use ratatui::style::{Color, Modifier};

fn comparison() -> QuestionModal {
    let lines = (0..50)
        .map(|n| format!("preview-line-{n}\n"))
        .collect::<String>();
    QuestionModal::new(
        QuestionRequestId(uuid::Uuid::now_v7()),
        vec![Question {
            question: "Which implementation?".into(),
            header: Some("Compare".into()),
            multi_select: false,
            options: vec![
                QuestionOption {
                    label: "Alpha".into(),
                    description: Some("First design".into()),
                    preview: Some(format!(
                        "# Sample\n\n```rust\nlet marker = \"CHOICE_A\";\n```\n\n```text\n{lines}```"
                    )),
                },
                QuestionOption {
                    label: "Beta".into(),
                    description: None,
                    preview: Some("**CHOICE_B**\n\n```json\n{\"enabled\": false}\n```".into()),
                },
            ],
        }],
    )
}

fn text(frame: &super::render::QuestionFrame) -> String {
    frame
        .rows
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn previews_follow_focus_page_and_geometry_without_answering() {
    let keys = KeyBindings::default();
    let mut theme = Theme::default();
    let color = Color::Rgb(12, 34, 56);
    theme.set_color(Role::SyntaxString, color);
    let mut modal = comparison();
    let frame = modal.render(120, 20, &keys, &theme);
    assert!(text(&frame).contains("Compare"));
    assert!(text(&frame).contains("CHOICE_A"));
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.to_string().contains("Alpha") && row.to_string().contains('│')),
        "wide comparisons must put options beside the preview"
    );
    let string = frame
        .rows
        .iter()
        .flat_map(|row| &row.spans)
        .find(|span| span.content.contains("CHOICE_A"))
        .unwrap();
    assert_eq!(string.style.fg, Some(color));
    assert!(
        !string.style.add_modifier.contains(Modifier::REVERSED),
        "left selection styling leaked into the preview"
    );
    for _ in 0..20 {
        modal.key(KeyEvent::new(KeyCode::PageDown, Modifiers::NONE), &keys);
        modal.render(120, 20, &keys, &theme);
    }
    let frame = modal.render(120, 20, &keys, &theme);
    assert!(text(&frame).contains("preview-line-49"));
    assert!(!text(&frame).contains("CHOICE_A"));
    assert!(modal.answers.is_empty());
    modal.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), &keys);
    let frame = modal.render(60, 18, &keys, &theme);
    assert!(text(&frame).contains("CHOICE_B"));
    let selected = frame
        .rows
        .iter()
        .position(|row| row.to_string().contains("› Beta"))
        .unwrap();
    let border = frame
        .rows
        .iter()
        .position(|row| row.to_string().contains('┌'))
        .unwrap();
    assert!(
        selected < border,
        "narrow comparisons must stack preview below choices"
    );
    assert_eq!(modal.preview_offset, 0);
    modal.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), &keys);
    modal.render(120, 18, &keys, &theme);
    modal.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &keys);
    let answer = "custom ".repeat(10);
    modal.paste(&answer);
    let frame = modal.render(120, 18, &keys, &theme);
    assert!(!text(&frame).contains("Preview"));
    assert!(
        frame
            .rows
            .iter()
            .any(|row| row.to_string().contains(&answer)),
        "custom answer did not regain the full width"
    );
    assert!(
        matches!(modal.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &keys),
        QuestionAction::Answer(_, ref answers) if answers == &vec![QuestionAnswer::Text {text: answer}])
    );
}

#[test]
fn preview_bounds_control_safety_and_resize_visibility_are_explicit() {
    let keys = KeyBindings::default();
    let theme = Theme::default();
    for width in [1, 2, 8, 30, 99, 100, 180] {
        for height in [1, 2, 3, 8, 24] {
            let mut modal = comparison();
            modal.questions[0].options[0].preview =
                Some("# Config\n\n```text\n\u{1b}]52;c;UNTRUSTED\u{7}\n界🦀 e\u{301}\n```".into());
            let frame = modal.render(width, height, &keys, &theme);
            assert_eq!(frame.rows.len(), usize::from(height));
            for row in &frame.rows {
                assert!(
                    row.width() <= usize::from(width),
                    "overflow at {width}×{height}: {row:?}"
                );
                assert!(!row.to_string().contains('\u{1b}'));
            }
            if height < 3 || width < 8 {
                assert!(matches!(
                    modal.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &keys),
                    QuestionAction::None
                ));
                assert!(modal.answers.is_empty());
            }
        }
    }
    let mut modal = comparison();
    modal.render(120, 24, &keys, &theme);
    modal.invalidate_visibility();
    assert!(matches!(
        modal.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &keys),
        QuestionAction::None
    ));
    modal.render(120, 24, &keys, &theme);
    assert!(
        matches!(modal.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &keys), QuestionAction::Answer(_, ref answers)
        if answers == &vec![QuestionAnswer::Option {index: 0}])
    );
}

#[test]
fn an_option_without_preview_remains_selectable_in_a_rich_question() {
    let keys = KeyBindings::default();
    let theme = Theme::default();
    let mut modal = comparison();
    modal.questions[0].options[1].preview = None;
    modal.key(KeyEvent::new(KeyCode::Down, Modifiers::NONE), &keys);
    let frame = modal.render(8, 8, &keys, &theme);
    assert!(frame.rows.iter().all(|row| row.width() <= 8));
    modal.render(80, 24, &keys, &theme);
    assert!(
        matches!(modal.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), &keys), QuestionAction::Answer(_, ref answers)
        if answers == &vec![QuestionAnswer::Option {index: 1}])
    );
}
