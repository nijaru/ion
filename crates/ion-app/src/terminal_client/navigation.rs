//! Saved-Session browsing and committed-Turn fork boundaries.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use ion_ai::Message;
use ion_core::{ForkPoint, TurnEndReason, TurnSummary};
use ion_host::{SessionControl, SessionSummary};

use super::{
    EditorDraft, Frontend, picker::Picker, picker::PickerItem, picker::PickerValue,
    short_session_id,
};
use crate::display_text::fit_line;

const MAX_MODEL_METADATA_WIDTH: usize = 96;

#[derive(Debug)]
pub(super) struct CommittedContextRefresh(anyhow::Error);

impl std::fmt::Display for CommittedContextRefresh {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Context selection committed but display refresh failed; reopen the saved Session before continuing: {:#}",
            self.0
        )
    }
}
impl std::error::Error for CommittedContextRefresh {}

pub(super) struct ContextPickerValue {
    expected: ion_core::ContextPosition,
    point: ion_core::ContextPoint,
}

pub(super) fn apply_context(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    value: ContextPickerValue,
) -> Result<()> {
    runtime.navigate_context(value.expected, value.point.head_entry)?;
    // A client projection failure cannot undo a committed cursor. Stop this
    // terminal rather than accept another prompt against a stale conversation.
    ui.refresh_session(runtime.session())
        .map_err(CommittedContextRefresh)?;
    let restore_error = if value.point.restore_input {
        ui.restore_input(value.point.message).err()
    } else {
        None
    };
    ui.status = if let Some(error) = restore_error {
        format!("Context selected; {error}; draft retained; workspace files unchanged")
    } else {
        "Context selected; /tree returns to preserved alternatives; workspace files unchanged"
            .into()
    };
    Ok(())
}

