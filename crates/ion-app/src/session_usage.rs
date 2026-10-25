//! One human-readable accounting report for terminal and passive CLI inspection.
use std::fmt::Write;

use ion_core::{SessionUsage, TokenCount, UsageTotals};

pub(crate) fn render(usage: &SessionUsage) -> String {
    let total = usage.total();
    let mut text = format!(
        "Recorded usage · {} observation(s)\nCost: unknown (historical prices/billing not recorded)\n",
        total.observations
    );
    if total.observations == 0 {
        text.push_str("No recorded usage yet.\n");
    } else {
        for (label, count) in [
            ("Input tokens", total.input_tokens),
            ("Output tokens", total.output_tokens),
            ("Cache read tokens", total.cache_read_input_tokens),
            ("Cache write tokens", total.cache_write_input_tokens),
        ] {
            // Writing to an owned String cannot fail.
            writeln!(text, "{label}: {}", measurement(count, total.observations))
                .expect("writing to a String is infallible");
        }
        writeln!(text, "Total tokens: {}", tokens(total))
            .expect("writing to a String is infallible");
        text.push_str("By request kind:\n");
        for (kind, counts) in [
            ("Assistant", usage.assistant),
            ("Compaction", usage.compaction),
            ("Cache refresh", usage.cache_warm),
        ] {
            if counts.observations > 0 {
                writeln!(
                    text,
                    "  {kind}: {} token(s), {} observation(s)",
                    tokens(counts),
                    counts.observations
                )
                .expect("writing to a String is infallible");
            }
        }
        text.push_str("By captured model:\n");
        for model in &usage.models {
            let logical = format!(
                "{}/{}",
                crate::metadata_field(&model.logical.provider),
                crate::metadata_field(&model.logical.model)
            );
            let effective = if model.logical == model.effective {
                String::new()
            } else {
                format!(
                    " → {}/{}",
                    crate::metadata_field(&model.effective.provider),
                    crate::metadata_field(&model.effective.model)
                )
            };
            let returned = model
                .returned_model
                .as_deref()
                .map_or_else(String::new, |returned| {
                    format!(" (provider reports {})", crate::metadata_field(returned))
                });
            writeln!(
                text,
                "  {logical}{effective}{returned}: {} token(s), {} observation(s)",
                tokens(model.usage),
                model.usage.observations
            )
            .expect("writing to a String is infallible");
        }
    }
    text.push_str("Scope: all recorded Session history, including copied/inactive paths.\nOther Sessions and unrecorded/in-flight requests excluded; not a bill or quota.");
    text
}

fn measurement(count: TokenCount, observations: usize) -> String {
    if count.unreported == observations {
        "unknown".into()
    } else if count.unreported == 0 {
        count.reported.to_string()
    } else {
        format!(
            "{} reported; {} observation(s) unreported",
            count.reported, count.unreported
        )
    }
}

fn tokens(usage: UsageTotals) -> String {
    if usage.input_tokens.unreported == usage.observations
        && usage.output_tokens.unreported == usage.observations
    {
        "unknown".into()
    } else if usage.has_complete_tokens() {
        usage.reported_tokens().to_string()
    } else {
        format!("{} reported (incomplete)", usage.reported_tokens())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_ai::ModelRef;
    use ion_core::ModelUsage;

    #[test]
    fn empty_unknown_and_measured_zero_have_distinct_reports() {
        let mut usage = SessionUsage::default();
        assert!(render(&usage).contains("No recorded usage yet."));
        usage.assistant.observations = 1;
        usage.assistant.input_tokens.unreported = 1;
        usage.assistant.output_tokens.unreported = 1;
        assert!(render(&usage).contains("Total tokens: unknown"));
        usage.assistant.input_tokens.unreported = 0;
        usage.assistant.output_tokens.unreported = 0;
        assert!(render(&usage).contains("Total tokens: 0\n"));
        assert!(render(&usage).contains("Cost: unknown"));
    }

    #[test]
    fn provider_metadata_cannot_inject_rows_or_terminal_controls() {
        let usage = SessionUsage {
            models: vec![ModelUsage {
                logical: ModelRef {
                    provider: "logical".into(),
                    model: "auto".into(),
                },
                effective: ModelRef {
                    provider: "physical".into(),
                    model: "route".into(),
                },
                returned_model: Some("alias\n\x1b[2J\u{85}end".into()),
                usage: UsageTotals {
                    observations: 1,
                    ..UsageTotals::default()
                },
            }],
            assistant: UsageTotals {
                observations: 1,
                ..UsageTotals::default()
            },
            ..SessionUsage::default()
        };
        let report = render(&usage);
        assert!(report.contains("logical/auto → physical/route (provider reports alias  [2J end)"));
        assert!(!report.contains('\x1b') && !report.contains('\u{85}'));
    }
}
