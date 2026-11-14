//! Ephemeral binding identity and cached Core planning chrome; no render-time I/O.
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
    let settled = !ui.session_io.blocked && ui.session_io.binding.is_none();
    if budget > 1 {
        let context = ui
            .context_estimate
            .as_ref()
            .filter(|_| settled && ui.conversation.live().is_none())
            .and_then(|estimate| match estimate {
                Ok(report) if &report.model == model && report.turn.is_none() => Some(format!(
                    "context ~{}/{}",
                    report.estimated_input_tokens,
                    report
                        .context_window_tokens
                        .map_or_else(|| "?".into(), |n| n.to_string()),
                )),
                Ok(_) => None,
                Err(_) => Some("context unavailable".into()),
            })
            // Do not truncate an estimate into a misleading partial denominator
            // or let it displace all Session/workspace orientation on narrow screens.
            .filter(|text| UnicodeWidthStr::width(text.as_str()) <= width / 2);
        let location_width =
            width.saturating_sub(context.as_ref().map_or(0, |text| text.len() + 3));
        // Keep the Session recognizable even when its workspace path is long.
        let session = fit_line(&ui.session_label, location_width / 2);
        let remaining = location_width.saturating_sub(UnicodeWidthStr::width(session.as_str()) + 3);
        let location = format!(
            "{} · {session}",
            fit_line(&ui.cwd.to_string_lossy(), remaining)
        );
        let location = match context {
            Some(context) => format!("{location} · {context}"),
            None => location,
        };
        rows.push(Line::styled(
            fit_line(&location, width),
            ui.themes
                .theme()
                .style(crate::presentation_style::Role::Secondary),
        ));
    }
    // Binding completion requires a fresh History before its preference can
    // accompany the current model. Painting never locks Session storage.
    let reasoning = ui.reasoning_preference.filter(|_| settled);
    let identity = match reasoning {
        Some(reasoning) => format!(
            "{}/{} · reasoning {reasoning} · /help",
            model.provider, model.model
        ),
        None => format!("{}/{} · /help", model.provider, model.model),
    };
    rows.push(Line::styled(
        fit_line(&identity, width),
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
    fn context_chrome_is_an_estimate_and_never_truncates_its_denominator() {
        let model = ModelRef {
            provider: "provider".into(),
            model: "model".into(),
        };
        let report = ion_core::ContextReport {
            model: model.clone(),
            revision: 5,
            turn: None,
            context_window_tokens: Some(5000),
            estimated_input_tokens: 1220,
            basis: ion_core::ContextEstimateBasis::Calibrated,
            retained_request_bytes: 6000,
            max_request_bytes: 10000,
            capacity: ion_core::ContextCapacity::Available {
                max_output_tokens: 1000,
            },
        };
        let mut ui = Frontend {
            session_label: "Session 12345678".into(),
            cwd: "/workspace".into(),
            context_estimate: Some(Ok(report.clone())),
            ..Frontend::default()
        };
        let footer = |ui: &Frontend, width| rows(ui, Some(&model), width, 2)[0].to_string();
        assert!(footer(&ui, 80).contains("context ~1220/5000"));
        assert!(footer(&ui, 80).contains("Session 12345678"));
        for width in 1..100 {
            let line = footer(&ui, width);
            assert!(UnicodeWidthStr::width(line.as_str()) <= width);
            assert!(!line.contains("context") || line.contains("context ~1220/5000"));
        }
        ui.session_io.blocked = true;
        assert!(!footer(&ui, 80).contains("context"));
        ui.session_io.blocked = false;
        let mut active = report.clone();
        active.turn = Some(1);
        ui.context_estimate = Some(Ok(active));
        assert!(!footer(&ui, 80).contains("context"));
        let mut other = report.clone();
        other.model.model = "other".into();
        ui.context_estimate = Some(Ok(other));
        assert!(!footer(&ui, 80).contains("context"));
        let mut unknown = report;
        unknown.context_window_tokens = None;
        ui.context_estimate = Some(Ok(unknown));
        assert!(footer(&ui, 80).contains("context ~1220/?"));
        ui.context_estimate = Some(Err(ion_core::AgentError::ImagesUnsupported));
        assert!(footer(&ui, 80).contains("context unavailable"));
    }

    #[test]
    fn identity_chrome_is_control_safe_bounded_and_does_not_change_the_draft() {
        let mut ui = Frontend {
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
        for (preference, label) in [
            (None, ""),
            (
                Some(ion_ai::Reasoning::ProviderDefault),
                " · reasoning default",
            ),
            (Some(ion_ai::Reasoning::Off), " · reasoning off"),
            (Some(ion_ai::Reasoning::Low), " · reasoning low"),
            (Some(ion_ai::Reasoning::Medium), " · reasoning medium"),
            (Some(ion_ai::Reasoning::High), " · reasoning high"),
            (
                Some(ion_ai::Reasoning::BudgetTokens(u32::MAX)),
                " · reasoning budget:4294967295",
            ),
        ] {
            ui.reasoning_preference = preference;
            assert_eq!(
                rows(&ui, Some(&model), 100, 1)[0].to_string(),
                format!("provider/vendor/model{label} · /help")
            );
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
        }
        ui.session_io.blocked = true;
        assert_eq!(
            rows(&ui, Some(&model), 100, 1)[0].to_string(),
            "provider/vendor/model · /help"
        );
        ui.session_io.blocked = false;
        ui.reasoning_preference = None;
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
