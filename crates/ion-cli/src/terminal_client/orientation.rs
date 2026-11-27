//! Ephemeral binding identity and cached Core planning chrome; no render-time I/O.
use ion_ai::ModelRef;
use ratatui::text::Line;
use unicode_width::UnicodeWidthStr;

use super::Frontend;

#[derive(Clone, Copy, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Field {
    Model,
    Reasoning,
    Context,
    Workspace,
    Session,
    Help,
    InputTokens,
    OutputTokens,
    CacheRead,
    CacheWrite,
    ProviderBalance,
    ProviderKeyUsage,
}

#[derive(serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Footer {
    primary: Vec<Field>,
    secondary: Vec<Field>,
}

impl Default for Footer {
    fn default() -> Self {
        Self {
            primary: vec![Field::Model, Field::Reasoning, Field::Help],
            secondary: vec![
                Field::Session,
                Field::Context,
                Field::InputTokens,
                Field::OutputTokens,
                Field::ProviderBalance,
                Field::ProviderKeyUsage,
                Field::Workspace,
            ],
        }
    }
}

impl Footer {
    pub(super) fn shows_context_estimate(&self) -> bool {
        self.primary.contains(&Field::Context) || self.secondary.contains(&Field::Context)
    }

    pub(super) fn shows_provider_usage(&self, kind: ion_app::ProviderUsageKind) -> bool {
        let field = match kind {
            ion_app::ProviderUsageKind::DeepSeekBalance => Field::ProviderBalance,
            ion_app::ProviderUsageKind::OpenRouterKey => Field::ProviderKeyUsage,
        };
        self.primary.contains(&field) || self.secondary.contains(&field)
    }

    pub(super) fn load(root: &std::path::Path) -> anyhow::Result<Self> {
        let footer: Self = ion_app::user_settings::UserSettings::load(root)?.footer()?;
        let mut seen = Vec::new();
        for field in footer.primary.iter().chain(&footer.secondary) {
            anyhow::ensure!(!seen.contains(field), "footer fields must not be repeated");
            seen.push(*field);
        }
        Ok(footer)
    }
}

fn provider_usage_text(field: Field, reading: &ion_app::ProviderUsageReading) -> Option<String> {
    use ion_app::ProviderUsageReading;
    match (field, reading) {
        (Field::ProviderBalance, ProviderUsageReading::Balance(balance)) => {
            Some(balance_text(balance))
        }
        (Field::ProviderKeyUsage, ProviderUsageReading::KeyCapRemaining(amount)) => Some(
            if amount.as_f64().is_some_and(|remaining| remaining <= 0.0) {
                "key cap reached".into()
            } else {
                format!("key cap ${amount} left")
            },
        ),
        (Field::ProviderKeyUsage, ProviderUsageReading::KeySpend(amount)) => {
            Some(format!("key spend ${amount}"))
        }
        _ => None,
    }
}

fn balance_text(balance: &ion_app::Balance) -> String {
    use ion_app::Currency;
    let mut text = String::from(if balance.is_available {
        "balance"
    } else {
        "insufficient balance"
    });
    for amount in &balance.balance_infos {
        text.push(' ');
        text.push_str(match amount.currency {
            Currency::Usd => "$",
            Currency::Cny => "¥",
        });
        text.push_str(&amount.total_balance);
    }
    text
}

fn token_field(label: &str, count: ion_core::TokenCount, observations: usize) -> Option<String> {
    (count.unreported < observations).then(|| {
        let partial = if count.unreported == 0 { "" } else { "+?" };
        format!("{label}{}{partial}", count.reported)
    })
}

