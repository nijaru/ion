//! Ephemeral identity chrome, derived from the current binding rather than Turn facts.
use ion_ai::ModelRef;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use super::Frontend;
use crate::display_text::fit_line;

pub(super) fn rows(
    ui: &Frontend,
    model: Option<&ModelRef>,
    width: usize,
    budget: usize,
) -> Vec<Line<'static>> {
    let Some(model) = model.filter(|_| budget > 0) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    if budget > 1 {
        // Keep the Session recognizable even when its workspace path is long.
        let session = fit_line(&ui.session_label, width / 2);
        let remaining = width.saturating_sub(UnicodeWidthStr::width(session.as_str()) + 3);
        rows.push(Line::styled(
            fit_line(
                &format!(
                    "{} · {session}",
                    fit_line(&ui.cwd.to_string_lossy(), remaining)
                ),
                width,
            ),
            ui.themes
                .theme()
                .style(crate::presentation_style::Role::Secondary),
        ));
    }
    rows.push(Line::styled(
        fit_line(
            &format!("{}/{} · /help", model.provider, model.model),
            width,
        ),
        ui.themes
            .theme()
            .style(crate::presentation_style::Role::Secondary),
    ));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_client::Editor;

    #[test]
    fn identity_chrome_is_control_safe_bounded_and_does_not_change_the_draft() {
        let ui = Frontend {
            editor: Editor {
                draft: "unsent 🦀".into(),
                cursor: 3,
                ..Editor::default()
            },
            cwd: "/long/workspace/🦀\u{1b}[2J".into(),
            session_label: "named\nsession".into(),
            ..Frontend::default()
        };
        let model = ModelRef {
            provider: "provider".into(),
            model: "vendor/model".into(),
        };
        for width in [1, 2, 12, 40, 100] {
            for budget in 0..=3 {
                let rows = rows(&ui, Some(&model), width, budget);
                assert_eq!(rows.len(), budget.min(2));
                for row in rows {
                    assert!(row.width() <= width);
                    let text = row.to_string();
                    assert!(!text.contains(['\u{1b}', '\n', '\r']));
                }
            }
        }
        assert!(
            rows(&ui, None, 100, 2).is_empty(),
            "setup has no bound Session"
        );
        let one = rows(&ui, Some(&model), 100, 1);
        assert_eq!(one[0].to_string(), "provider/vendor/model · /help");
        let two = rows(&ui, Some(&model), 100, 2);
        assert!(two[0].to_string().contains("named session"));
        assert_eq!(
            (ui.editor.draft.as_str(), ui.editor.cursor),
            ("unsent 🦀", 3)
        );
    }
}
