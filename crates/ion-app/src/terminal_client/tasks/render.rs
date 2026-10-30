//! Literal task evidence: observations and durable outcomes stay distinct.
use anyhow::Result;
use ion_host::{BackgroundRunSnapshot, ChildActivity, ChildActivitySnapshot};

pub(super) fn state(joined: bool, progress: Option<&ChildActivitySnapshot>) -> &'static str {
    if joined {
        "Finished"
    } else if progress.is_some_and(|progress| progress.stopping) {
        "Stopping"
    } else {
        "Working"
    }
}

pub(super) fn activity(progress: &ChildActivitySnapshot) -> String {
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
        .and_then(|ms| super::super::navigation::utc_timestamp(ms / 1000))
        .map(|value| format!("{value} UTC"))
        .unwrap_or_else(|| "not observed".into());
    format!("activity: {kind}{tool} · elapsed {duration}\nlast activity: {last}")
}

pub(super) fn end_label(end: &ion_core::TurnEndReason) -> &'static str {
    match end {
        ion_core::TurnEndReason::Completed => "completed",
        ion_core::TurnEndReason::Cancelled => "cancelled",
        ion_core::TurnEndReason::Interrupted => "interrupted",
        ion_core::TurnEndReason::Failed(_) => "failed",
    }
}

pub(super) fn status(run: &BackgroundRunSnapshot) -> String {
    let progress = run.progress.as_ref();
    let mut text = format!(
        "Task {} · {}\nCleanup: {}",
        run.receipt.record.run,
        state(run.joined, progress.map(|progress| &progress.status)),
        if run.joined { "joined" } else { "pending" }
    );
    if let Some(completion) = &run.completion {
        let status = match completion {
            ion_core::CompletionStatus::Waiting => "waiting for join".into(),
            ion_core::CompletionStatus::Queued => "queued; report not acknowledged".into(),
            ion_core::CompletionStatus::Offered { turn } => {
                format!("offered in Turn {turn}; not proof of model read")
            }
            ion_core::CompletionStatus::Revoked { reason } => {
                format!("revoked ({reason:?}); report retained")
            }
        };
        text.push_str(&format!("\nReadiness: {status}"));
    }
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
            append_preview(&mut text, output);
        }
    } else {
        text.push_str("\nLive progress unavailable");
    }
    text.push_str(&format!(
        "\n\nModel: {}/{}\nChild {} · {}",
        run.receipt.record.spec.model.provider,
        run.receipt.record.spec.model.model,
        run.receipt.record.child,
        run.receipt.record.path.display()
    ));
    if run.joined {
        text.push_str(&format!(
            "\nOpen /tasks and choose Show result, or use /tasks result {}, for the durable outcome",
            run.receipt.record.run
        ));
    } else {
        text.push_str("\nOnly joined means cleanup finished; silence alone does not prove a stall");
    }
    text
}

pub(super) fn result(run: &BackgroundRunSnapshot) -> Result<String> {
    anyhow::ensure!(
        run.joined,
        "Task is still finishing; use /tasks to inspect progress"
    );
    let mut text = format!(
        "Task {} · observed outcome · cleanup joined",
        run.receipt.record.run
    );
    if let Some(error) = &run.error {
        text.push_str(&format!("\nInfrastructure error: {error}"));
    }
    match run.receipt.outcome.as_ref() {
        Some(ion_core::DelegationOutcome::Joined {
            execution_timed_out,
            elapsed_ms,
            partial_output,
            receipt,
            error,
            answer,
        }) => {
            if *execution_timed_out {
                text.push_str("\nExecution deadline expired");
            }
            if let Some(error) = error {
                text.push_str(&format!("\nChild error: {error}"));
            }
            if let Some(receipt) = receipt {
                text.push_str(&format!(
                    "\nEnd: {}\nTurn: {} · request {}/{}",
                    receipt.end.as_ref().map_or("unknown", end_label),
                    receipt.turn,
                    receipt.key.scope(),
                    receipt.key.id()
                ));
                if let Some(ion_core::TurnEndReason::Failed(error)) = &receipt.end {
                    text.push_str(&format!("\nTurn failure: {error}"));
                }
            } else {
                text.push_str("\nNo committed Turn closure observed; effects may be unknown");
            }
            if let Some(ms) = elapsed_ms {
                text.push_str(&format!("\nRuntime: {ms} ms"));
            }
            if let Some(answer) = answer {
                text.push_str(&format!("\n\nFinal answer\n{answer}"));
            }
            if let Some(output) = partial_output {
                append_preview(&mut text, output);
            }
        }
        Some(ion_core::DelegationOutcome::Unknown) => text.push_str(
            "\nOutcome unknown; inspect saved facts and effects. Do not replay implicitly.",
        ),
        None => text.push_str(
            "\nNo committed outcome; inspect saved facts and effects. This is not success.",
        ),
    }
    for steering in &run.steering {
        text.push_str(&format!(
            "\n\nGuidance {}/{} · ",
            steering.request.scope(),
            steering.request.id()
        ));
        match &steering.status {
            ion_host::SteeringStatus::Queued => {
                text.push_str("queued, not proof of model delivery")
            }
            ion_host::SteeringStatus::Committed { acceptance } => {
                text.push_str(&format!("committed to Turn {}", acceptance.turn))
            }
            ion_host::SteeringStatus::Unknown => {
                text.push_str("delivery unknown; inspect saved facts before replay")
            }
            ion_host::SteeringStatus::Returned { input, reason } => {
                text.push_str(&format!(
                    "returned, not delivered · {}\n",
                    match reason {
                        ion_host::ReturnSnapshot::Stopped => "stopped",
                        ion_host::ReturnSnapshot::Cleared => "cleared",
                        ion_host::ReturnSnapshot::TurnEnded => "Turn ended",
                        ion_host::ReturnSnapshot::Preparation(error) => error,
                        ion_host::ReturnSnapshot::NotAccepted => "not accepted",
                    }
                ));
                text.push_str(&returned_input(input));
                text.push_str("\nRemoving this task also releases this uncommitted guidance; it is not automatically replayed.");
            }
        }
    }
    text.push_str(&format!(
        "\n\nModel: {}/{}\nParent {}\nChild {} · {}",
        run.receipt.record.spec.model.provider,
        run.receipt.record.spec.model.model,
        run.receipt.record.site.session,
        run.receipt.record.child,
        run.receipt.record.path.display()
    ));
    Ok(text)
}