pub(super) fn tree(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    args: &str,
) -> Result<()> {
    let view = runtime.session().view()?;
    let expected = view.context_position();
    let points = view.context_points();
    if !args.is_empty() {
        let entry = args.parse::<u64>().context("use /tree [ENTRY]")?;
        let point = points
            .into_iter()
            .find(|point| point.entry == entry)
            .context("selected message does not exist or is not a complete boundary")?;
        return apply_context(runtime, ui, ContextPickerValue { expected, point });
    }
    let items = points
        .into_iter()
        .rev()
        .map(|point| PickerItem {
            label: format!(
                "{} entry {} · {} · {}",
                if point.selected_path {
                    "[path]"
                } else {
                    "[alternative]"
                },
                point.entry,
                if point.restore_input {
                    "edit before"
                } else if point.message.role == ion_ai::Role::Tool {
                    "after tool group"
                } else {
                    "continue after"
                },
                plain(&crate::preview_input(&point.message))
            ),
            detail: Some("Changes model context only; workspace files remain current".into()),
            value: PickerValue::Context(ContextPickerValue { expected, point }),
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(!items.is_empty(), "no messages to explore");
    ui.pickers.open(Picker {
        title: "Explore context (files unchanged)",
        query: String::new(),
        selected: 0,
        items,
    });
    Ok(())
}

pub(super) struct ForkPickerValue {
    pub point: ForkPoint,
    pub restore: Option<Message>,
}

pub(super) fn apply_fork(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    value: ForkPickerValue,
) -> Result<()> {
    let id = runtime.fork_session(value.point)?;
    ui.refresh_session(runtime.session())?;
    let restore_error = value
        .restore
        .and_then(|input| ui.restore_input(input).err());
    ui.status = if let Some(error) = restore_error {
        format!(
            "Forked as {}; {error}; draft retained; inspect source to copy selected input",
            short_session_id(&id)
        )
    } else {
        format!(
            "Forked as {}; both sessions use the same working directory",
            short_session_id(&id)
        )
    };
    Ok(())
}

pub(super) fn resume(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    args: &str,
) -> Result<()> {
    if args.is_empty() {
        let catalog = runtime.catalog().clone();
        let current = runtime.session().id().to_string();
        ui.pickers.open_sessions(catalog, current);
    } else {
        let path = runtime.catalog().by_id(args)?;
        runtime.switch_session(path)?;
        ui.refresh_session(runtime.session())?;
    }
    Ok(())
}

pub(super) fn fork(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    args: &str,
) -> Result<()> {
    let turns = runtime.session().view()?.turns();
    if args.is_empty() {
        ui.pickers.open(Picker {
            title: "Fork before Turn",
            query: String::new(),
            selected: 0,
            items: fork_before_items(turns),
        });
    } else {
        let turn: u64 = args.parse().context("use /fork TURN")?;
        let input = turns
            .into_iter()
            .find(|item| item.turn == turn)
            .context("selected Turn does not exist")?
            .input;
        apply_fork(
            runtime,
            ui,
            ForkPickerValue {
                point: ForkPoint::BeforeTurn(turn),
                restore: Some(input),
            },
        )?;
    }
    Ok(())
}

pub(super) fn fork_after(
    runtime: &mut SessionControl<EditorDraft>,
    ui: &mut Frontend,
    args: &str,
) -> Result<()> {
    if args.is_empty() {
        let items = fork_after_items(runtime.session().view()?.turns());
        anyhow::ensure!(!items.is_empty(), "no ended Turns to fork after");
        ui.pickers.open(Picker {
            title: "Fork after Turn",
            query: String::new(),
            selected: 0,
            items,
        });
    } else {
        let turn: u64 = args.parse().context("use /fork-after [TURN]")?;
        // Core owns boundary validation; browsing is only a presentation aid.
        apply_fork(
            runtime,
            ui,
            ForkPickerValue {
                point: ForkPoint::AfterTurn(turn),
                restore: None,
            },
        )?;
    }
    Ok(())
}

fn fork_before_items(turns: Vec<TurnSummary>) -> Vec<PickerItem> {
    turns
        .into_iter()
        .map(|item| PickerItem {
            label: format!(
                "Turn {}  {}",
                item.turn,
                plain(&crate::preview_input(&item.input))
            ),
            detail: None,
            value: PickerValue::Fork(ForkPickerValue {
                point: ForkPoint::BeforeTurn(item.turn),
                restore: Some(item.input),
            }),
        })
        .collect()
}

fn fork_after_items(turns: Vec<TurnSummary>) -> Vec<PickerItem> {
    turns
        .into_iter()
        .filter_map(|item| {
            let end = item.end.as_ref()?;
            Some(PickerItem {
                label: format!(
                    "Turn {}  ended {}  {}",
                    item.turn,
                    turn_end_label(end),
                    plain(&crate::preview_input(&item.input))
                ),
                detail: None,
                value: PickerValue::Fork(ForkPickerValue {
                    point: ForkPoint::AfterTurn(item.turn),
                    restore: None,
                }),
            })
        })
        .collect()
}

pub(super) fn session_items(
    sessions: Vec<SessionSummary>,
    current_id: &str,
    now: SystemTime,
) -> Vec<PickerItem> {
    sessions
        .into_iter()
        .map(|session| {
            let is_current = session.id == current_id;
            let label = session_label(&session, is_current);
            let detail = session_detail(&session, is_current, now);
            PickerItem {
                label,
                detail: Some(detail),
                value: PickerValue::Session(session.path),
            }
        })
        .collect()
}

fn session_label(session: &SessionSummary, current: bool) -> String {
    let mut parts = vec![short_session_id(&session.id).to_owned()];
    if let Some(name) = session.name.as_deref().filter(|name| !name.is_empty()) {
        parts.push(plain(name));
    }
    if let Some(preview) = session
        .preview
        .as_deref()
        .filter(|preview| !preview.is_empty())
    {
        parts.push(plain(preview));
    }
    format!(
        "{}{}",
        if current { "[current] " } else { "" },
        parts.join("  ")
    )
}

fn session_detail(session: &SessionSummary, current: bool, now: SystemTime) -> String {
    let model = session.model.as_ref().map_or_else(
        || "unknown".to_owned(),
        |model| {
            let identity = format!("{}/{}", plain(&model.provider), plain(&model.model));
            fit_line(&identity, MAX_MODEL_METADATA_WIDTH)
        },
    );
    format!(
        "id {}{} · {} turn(s)\nmodel {} · {}",
        plain(&session.id),
        if current { " · current Session" } else { "" },
        session.turns,
        model,
        activity_label(session.updated, now)
    )
}

fn activity_label(updated: SystemTime, now: SystemTime) -> String {
    let relative = match updated.duration_since(now) {
        Ok(ahead) => format!("in {}", duration_label(ahead)),
        Err(error) => format!("{} ago", duration_label(error.duration())),
    };
    let Ok(since_epoch) = updated.duration_since(UNIX_EPOCH) else {
        return format!("file activity time unknown ({relative})");
    };
    let Some(timestamp) = utc_timestamp(since_epoch.as_secs()) else {
        return format!("file activity time unknown ({relative})");
    };
    format!("file activity {timestamp} UTC ({relative})")
}

fn duration_label(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0 => "under 1s".into(),
        1..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

pub(super) fn utc_timestamp(seconds: u64) -> Option<String> {
    let days = i64::try_from(seconds / 86_400).ok()?;
    let remainder = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600;
    let minute = remainder % 3_600 / 60;
    let second = remainder % 60;
    Some(format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}"
    ))
}

// Howard Hinnant's proleptic Gregorian conversion, with day zero at 1970-01-01.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month, day)
}