pub(super) fn rows(
    ui: &Frontend,
    model: Option<&ModelRef>,
    width: usize,
    budget: usize,
) -> Vec<Line<'static>> {
    let Some(model) = model.filter(|_| budget > 0) else {
        return Vec::new();
    };
    let settled = !ui.session_io.blocked && ui.session_io.binding.is_none();
    let usage = ui.usage_totals;
    let tokens = |label, count| {
        settled
            .then(|| token_field(label, count, usage.observations))
            .flatten()
    };
    let value = |field| match field {
        Field::InputTokens => tokens("↑", usage.input_tokens),
        Field::OutputTokens => tokens("↓", usage.output_tokens),
        Field::CacheRead => tokens("R", usage.cache_read_input_tokens),
        Field::CacheWrite => tokens("W", usage.cache_write_input_tokens),
        Field::ProviderBalance | Field::ProviderKeyUsage => ui
            .provider_usage
            .current()
            .and_then(|reading| provider_usage_text(field, reading)),
        Field::Model => Some(format!("{}/{}", model.provider, model.model)),
        Field::Session => (!ui.session_label.is_empty()).then(|| ui.session_label.clone()),
        Field::Workspace => (!ui.cwd.as_os_str().is_empty()).then(|| ui.cwd.display().to_string()),
        Field::Reasoning => ui
            .reasoning_preference
            .filter(|_| settled)
            .map(|effort| format!("reasoning {effort}")),
        Field::Help => Some("/help".to_owned()),
        Field::Context => ui
            .context_estimate
            .as_ref()
            .filter(|_| settled && !ui.session_io.refreshing() && ui.conversation.live().is_none())
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
            }),
    };
    let row = |fields: &[Field]| {
        let mut text = String::new();
        let mut remaining = width;
        for &field in fields {
            let Some(raw) = value(field) else { continue };
            let separator = if text.is_empty() { "" } else { " · " };
            let room = remaining.saturating_sub(UnicodeWidthStr::width(separator));
            if room == 0 {
                break;
            }
            let (fitted, clipped) = crate::display_text::fit_line_with_status(&raw, room);
            // Keep fields whole on overflow, except for long identifiers and paths.
            if clipped && !matches!(field, Field::Model | Field::Session | Field::Workspace) {
                break;
            }
            if fitted == "…" && !text.is_empty() {
                break;
            }
            text.push_str(separator);
            text.push_str(&fitted);
            remaining = width.saturating_sub(UnicodeWidthStr::width(text.as_str()));
        }
        (!text.is_empty()).then(|| {
            Line::styled(
                text,
                ui.themes
                    .theme()
                    .style(crate::presentation_style::Role::Secondary),
            )
        })
    };
    let primary = row(&ui.footer.primary);
    let secondary = if budget > usize::from(primary.is_some()) {
        row(&ui.footer.secondary)
    } else {
        None
    };
    secondary.into_iter().chain(primary).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_client::Editor;

    #[test]
    fn provider_key_usage_is_distinct_from_account_balance() {
        use ion_app::{ProviderUsageKind, ProviderUsageReading};
        let footer: Footer =
            serde_json::from_str(r#"{"primary":["provider_key_usage"],"secondary":[]}"#).unwrap();
        assert!(footer.shows_provider_usage(ProviderUsageKind::OpenRouterKey));
        assert!(!footer.shows_provider_usage(ProviderUsageKind::DeepSeekBalance));
        let usage = ProviderUsageReading::KeySpend(serde_json::Number::from_f64(2.75).unwrap());
        assert_eq!(
            provider_usage_text(Field::ProviderKeyUsage, &usage).as_deref(),
            Some("key spend $2.75")
        );
        assert!(provider_usage_text(Field::ProviderBalance, &usage).is_none());
        let exhausted = ProviderUsageReading::KeyCapRemaining(0.into());
        assert_eq!(
            provider_usage_text(Field::ProviderKeyUsage, &exhausted).as_deref(),
            Some("key cap reached")
        );
    }

    #[test]
    fn configured_footer_drops_lower_priority_fields_and_then_the_secondary_row() {
        let ui = Frontend {
            footer: serde_json::from_str(
                r#"{"primary":["help","model"],"secondary":["workspace"]}"#,
            )
            .unwrap(),
            cwd: "/workspace".into(),
            ..Frontend::default()
        };
        let model = ModelRef {
            provider: "p".into(),
            model: "m".into(),
        };
        let rendered = |width, budget| {
            rows(&ui, Some(&model), width, budget)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(rendered(30, 2), ["/workspace", "/help · p/m"]);
        assert_eq!(rendered(30, 1), ["/help · p/m"]);
        assert_eq!(rendered(7, 1), ["/help"]);
    }

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
