//! Human inspection over Host custody. No client-owned task lifecycle or cache.
use anyhow::{Context, Result};
use ion_host::{
    BackgroundJobStatus, BackgroundOwner, BackgroundRunSnapshot, ChildActivity,
    ChildActivitySnapshot,
};

use super::Frontend;

const USAGE: &str = "use /tasks [status|result|cancel|acknowledge UUID]";

pub(super) fn command(ui: &mut Frontend, owner: &BackgroundOwner, args: &str) -> Result<()> {
    if args.is_empty() {
        let jobs = owner.jobs();
        if jobs.is_empty() {
            ui.print("No retained background tasks".into());
        }
        for job in jobs {
            ui.print(summary(&job));
        }
        return Ok(());
    }
    let (action, id) = args.split_once(char::is_whitespace).context(USAGE)?;
    let child = id.trim().parse::<uuid::Uuid>()?;
    match action {
        "status" => {
            let report = owner
                .run(child)
                .context("child is not owned here (unknown or acknowledged)")?;
            ui.print(status(&report));
        }
        "result" => {
            let report = owner
                .run(child)
                .context("child is not owned here (unknown or acknowledged)")?;
            anyhow::ensure!(
                report.joined,
                "child has not joined; use /tasks status to inspect live progress"
            );
            let outcome = report
                .receipt
                .outcome
                .context("joined without a committed outcome; inspect Session facts")?;
            ui.print(format!(
                "Child {child} · observed outcome\n{}",
                serde_json::to_string_pretty(&outcome)?
            ));
        }
        "cancel" => {
            owner.request_stop(child)?;
            ui.status = format!("Stop requested for {child}; cleanup must still join");
        }
        "acknowledge" => {
            let report = owner.take(child).context(
                "no joined result to acknowledge (running, unknown or already acknowledged)",
            )??;
            ui.print(format!(
                "Acknowledged {child}; committed facts remain at {}",
                report.receipt.record.path.display()
            ));
        }
        _ => anyhow::bail!(USAGE),
    }
    Ok(())
}

fn state(joined: bool, progress: Option<&ChildActivitySnapshot>) -> &'static str {
    if joined {
        "joined"
    } else if progress.is_some_and(|progress| progress.stopping) {
        "stopping · cleanup unjoined"
    } else {
        "unjoined"
    }
}

fn activity(progress: &ChildActivitySnapshot) -> String {
    let kind = match progress.activity {
        Some(ChildActivity::Model) => "model",
        Some(ChildActivity::Thinking) => "thinking (content hidden)",
        Some(ChildActivity::Text) => "text",
        Some(ChildActivity::Tool) => "tool",
        None => "none observed",
    };
    let tool = progress
        .current_tool
        .as_ref()
        .map(|name| format!(" · {name}"))
        .unwrap_or_default();
    let elapsed = progress.elapsed_ms;
    let duration = if elapsed < 60_000 {
        format!("{}.{:01}s", elapsed / 1000, elapsed % 1000 / 100)
    } else {
        format!("{}m {}s", elapsed / 60_000, elapsed % 60_000 / 1000)
    };
    let last = progress
        .last_activity_at_ms
        .and_then(|ms| super::navigation::utc_timestamp(ms / 1000))
        .map(|value| format!("{value} UTC"))
        .unwrap_or_else(|| "not observed".into());
    format!("activity: {kind}{tool} · elapsed {duration}\nlast activity: {last}")
}

fn end_label(end: &ion_core::TurnEndReason) -> &'static str {
    match end {
        ion_core::TurnEndReason::Completed => "completed",
        ion_core::TurnEndReason::Cancelled => "cancelled",
        ion_core::TurnEndReason::Interrupted => "interrupted",
        ion_core::TurnEndReason::Failed(_) => "failed",
    }
}

fn summary(job: &BackgroundJobStatus) -> String {
    let mut text = format!("{} · {}", job.id, state(job.joined, job.progress.as_ref()));
    if let Some(end) = &job.end {
        text.push_str(&format!(" · end: {}", end_label(end)));
    }
    if let Some(error) = &job.error {
        text.push_str(&format!("\nerror: {error}"));
    }
    if let Some(progress) = &job.progress {
        text.push_str(&format!("\n{}", activity(progress)));
    }
    text.push_str(&format!("\nparent {} · {}", job.parent, job.path.display()));
    text
}