fn turn_end_label(end: &TurnEndReason) -> String {
    match end {
        TurnEndReason::Completed => "completed".into(),
        TurnEndReason::Cancelled => "cancelled".into(),
        TurnEndReason::Interrupted => "interrupted".into(),
        TurnEndReason::Failed(error) => format!("failed: {}", plain(error)),
    }
}

fn plain(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ion_ai::{Content, ModelRef, Reasoning, Role};

    use super::*;

    fn input(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![Content::Text(text.into())],
            provider_replay: None,
        }
    }

    fn turn(turn: u64, end: Option<TurnEndReason>) -> TurnSummary {
        TurnSummary {
            turn,
            reasoning: Reasoning::ProviderDefault,
            input: input(&format!("request {turn}")),
            model: ModelRef {
                provider: "smoke".into(),
                model: "model".into(),
            },
            end,
        }
    }

    fn summary(name: &str, model: Option<ModelRef>, updated: SystemTime) -> SessionSummary {
        SessionSummary {
            id: "019abcdef0123456789abcdef01234567".into(),
            path: "/state/session.sqlite".into(),
            name: Some(name.into()),
            preview: Some("first prompt\nsecond line".into()),
            model,
            turns: 3,
            updated,
        }
    }

    #[test]
    fn saved_session_presentation_marks_current_and_reports_existing_facts() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let recent = now - Duration::from_secs(125);
        let item = session_items(
            vec![summary("Named\u{1b}[31m task", None, recent)],
            "019abcdef0123456789abcdef01234567",
            now,
        )
        .pop()
        .unwrap();
        assert!(item.label.starts_with("[current] 019abcdef012"));
        assert!(item.label.contains("Named [31m task"));
        assert!(!item.label.contains('\u{1b}'));
        let detail = item.detail.as_deref().unwrap();
        assert!(detail.contains("id 019abcdef0123456789abcdef01234567 · current Session"));
        assert!(detail.contains("3 turn(s)"));
        assert!(detail.contains("model unknown"));
        assert!(detail.contains("file activity 2023-11-14 22:11:15 UTC (2m ago)"));
    }

    #[test]
    fn saved_session_detail_bounds_model_metadata_but_not_session_identity() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let huge_model = "m".repeat(4_096);
        let summary = summary(
            "Large model",
            Some(ModelRef {
                provider: "provider\u{1b}[31m".into(),
                model: huge_model.clone(),
            }),
            now,
        );
        let detail = session_detail(&summary, false, now);
        assert!(detail.contains("id 019abcdef0123456789abcdef01234567"));
        assert!(detail.contains('…'));
        assert!(!detail.contains(&huge_model));
        assert!(!detail.contains('\u{1b}'));
        assert!(
            detail.len() < 512,
            "metadata display is unbounded: {detail:?}"
        );
        assert_eq!(summary.model.as_ref().unwrap().model, huge_model);
    }

    #[test]
    fn activity_time_keeps_future_mtime_truthful() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let future = now + Duration::from_secs(3_600);
        let detail = session_detail(
            &summary(
                "Future",
                Some(ModelRef {
                    provider: "p".into(),
                    model: "m".into(),
                }),
                future,
            ),
            false,
            now,
        );
        assert!(detail.contains("model p/m"));
        assert!(detail.contains("file activity 2023-11-14 23:13:20 UTC (in 1h)"));
        assert!(!detail.contains("just now"));
        assert!(!detail.contains("current Session"));
    }

    #[test]
    fn fork_after_picker_requires_a_recorded_end_and_keeps_reasons() {
        let items = fork_after_items(vec![
            turn(1, Some(TurnEndReason::Completed)),
            turn(2, None),
            turn(3, Some(TurnEndReason::Cancelled)),
            turn(4, Some(TurnEndReason::Failed("provider\tfailed".into()))),
        ]);
        assert_eq!(items.len(), 3);
        assert!(items[0].label.contains("ended completed"));
        assert!(items[1].label.contains("ended cancelled"));
        assert!(items[2].label.contains("ended failed: provider failed"));
        let before = fork_before_items(vec![turn(1, None), turn(2, None)]);
        assert_eq!(before.len(), 2);
        assert!(before.iter().all(|item| item.label.starts_with("Turn ")));
    }
}