fn append_preview(text: &mut String, output: &ion_core::DelegationPartialOutput) {
    text.push_str("\n\nProvisional child output — not a final answer or committed fact\n");
    text.push_str(&output.text);
    if output.truncated {
        text.push_str("\n[preview truncated at 50 KiB UTF-8]");
    }
    if output.observation_gap {
        text.push_str("\n[progress gap; missing text was not reconstructed]");
    }
}

fn returned_input(input: &ion_host::UserInput) -> String {
    let (mut text, attachments) = match input {
        ion_host::UserInput::Prompt {
            text,
            images,
            files,
        } => {
            let mut text = text.clone();
            if !files.is_empty() {
                text.push_str(&format!(
                    "\n[{} text file snapshot(s) retained]",
                    files.len()
                ));
            }
            (text, images.len())
        }
        ion_host::UserInput::Message(message) => {
            let mut text = String::new();
            let mut attachments = 0;
            for part in &message.content {
                if let ion_ai::Content::Text(value) = part {
                    text.push_str(value);
                } else {
                    attachments += 1;
                }
            }
            (text, attachments)
        }
    };
    if attachments > 0 {
        text.push_str(&format!(
            "\n[{attachments} non-text attachment(s); contents not rendered]"
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::super::item;
    use super::*;
    use ion_core::{
        DelegationOutcome, DelegationOwnership, DelegationPartialOutput, DelegationReceipt,
        DelegationRecord, DelegationSite, DelegationSpec, DelegationTarget, ToolOccurrence,
    };
    use ion_host::BackgroundJobStatus;
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
            completion: None,
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

    fn job(run: &BackgroundRunSnapshot) -> BackgroundJobStatus {
        BackgroundJobStatus {
            completion: run.completion.clone(),
            id: run.receipt.record.run,
            model: run.receipt.record.spec.model.clone(),
            parent: run.receipt.record.site.session,
            path: run.receipt.record.path.clone(),
            joined: run.joined,
            end: None,
            error: run.error.clone(),
            progress: run.progress.as_ref().map(|p| p.status.clone()),
        }
    }

    #[test]
    fn task_actions_follow_cleanup_and_keep_draft_custody() {
        use super::super::{TaskChoice, actions};
        use crate::terminal_client::{Action, Frontend, PickerValue};
        use ion_terminal::{KeyCode, KeyEvent, Modifiers};
        let mut run = observation();
        for (joined, stopping) in [(false, false), (false, true), (true, true)] {
            run.joined = joined;
            run.progress.as_mut().unwrap().status.stopping = stopping;
            let picker = actions(&job(&run));
            let offered = |wanted| {
                picker
                    .items
                    .iter()
                    .any(|item| matches!(item.value, PickerValue::Task(choice) if choice == wanted))
            };
            assert_eq!(
                offered(TaskChoice::Stop(run.receipt.record.run)),
                !joined && !stopping
            );
            assert_eq!(offered(TaskChoice::Result(run.receipt.record.run)), joined);
            assert_eq!(offered(TaskChoice::Remove(run.receipt.record.run)), joined);
            let mut ui = Frontend {
                draft: "保留_🦀".into(),
                cursor: 3,
                ..Frontend::default()
            };
            ui.pickers.open(picker);
            assert!(matches!(
                ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
                Action::None
            ));
            ui.pickers.rows(100, 20, ui.themes.theme());
            assert!(matches!(
                ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
                Action::Pick(PickerValue::Task(TaskChoice::Progress(_)))
            ));
            assert_eq!((ui.draft.as_str(), ui.cursor), ("保留_🦀", 3));
        }
    }

    #[test]
    fn completed_task_cannot_retarget_filtered_stop_to_removal() {
        use super::super::{TaskChoice, actions, refresh_selection};
        use crate::terminal_client::{Action, Frontend, PickerValue};
        use ion_terminal::{KeyCode, KeyEvent, Modifiers};
        let mut run = observation();
        let mut previous = actions(&job(&run));
        previous.query = "task".into();
        assert!(matches!(
            previous.items[previous.matches(false, None)[0]].value,
            PickerValue::Task(TaskChoice::Stop(_))
        ));
        run.joined = true;
        let mut next = actions(&job(&run));
        refresh_selection(&mut next, &previous);
        let mut ui = Frontend::default();
        ui.pickers.open(next);
        ui.pickers.rows(100, 20, ui.themes.theme());
        assert!(matches!(
            ui.key(KeyEvent::new(KeyCode::Enter, Modifiers::NONE), None, None),
            Action::Pick(PickerValue::Task(TaskChoice::Progress(_)))
        ));
    }

    #[test]
    fn readable_result_keeps_final_failure_and_unknown_distinct() {
        let mut run = observation();
        assert!(result(&run).is_err());
        run.joined = true;
        run.receipt.outcome = Some(DelegationOutcome::Joined {
            execution_timed_out: false,
            elapsed_ms: Some(123),
            partial_output: None,
            receipt: Some(ion_core::RequestReceipt {
                session: run.receipt.record.child,
                key: run.receipt.record.request_key(),
                turn: 1,
                end: Some(ion_core::TurnEndReason::Completed),
            }),
            error: None,
            answer: Some("ANSWER_🦀".into()),
        });
        let text = result(&run).unwrap();
        assert!(text.contains("Final answer\nANSWER_🦀") && text.contains("Runtime: 123 ms"));
        run.receipt.outcome = Some(DelegationOutcome::Joined {
            execution_timed_out: true,
            elapsed_ms: None,
            partial_output: run.progress.as_ref().unwrap().provisional_output.clone(),
            receipt: None,
            error: Some("EXECUTION_FAILURE".into()),
            answer: None,
        });
        let text = result(&run).unwrap();
        assert!(
            text.find("EXECUTION_FAILURE").unwrap()
                < text.find("Provisional child output").unwrap()
        );
        assert!(
            text.contains("deadline expired")
                && text.contains("progress gap")
                && text.contains("preview truncated")
        );
        assert!(!text.contains("Final answer"));
        assert!(
            super::super::super::notice_rows(&text, 70)
                .iter()
                .flat_map(|row| &row.spans)
                .all(|span| !span.content.contains('\u{1b}'))
        );
        run.receipt.outcome = Some(DelegationOutcome::Unknown);
        assert!(result(&run).unwrap().contains("Do not replay implicitly"));
    }

    #[test]
    fn returned_guidance_renders_literal_text_without_decoding_attachments() {
        let input = ion_host::UserInput::Prompt {
            files: Vec::new(),
            text: "UNDELIVERED_🦀".into(),
            images: vec![ion_host::image_input::ImageSource::Path(
                "missing.png".into(),
            )],
        };
        assert_eq!(
            returned_input(&input),
            "UNDELIVERED_🦀\n[1 non-text attachment(s); contents not rendered]"
        );
        assert_eq!(
            returned_input(&ion_host::UserInput::Message(ion_ai::Message::user_input(
                "LITERAL".into(),
                []
            ))),
            "LITERAL"
        );
    }

    #[test]
    fn summaries_distinguish_custody_and_show_only_content_free_observations() {
        let run = observation();
        let mut job = BackgroundJobStatus {
            completion: None,
            id: run.receipt.record.run,
            model: run.receipt.record.spec.model.clone(),
            parent: run.receipt.record.site.session,
            path: run.receipt.record.path,
            joined: false,
            end: None,
            error: None,
            progress: run.progress.map(|progress| progress.status),
        };
        for (joined, stopping, expected) in [
            (false, false, "Working"),
            (false, true, "Stopping"),
            (true, true, "Finished"),
        ] {
            job.joined = joined;
            job.progress.as_mut().unwrap().stopping = stopping;
            let row = item(&job);
            let text = format!("{}\n{}", row.label, row.detail.unwrap());
            assert!(text.lines().next().unwrap().contains(expected), "{text}");
            job.error = Some("FAILURE_CAUSE".into());
            assert!(
                item(&job)
                    .detail
                    .unwrap()
                    .starts_with("Error: FAILURE_CAUSE")
            );
            job.error = None;
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
        let rows = super::super::super::notice_rows(&text, 70);
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