fn status(run: &BackgroundRunSnapshot) -> String {
    let progress = run.progress.as_ref();
    let mut text = format!(
        "Task {} · {}",
        run.receipt.record.run,
        state(run.joined, progress.map(|progress| &progress.status))
    );
    if let Some(error) = &run.error {
        text.push_str(&format!("\nerror: {error}"));
    }
    if let Some(ion_core::DelegationOutcome::Joined {
        execution_timed_out,
        error,
        receipt,
        ..
    }) = &run.receipt.outcome
    {
        if *execution_timed_out {
            text.push_str("\nexecution deadline expired");
        }
        if let Some(error) = error {
            text.push_str(&format!("\nchild error: {error}"));
        }
        if let Some(receipt) = receipt {
            text.push_str(&format!(
                "\nchild end: {}",
                receipt.end.as_ref().map_or("unknown", end_label)
            ));
        }
    }
    if let Some(progress) = progress {
        text.push_str(&format!("\n{}", activity(&progress.status)));
        if let Some(output) = &progress.provisional_output {
            text.push_str("\n\nProvisional child output — not a final answer or committed fact\n");
            text.push_str(&output.text);
            if output.truncated {
                text.push_str("\n[preview truncated at 50 KiB UTF-8]");
            }
            if output.observation_gap {
                text.push_str("\n[progress gap; missing text was not reconstructed]");
            }
        }
    } else {
        text.push_str("\nLive progress unavailable");
    }
    text.push_str(&format!(
        "\n\nChild {} · {}",
        run.receipt.record.child,
        run.receipt.record.path.display()
    ));
    if run.joined {
        text.push_str(&format!(
            "\nUse /tasks result {} for the durable outcome",
            run.receipt.record.run
        ));
    } else {
        text.push_str("\nOnly joined means cleanup finished; silence alone does not prove a stall");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use ion_core::{
        DelegationOutcome, DelegationOwnership, DelegationPartialOutput, DelegationReceipt,
        DelegationRecord, DelegationSite, DelegationSpec, DelegationTarget, ToolOccurrence,
    };
    use ion_host::ChildProgressSnapshot;

    fn observation() -> BackgroundRunSnapshot {
        let record = DelegationRecord {
            run: uuid::Uuid::from_u128(1),
            child: uuid::Uuid::from_u128(2),
            site: DelegationSite {
                session: uuid::Uuid::from_u128(3),
                turn: 1,
                tool: ToolOccurrence {
                    assistant_entry: 2,
                    ordinal: 0,
                },
                composed_child: None,
            },
            path: "/children/observed.sqlite".into(),
            ownership: DelegationOwnership::Background {
                owner: uuid::Uuid::from_u128(4),
            },
            spec: DelegationSpec {
                target: DelegationTarget::Fresh {
                    directory: "/children".into(),
                },
                cwd: None,
                model: ion_ai::ModelRef {
                    provider: "fixture".into(),
                    model: "child".into(),
                },
                reasoning: ion_ai::Reasoning::ProviderDefault,
                execution_timeout_ms: None,
                authority: ion_core::AuthorityScope::new("fixture").unwrap(),
                input: ion_ai::Message::user_input("HIDDEN_TASK_ARGUMENT".into(), []),
            },
        };
        let progress = ChildProgressSnapshot {
            status: ChildActivitySnapshot {
                child_session: record.child,
                request: record.request_key(),
                elapsed_ms: 12_300,
                last_activity_at_ms: Some(1_700_000_000_000),
                activity: Some(ChildActivity::Thinking),
                current_tool: None,
                stopping: false,
            },
            provisional_output: Some(DelegationPartialOutput {
                text: "literal ```rust\n\u{1b}[31mVISIBLE 🚀".into(),
                truncated: true,
                observation_gap: true,
            }),
        };
        BackgroundRunSnapshot {
            receipt: DelegationReceipt {
                record,
                transferred: true,
                outcome: None,
            },
            joined: false,
            error: None,
            steering: vec![],
            progress: Some(progress),
        }
    }

    #[test]
    fn summaries_distinguish_custody_and_show_only_content_free_observations() {
        let run = observation();
        let mut job = BackgroundJobStatus {
            id: run.receipt.record.run,
            parent: run.receipt.record.site.session,
            path: run.receipt.record.path,
            joined: false,
            end: None,
            error: None,
            progress: run.progress.map(|progress| progress.status),
        };
        for (joined, stopping, expected) in [
            (false, false, "unjoined"),
            (false, true, "stopping · cleanup unjoined"),
            (true, true, "joined"),
        ] {
            job.joined = joined;
            job.progress.as_mut().unwrap().stopping = stopping;
            let text = summary(&job);
            assert!(text.lines().next().unwrap().ends_with(expected), "{text}");
            assert!(text.contains("thinking (content hidden)") && text.contains("elapsed 12.3s"));
            assert!(text.contains("2023-11-14 22:13:20 UTC"));
            assert!(!text.contains("VISIBLE") && !text.contains("HIDDEN_TASK_ARGUMENT"));
        }
    }

    #[test]
    fn failure_precedes_explicit_provisional_literal_preview_with_gap_and_truncation() {
        let mut run = observation();
        run.joined = true;
        run.error = Some("infrastructure cause".into());
        run.receipt.outcome = Some(DelegationOutcome::Joined {
            execution_timed_out: true,
            elapsed_ms: Some(12_300),
            partial_output: None,
            receipt: None,
            error: Some("execution cause".into()),
            answer: None,
        });
        let text = status(&run);
        assert!(
            text.find("infrastructure cause").unwrap()
                < text.find("Provisional child output").unwrap()
        );
        assert!(
            text.find("execution cause").unwrap() < text.find("Provisional child output").unwrap()
        );
        assert!(text.contains("not a final answer or committed fact"));
        assert!(text.contains("preview truncated") && text.contains("progress gap"));
        assert!(text.contains("for the durable outcome") && !text.contains("HIDDEN_TASK_ARGUMENT"));
        // This is the real inline/fullscreen notice path, not Markdown execution
        // or a direct write of untrusted control bytes to the emulator.
        let rows = super::super::notice_rows(&text, 70);
        assert!(
            rows.iter()
                .flat_map(|row| row.spans.iter())
                .all(|span| !span.content.contains('\u{1b}')
                    && span.style == ratatui::style::Style::default())
        );
        assert!(
            rows.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                .contains("literal ```rust")
        );
    }
}
