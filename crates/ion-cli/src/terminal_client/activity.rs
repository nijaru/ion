//! Provisional terminal projection of shared control updates, not execution custody.
use anyhow::Result;
use ion_app::{
    ControlUpdate, Delivery, InputPhase, Observation, OperationOutcome, ReturnReason,
    SessionControl, UserInput,
};
use ion_core::AgentEvent;

use super::{EditorDraft, Frontend};

#[derive(Clone, Copy)]
pub(super) enum OperationKind {
    Coding,
    Shell,
    Compaction,
    Preparation,
    Binding,
    McpReconnect,
}

#[derive(Clone, Copy)]
pub(super) struct ActiveOperation {
    pub(super) kind: OperationKind,
    pub(super) stop_requested: bool,
}

impl ActiveOperation {
    pub(super) fn from_state(state: ion_app::ControlState) -> Option<Self> {
        let (kind, stop_requested) = match state.operation {
            Some(operation) => (
                match operation.kind {
                    ion_app::OperationKind::Turn => OperationKind::Coding,
                    ion_app::OperationKind::Shell => OperationKind::Shell,
                    ion_app::OperationKind::Compact => OperationKind::Compaction,
                },
                state.stop_requested,
            ),
            None => match state.mcp_reconnect {
                Some(reconnect) => (OperationKind::McpReconnect, reconnect.stop_requested),
                None if state.binding.is_some() => (OperationKind::Binding, state.stop_requested),
                None if state.busy => (OperationKind::Preparation, state.stop_requested),
                None => return None,
            },
        };
        Some(Self {
            kind,
            stop_requested,
        })
    }

    pub(super) fn is_cancelled(self) -> bool {
        self.stop_requested
    }

    pub(super) fn controls(self, ui: &Frontend) -> String {
        use crate::keybindings::KeyAction;
        let keys = &ui.keys;
        let submit = keys.hint(KeyAction::Submit);
        let follow_up = keys.hint(KeyAction::FollowUp);
        let cancel = ui.stop_hint();
        match self.kind {
            OperationKind::Coding => {
                format!("{cancel} stops · {submit} steers · {follow_up} queues")
            }
            OperationKind::Shell | OperationKind::Compaction | OperationKind::McpReconnect => {
                format!("{cancel} stops · {submit} queues")
            }
            OperationKind::Preparation => format!("{cancel} stops · {follow_up} queues"),
            OperationKind::Binding => {
                format!("{cancel} stops · input waits for Session refresh")
            }
        }
    }

    pub(super) fn label(self) -> &'static str {
        match (self.kind, self.stop_requested) {
            (OperationKind::Coding, true) => "Cancelling turn…",
            (OperationKind::Shell, true) => "Cancelling shell…",
            (OperationKind::Compaction, true) => "Cancelling compaction…",
            (OperationKind::Preparation, true) => "Cancelling input…",
            (OperationKind::Binding, true) => "Stopping Session change…",
            (OperationKind::McpReconnect, true) => "Cancelling MCP reconnect…",
            (OperationKind::Coding, false) => "Working",
            (OperationKind::Shell, false) => "Running shell",
            (OperationKind::Compaction, false) => "Summarizing context",
            (OperationKind::Preparation, false) => "Preparing input",
            (OperationKind::Binding, false) => "Preparing Session change",
            (OperationKind::McpReconnect, false) => "Reconnecting MCP",
        }
    }
}

pub(super) fn preview_rows(
    preview: &ion_core::ToolOutputPreview,
    width: usize,
    budget: usize,
) -> Vec<String> {
    let mut rows = Vec::new();
    for stream in [
        ion_core::OutputStream::Stdout,
        ion_core::OutputStream::Stderr,
    ] {
        let bytes = preview.bytes(stream);
        if !bytes.is_empty() {
            let text = String::from_utf8_lossy(bytes);
            // Display the latest update on each progress line, not a VT
            // replay. Layout still sanitizes all remaining terminal controls.
            let lines = text.trim_end_matches('\n').split('\n').map(|line| {
                line.trim_end_matches('\r')
                    .rsplit('\r')
                    .next()
                    .unwrap_or_default()
            });
            let text = lines.collect::<Vec<_>>().join("\n");
            crate::display_text::push_wrapped(
                &mut rows,
                &format!("{} · {text}", stream.label()),
                width,
            );
        }
    }
    rows.drain(..rows.len().saturating_sub(budget));
    rows
}

pub(super) struct RetryProgress {
    attempt: usize,
    max_retries: usize,
    until: tokio::time::Instant,
}

impl RetryProgress {
    pub(super) fn label(&self, now: tokio::time::Instant) -> Option<String> {
        (now < self.until).then(|| {
            let remaining = (self.until - now).as_nanos().div_ceil(1_000_000_000);
            format!(
                "Retrying ({}/{}) in {remaining}s…",
                self.attempt, self.max_retries
            )
        })
    }
}

/// Returned errors have already been made visible in UI status. The caller
/// may keep interacting after an ordinary operation failure; shutdown instead
/// combines those failures with its primary I/O error.
pub(super) fn apply(
    ui: &mut Frontend,
    control: &SessionControl<EditorDraft>,
    update: ControlUpdate<EditorDraft>,
) -> Result<Option<anyhow::Error>> {
    match update {
        ControlUpdate::BindingFinished(finished) => {
            super::session_io::binding_finished(ui, control, finished)?
        }
        ControlUpdate::RequestAdmission(_) | ControlUpdate::RecoveryAdmission(_) => {
            // Native input never invokes keyed SDK request/recovery admission.
            // Propagate through chat's common shutdown boundary to join custody.
            anyhow::bail!("native frontend received an unexpected SDK admission update");
        }
        ControlUpdate::McpReconnectFinished(finished) => {
            match finished.result {
                Ok(report) => {
                    ui.note(format!("MCP server {} reconnected", finished.server));
                    for diagnostic in report.diagnostics {
                        ui.note(diagnostic);
                    }
                    ui.status.clear();
                }
                Err(error) => {
                    ui.note(format!("MCP server {} reconnect failed: {error:#}. Unknown effects were not replayed.", finished.server));
                    ui.status = format!("MCP reconnect failed: {error:#}");
                }
            }
        }
        ControlUpdate::Automated { .. } => {
            ui.discard_provisional();
            ui.conversation.clear_live();
            ui.status = "Managed-work readiness · automatic Turn".into();
        }
        ControlUpdate::Input {
            delivery, phase, ..
        } => {
            ui.status = match phase {
                InputPhase::Preparing => "Preparing input…",
                InputPhase::Queued => match delivery {
                    Delivery::Start => "Starting turn…",
                    Delivery::Steer(_) => "Steering queued for the active turn",
                    Delivery::FollowUp => "Follow-up queued",
                },
            }
            .into();
        }
        ControlUpdate::Dispatched { .. } => {}
        ControlUpdate::Accepted { .. } => {
            // No editor custody remains here. In particular, a later history
            // read failure must never reconstruct or replay accepted input.
            ui.status.clear();
        }
        ControlUpdate::Activity(Observation::Event { operation, event }) => {
            match event.as_ref() {
                AgentEvent::ToolOutputProgress { preview, .. } => {
                    if control.current_operation() == Some(operation) {
                        ui.command_preview = preview.clone();
                    }
                    return Ok(None);
                }
                AgentEvent::ToolFinished { .. } => {
                    ui.command_preview = ion_core::ToolOutputPreview::default()
                }
                _ => {}
            }
            if let AgentEvent::ProviderRetry {
                attempt,
                max_retries,
                delay_ms,
            } = event.as_ref()
            {
                ui.retry_progress = tokio::time::Instant::now()
                    .checked_add(std::time::Duration::from_millis(*delay_ms))
                    .map(|until| RetryProgress {
                        attempt: *attempt,
                        max_retries: *max_retries,
                        until,
                    });
                // Native chat shows the transient retry in the activity indicator,
                // not also as a transcript-side notice. Headless diagnostics stay intact.
                return Ok(None);
            }
            if matches!(
                event.as_ref(),
                AgentEvent::TextDelta { .. }
                    | AgentEvent::ThinkingDelta { .. }
                    | AgentEvent::ModelOutputObserved
                    | AgentEvent::AssistantCommitted { .. }
                    | AgentEvent::TurnEnded { .. }
            ) {
                ui.retry_progress = None;
            }
            if matches!(
                event.as_ref(),
                AgentEvent::ContextCompactionRetry { .. }
                    | AgentEvent::ContextCompactionSkipped { .. }
            ) && !ui.conversation.live_for(operation)
                && let Some(note) = crate::agent_events::diagnostic(&event)
            {
                ui.note(note);
            }
            if matches!(event.as_ref(), AgentEvent::ResponseRestarted)
                && ui.conversation.live_for(operation)
            {
                ui.discard_provisional();
            }
            if matches!(event.as_ref(), AgentEvent::AssistantCommitted { .. })
                && ui.conversation.live_for(operation)
                && let Some(live) = ui.conversation.live()
            {
                ui.view.commit_response(live, &event);
                if let Some(details) = &mut ui.details {
                    details.commit_response(live, &event);
                }
            }
            if ui.conversation.observe(operation, &event) {
                ui.session_io.request_refresh();
            }
        }
        ControlUpdate::Activity(Observation::ShellOutput {
            operation,
            stream,
            bytes,
        }) => {
            if control.current_operation() == Some(operation) {
                ui.command_preview.append(stream, &bytes);
            }
        }
        ControlUpdate::Activity(Observation::Gap { .. }) => {
            ui.retry_progress = None;
            ui.command_preview = ion_core::ToolOutputPreview::default();
            // Drop the whole partial projection. Host may resume observations
            // at a committed boundary, but LiveTranscript's tool/child indexes
            // require an uninterrupted Turn. Stay on committed facts until the
            // next Turn instead of inventing a second full-history snapshot.
            ui.discard_provisional();
            ui.conversation.clear_live();
            ui.note("Live updates skipped; showing committed Session facts".into());
            ui.session_io.request_refresh();
        }
        ControlUpdate::Returned(returned) => {
            let UserInput::Prompt { attachments, .. } = returned.input else {
                unreachable!("terminal submits only raw prompts")
            };
            let withdrawn = matches!(returned.reason, ReturnReason::Cleared);
            let composer_available = !ui.clipboard_pending()
                && ui.input_target() == super::interaction::InputTarget::Composer;
            if withdrawn && composer_available {
                ui.show_draft(super::RecoveredDraft {
                    source: returned.metadata,
                    attachments,
                });
            } else {
                ui.return_draft(returned.metadata, attachments);
            }
            let retention = if !composer_available {
                ui.status = "Returned draft retained separately".into();
                "Draft retained separately until the composer is available"
            } else {
                ui.status = "Draft returned".into();
                "Draft retained"
            };
            // An explicit withdrawal already shows its result in the
            // composer and dock. Keep failures, not routine queue edits, in
            // the transcript.
            if matches!(returned.reason, ReturnReason::Cleared)
                && returned.preparation_error.is_none()
            {
                return Ok(None);
            }
            let fault = returned
                .preparation_error
                .map(|error| format!("; preparation also failed: {error:#}"))
                .unwrap_or_default();
            let kind = match returned.delivery {
                Delivery::Start => "Prompt",
                Delivery::Steer(_) => "Steering",
                Delivery::FollowUp => "Follow-up",
            };
            let reason = match returned.reason {
                ReturnReason::Stopped => "stop requested".into(),
                ReturnReason::Cleared => "withdrawn from queue".into(),
                ReturnReason::TurnEnded => "turn ended before acceptance".into(),
                ReturnReason::NotAccepted => "not durably accepted".into(),
                ReturnReason::PredecessorFailed => {
                    "preceding work failed; input not attempted".into()
                }
                ReturnReason::Preparation(error) => format!("preparation failed: {error:#}"),
            };
            ui.note(format!("{kind} returned: {reason}{fault}. {retention}; {} additional draft(s) · {} cycles without resubmitting", ui.editor.recovered.len(), ui.keys.hint(crate::keybindings::KeyAction::Dequeue)));
        }
        ControlUpdate::Unresolved(input) => {
            let id = ui
                .recovery
                .input(input, super::recovery::Origin::capture(control.session()))?;
            ui.status = format!("Input commit unresolved · /recovery {id} · not replayed");
            ui.note(format!("Input source quarantined as recovery {id}; inspect with /recovery {id}. Not restored or replayed."));
        }
        ControlUpdate::Settled(mut settlement) => {
            ui.command_preview = ion_core::ToolOutputPreview::default();
            ui.retry_progress = None;
            let observed_success = matches!(&settlement.outcome, OperationOutcome::Turn(Ok(_)))
                && ui.conversation.live_for(settlement.operation);
            if observed_success && let Some(live) = ui.conversation.live() {
                for item in &live.projection().items {
                    // The display strips empty text, so an image note could
                    // look like a prompt. Let the exact snapshot supply that
                    // suffix rather than borrowing text from an attachment.
                    if matches!(item, ion_core::TranscriptItem::User(user) if user.parts.iter().any(|part| matches!(part, ion_core::TranscriptPart::Image { .. })))
                    {
                        break;
                    }
                    if let ion_core::TranscriptItem::User(user) = item
                        && let Some(source) = user.source
                        && let Some(text) = user.parts.iter().find_map(|part| match part {
                            ion_core::TranscriptPart::Text(text) => Some(text),
                            _ => None,
                        })
                    {
                        ui.editor.append_prompt_history(source.entry, text);
                    }
                }
            }
            ui.discard_provisional();
            ui.conversation.clear_live();
            let cold_shell_refusal =
                matches!(&settlement.outcome, OperationOutcome::NotDispatched(_));
            let not_dispatched = matches!(
                &settlement.outcome,
                OperationOutcome::NotDispatched(_)
                    | OperationOutcome::Compact(Err(ion_core::AgentError::CompactionAccess(_)))
            );
            let result = match settlement.outcome {
                OperationOutcome::NotDispatched(error) => {
                    Err(error.context("Operation was not dispatched; no executor invoked"))
                }
                OperationOutcome::Turn(Err(ion_core::AgentError::Cancelled))
                    if settlement.stop_requested =>
                {
                    Ok("Turn stopped".into())
                }
                OperationOutcome::Compact(Err(ion_core::AgentError::Cancelled))
                    if settlement.stop_requested =>
                {
                    Ok("Compaction stopped".into())
                }
                OperationOutcome::Turn(result) => result.map(|_| String::new()).map_err(Into::into),
                OperationOutcome::Compact(result) => result
                    .map(|changed| {
                        if changed {
                            "Context summarized; raw history retained".into()
                        } else {
                            "No settled history to summarize".into()
                        }
                    })
                    .map_err(Into::into),
                OperationOutcome::Shell(result) => result
                    .map(|output| {
                        if output.value["cancelled"] == true {
                            "Shell stopped".into()
                        } else if output.is_error {
                            "Shell finished with an error".into()
                        } else {
                            "Shell finished".into()
                        }
                    })
                    .map_err(Into::into),
            };
            let error: Option<anyhow::Error> = match result {
                Ok(status) => {
                    ui.status = status;
                    None
                }
                Err(error) => {
                    ui.status = if not_dispatched {
                        format!("Operation not started: {error:#}")
                    } else {
                        format!(
                            "Operation ended: {error:#}; inspect Session and effects before retrying"
                        )
                    };
                    Some(error)
                }
            };
            // Only cold preflight proves the executor was never invoked.
            // Restore that exact source, never an uncertain executed command.
            if not_dispatched {
                if let Some(source) = settlement.metadata.take() {
                    ui.return_command_source(source);
                }
            } else {
                drop(settlement.metadata);
            }
            if observed_success {
                ui.session_io.schedule_refresh();
            } else if !cold_shell_refusal {
                // Cold refusal did not write Session facts. Preserve any
                // existing refresh requirement, but don't create one here.
                ui.session_io.request_refresh();
            }
            return Ok(error);
        }
        ControlUpdate::QuestionChanged => {}
        ControlUpdate::Idle => {
            ui.retry_progress = None;
            ui.discard_provisional();
            ui.conversation.clear_live();
        }
    }
    Ok(None)
}

pub(super) fn cleanup_update(
    ui: &mut Frontend,
    control: &SessionControl<EditorDraft>,
    update: ControlUpdate<EditorDraft>,
    outcome: &mut Result<()>,
) {
    match apply(ui, control, update) {
        Ok(Some(error)) | Err(error) => combine_error(outcome, error),
        Ok(None) => {}
    }
}

pub(super) fn combine_error(outcome: &mut Result<()>, error: anyhow::Error) {
    *outcome = Err(match std::mem::replace(outcome, Ok(())) {
        Ok(()) => error,
        Err(primary) => {
            primary.context(format!("Concurrent cleanup/settlement failure: {error:#}"))
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn live_preview_replaces_progress_updates_without_altering_captured_bytes() {
        use ion_core::OutputStream;
        let mut preview = ion_core::ToolOutputPreview::default();
        let stdout = b"start\r\n10%\r20%\r\n30%\r40%\r";
        preview.append(OutputStream::Stdout, stdout);
        preview.append(OutputStream::Stderr, b"warning\r\n");
        assert_eq!(
            preview_rows(&preview, 80, 5),
            ["stdout · start", "20%", "40%", "stderr · warning"]
        );
        assert_eq!(preview.bytes(OutputStream::Stdout), stdout);
    }

    #[tokio::test]
    async fn sdk_only_admission_updates_are_invariants_followed_by_owned_shutdown() {
        for recovery in [false, true] {
            let mut fixture = super::super::tests::Fixture::new();
            let mut ui = Frontend::default();
            if recovery {
                fixture
                    .control
                    .launch_recovery(
                        ion_core::RecoveryPoint {
                            session: uuid::Uuid::now_v7(),
                            turn: 1,
                            checkpoint: 1,
                        },
                        EditorDraft::at_end("SDK continuation".into()),
                    )
                    .unwrap();
            } else {
                // Reject before dispatch; this invariant test performs no network I/O.
                rusqlite::Connection::open(fixture.control.session().path()).unwrap()
                    .execute_batch("CREATE TRIGGER no_request BEFORE INSERT ON entries BEGIN SELECT RAISE(ABORT, 'fixture rejection'); END;").unwrap();
                let plan = fixture
                    .control
                    .prepare_request(
                        ion_core::RequestKey::new("fixture".into(), "once".into()).unwrap(),
                        ion_ai::Message::user_input("literal SDK input".into(), []),
                    )
                    .await
                    .unwrap();
                fixture
                    .control
                    .launch_request(plan, EditorDraft::at_end("SDK request".into()))
                    .unwrap();
            }
            let error = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let update = fixture.control.next().await;
                    if matches!(
                        update,
                        ControlUpdate::RequestAdmission(_) | ControlUpdate::RecoveryAdmission(_)
                    ) {
                        break apply(&mut ui, &fixture.control, update).unwrap_err();
                    }
                    apply(&mut ui, &fixture.control, update).unwrap();
                }
            })
            .await
            .unwrap();
            assert!(error.to_string().contains("unexpected SDK admission"));
            let mut outcome = Err(error);
            fixture.control.request_shutdown();
            let report = fixture.control.shutdown().await;
            for update in report.updates {
                cleanup_update(&mut ui, &fixture.control, update, &mut outcome);
            }
            assert!(!fixture.control.is_busy());
            assert!(report.cleanup_errors.is_empty());
            assert!(outcome.is_err());
        }
    }

    #[tokio::test]
    async fn ordinary_returns_and_withdrawals_keep_a_newer_picker_in_control() {
        for withdrawn in [false, true] {
            let mut fixture = super::super::tests::Fixture::new();
            let source = "  queued original 🦀";
            fixture
                .control
                .submit(
                    Delivery::FollowUp,
                    UserInput::Prompt {
                        text: source.into(),
                        attachments: vec![ion_app::attachments::AttachmentSource::ImageFile(
                            fixture.root.join("missing.png"),
                        )],
                    },
                    EditorDraft {
                        literal: source.into(),
                        cursor: 2,
                    },
                )
                .unwrap();
            if withdrawn {
                fixture.control.clear_queue();
            } else {
                fixture.control.request_stop();
            }
            let mut ui = Frontend::default();
            ui.pickers.open(
                super::super::Picker::reasoning_for(
                    fixture.control.agent(),
                    ion_ai::Reasoning::default(),
                )
                .unwrap(),
            );
            super::super::tests::drain(&mut fixture.control, &mut ui).await;
            assert_eq!(
                ui.input_target(),
                super::super::interaction::InputTarget::Picker
            );
            assert!(ui.editor.draft.is_empty());
            assert!(ui.editor.attachments.is_empty());
            assert_eq!(ui.editor.recovered.len(), 1);
            let returned = &ui.editor.recovered[0];
            assert_eq!(
                (returned.source.literal.as_str(), returned.source.cursor),
                (source, 2)
            );
            assert!(
                matches!(&returned.attachments[..], [ion_app::attachments::AttachmentSource::ImageFile(path)] if path.ends_with("missing.png"))
            );
            assert!(fixture.control.session().view().unwrap().entries.is_empty());
            assert!(!fixture.control.is_busy());
            fixture.control.shutdown().await;
        }
    }

    #[tokio::test]
    async fn uncertain_input_is_retained_separately_and_never_restored_or_resent() {
        let mut fixture = super::super::tests::Fixture::new();
        rusqlite::Connection::open(fixture.control.session().path()).unwrap().execute_batch(
            "CREATE TABLE commit_parent (id INTEGER PRIMARY KEY);
             CREATE TABLE commit_child (parent INTEGER REFERENCES commit_parent(id) DEFERRABLE INITIALLY DEFERRED);
             CREATE TRIGGER uncertain_commit AFTER INSERT ON entries
             BEGIN INSERT INTO commit_child(parent) VALUES (999); END;"
        ).unwrap();
        let literal = "  uncertain original 🦀\n";
        fixture
            .control
            .submit(
                Delivery::Start,
                UserInput::Prompt {
                    text: literal.into(),
                    attachments: Vec::new(),
                },
                EditorDraft::at_end(literal.into()),
            )
            .unwrap();
        let mut ui = Frontend::default();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut unresolved = false;
            loop {
                let update = fixture.control.next().await;
                unresolved |= matches!(update, ControlUpdate::Unresolved(_));
                let settled = matches!(update, ControlUpdate::Settled(_));
                apply(&mut ui, &fixture.control, update).unwrap();
                if settled {
                    assert!(
                        unresolved,
                        "exercise the real unresolved → settled ordering"
                    );
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(ui.editor.draft.is_empty());
        assert!(ui.editor.recovered.is_empty());
        assert!(ui.recovery.blocks(fixture.control.session().id()));
        assert_eq!(ui.recovery.list().lines().count(), 2);
        let id = ui
            .recovery
            .list()
            .lines()
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        let transcript = ui
            .pending_history_rows(200)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            transcript.contains(&format!("/recovery {id}")),
            "settlement must not hide the recovery action"
        );
        assert!(transcript.contains("quarantined"));
        assert!(
            !transcript.contains(literal.trim()),
            "the notice must not dump the source"
        );
        super::super::interaction::submit_editor(
            &mut fixture.control,
            &mut ui,
            Delivery::Start,
            EditorDraft::at_end("do not send".into()),
        );
        assert_eq!(ui.editor.draft, "do not send");
        assert!(ui.status.contains("/recovery"));
        assert_eq!(ui.recovery.list().lines().count(), 2);
        fixture.control.shutdown().await;
    }

    #[tokio::test]
    async fn maintenance_is_not_input_preparation_or_stale_core_stop_intent() {
        let mut fixture = super::super::tests::Fixture::new();
        let id = fixture
            .control
            .compact(None, EditorDraft::at_end(String::new()))
            .unwrap();
        for stopping in [false, true] {
            let active = ActiveOperation::from_state(ion_app::ControlState {
                operation: None,
                mcp_reconnect: Some(ion_app::McpReconnectState {
                    id,
                    stop_requested: stopping,
                }),
                stop_requested: !stopping,
                busy: true,
                ..fixture.control.state()
            })
            .unwrap();
            assert!(matches!(active.kind, OperationKind::McpReconnect));
            assert_eq!(active.is_cancelled(), stopping);
            assert_eq!(
                active.label(),
                if stopping {
                    "Cancelling MCP reconnect…"
                } else {
                    "Reconnecting MCP"
                }
            );
            assert!(!active.controls(&Frontend::default()).contains("steers"));
        }
        fixture.control.shutdown().await;
    }

    #[tokio::test]
    async fn details_reading_survives_commit_and_retires_restarted_or_lost_streams() {
        use ion_ai::{Content, Message, ResponseTermination};
        let mut fixture = super::super::tests::Fixture::new();
        let operation = fixture
            .control
            .compact(None, EditorDraft::at_end(String::new()))
            .unwrap();
        let mut ui = Frontend::default();
        ui.load_history(
            fixture.control.session(),
            &fixture.control.session().view().unwrap(),
        );
        let event = |event| {
            ControlUpdate::Activity(Observation::Event {
                operation,
                event: Arc::new(event),
            })
        };
        let body = (0..100)
            .map(|index| format!("BODY_{index:03}\n"))
            .collect::<String>();
        for value in [
            AgentEvent::TurnAccepted {
                entry: 1,
                turn: 1,
                input: Message::user_input("request".into(), []),
            },
            AgentEvent::ThinkingDelta {
                block: 0,
                text: "thinking\n".repeat(40),
            },
            AgentEvent::TextDelta {
                block: 0,
                text: body.clone(),
            },
        ] {
            apply(&mut ui, &fixture.control, event(value)).unwrap();
        }
        ui.open_details(None);
        super::super::modal_rows(&mut ui, None, 80, 12);
        ui.details.as_mut().unwrap().scroll_by(-30);
        let before = super::super::modal_rows(&mut ui, None, 80, 12).0[0].to_string();
        assert!(before.starts_with("BODY_"));
        apply(
            &mut ui,
            &fixture.control,
            event(AgentEvent::AssistantCommitted {
                entry: 2,
                turn: 1,
                content: vec![
                    Content::Thinking("thinking\n".repeat(40)),
                    Content::Text(body),
                ],
                tool_activities: vec![],
                termination: ResponseTermination::Completed,
            }),
        )
        .unwrap();
        assert_eq!(
            super::super::modal_rows(&mut ui, None, 80, 12).0[0].to_string(),
            before
        );
        // Commit splits the streamed parts into separately sourced entries.
        // A refresh must keep the text part, not adopt the earlier thinking part.
        ui.details.as_mut().unwrap().invalidate();
        assert_eq!(
            super::super::modal_rows(&mut ui, None, 30, 16).0[0].to_string(),
            before
        );
        apply(
            &mut ui,
            &fixture.control,
            event(AgentEvent::TextDelta {
                block: 0,
                text: "RETRY_BODY\n".repeat(100),
            }),
        )
        .unwrap();
        super::super::modal_rows(&mut ui, None, 80, 12);
        // Reading pins the older page; Later deliberately enters the new response.
        ui.details
            .as_mut()
            .unwrap()
            .navigate(crate::keybindings::KeyAction::Later);
        super::super::modal_rows(&mut ui, None, 80, 12);
        ui.details.as_mut().unwrap().scroll_by(-30);
        assert!(
            super::super::modal_rows(&mut ui, None, 80, 12).0[0]
                .to_string()
                .starts_with("RETRY_BODY")
        );
        apply(
            &mut ui,
            &fixture.control,
            event(AgentEvent::ResponseRestarted),
        )
        .unwrap();
        apply(
            &mut ui,
            &fixture.control,
            event(AgentEvent::TextDelta {
                block: 0,
                text: "NEW_RESPONSE\n".repeat(100),
            }),
        )
        .unwrap();
        let retired = super::super::modal_rows(&mut ui, None, 80, 12).0;
        assert!(
            !retired
                .iter()
                .any(|row| row.to_string().contains("NEW_RESPONSE"))
        );
        assert!(retired.iter().any(|row| row.to_string().contains("BODY_")));
        // Losing an anchored stream follows the same retirement path.
        ui.details
            .as_mut()
            .unwrap()
            .navigate(crate::keybindings::KeyAction::Later);
        super::super::modal_rows(&mut ui, None, 80, 12);
        ui.details.as_mut().unwrap().scroll_by(-30);
        assert!(
            super::super::modal_rows(&mut ui, None, 80, 12).0[0]
                .to_string()
                .starts_with("NEW_RESPONSE")
        );
        apply(
            &mut ui,
            &fixture.control,
            ControlUpdate::Activity(Observation::Gap { skipped: 3 }),
        )
        .unwrap();
        let after_gap = super::super::modal_rows(&mut ui, None, 80, 12).0;
        assert!(
            !after_gap
                .iter()
                .any(|row| row.to_string().contains("NEW_RESPONSE"))
        );
        assert!(
            after_gap
                .iter()
                .any(|row| row.to_string().contains("BODY_"))
        );
        fixture.control.shutdown().await;
    }

    #[tokio::test]
    async fn manual_compaction_retry_is_visible_without_a_live_turn() {
        let mut fixture = super::super::tests::Fixture::new();
        let id = fixture
            .control
            .compact(None, EditorDraft::at_end("compact".into()))
            .unwrap();
        let mut ui = Frontend::default();
        apply(
            &mut ui,
            &fixture.control,
            ControlUpdate::Activity(Observation::Event {
                operation: id,
                event: Arc::new(AgentEvent::ContextCompactionRetry {
                    through_entry: 10,
                    next_through_entry: 4,
                }),
            }),
        )
        .unwrap();
        assert!(ui.conversation.live().is_none());
        let rows = ui.pending_history_rows(80);
        assert!(rows.iter().any(|row| {
            row.to_string()
                .contains("summary request too large; retrying through entry 4")
        }));
        fixture.control.shutdown().await;
    }

    #[tokio::test]
    async fn stopped_initial_preparation_does_not_lose_its_ui_stop_intent() {
        let mut fixture = super::super::tests::Fixture::new();
        fixture
            .control
            .submit(
                Delivery::Start,
                UserInput::Prompt {
                    text: "initial".into(),
                    attachments: vec![ion_app::attachments::AttachmentSource::ImageFile(
                        fixture.root.join("missing.png"),
                    )],
                },
                EditorDraft::at_end("initial".into()),
            )
            .unwrap();
        fixture.control.request_stop();
        let update = fixture.control.next().await;
        assert!(matches!(
            update,
            ControlUpdate::Input {
                phase: InputPhase::Preparing,
                ..
            }
        ));
        assert!(
            apply(&mut Frontend::default(), &fixture.control, update)
                .unwrap()
                .is_none()
        );
        assert!(
            ActiveOperation::from_state(fixture.control.state())
                .unwrap()
                .is_cancelled()
        );
        fixture.control.shutdown().await;
    }

    #[tokio::test]
    async fn resync_discards_partial_projection_and_avoids_duplicate_ahead_history() {
        let mut fixture = super::super::tests::Fixture::new();
        let mut ui = Frontend::default();
        ui.load_history(
            fixture.control.session(),
            &fixture.control.session().view().unwrap(),
        );
        fixture
            .control
            .submit(
                Delivery::Start,
                UserInput::Prompt {
                    text: "request".into(),
                    attachments: vec![],
                },
                EditorDraft::at_end("request".into()),
            )
            .unwrap();
        let (operation, accepted) =
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let update = fixture.control.next().await;
                    if let ControlUpdate::Activity(Observation::Event { operation, event }) =
                        &update
                        && matches!(event.as_ref(), AgentEvent::TurnAccepted { .. })
                    {
                        break (*operation, event.clone());
                    }
                    apply(&mut ui, &fixture.control, update).unwrap();
                }
            })
            .await
            .unwrap();
        let event = |event| ControlUpdate::Activity(Observation::Event { operation, event });
        apply(&mut ui, &fixture.control, event(accepted.clone())).unwrap();
        apply(
            &mut ui,
            &fixture.control,
            event(Arc::new(AgentEvent::TextDelta {
                block: 0,
                text: "lost prefix".into(),
            })),
        )
        .unwrap();
        assert!(ui.conversation.live().is_some());
        super::super::conversation::fixtures::publish(&mut ui.conversation);

        apply(
            &mut ui,
            &fixture.control,
            ControlUpdate::Activity(Observation::Gap { skipped: 2 }),
        )
        .unwrap();
        for value in [
            AgentEvent::ResponseRestarted,
            AgentEvent::TextDelta {
                block: 0,
                text: "surviving tail".into(),
            },
        ] {
            apply(&mut ui, &fixture.control, event(Arc::new(value))).unwrap();
        }
        super::super::tests::settle_io(&mut fixture.control, &mut ui).await;
        assert!(ui.conversation.live().is_none());
        assert_eq!(
            ui.conversation.items().cloned().collect::<Vec<_>>(),
            ion_core::TranscriptProjection::from_session(
                &fixture.control.session().view().unwrap()
            )
            .items
        );
        assert!(ui.conversation.has_published_prefix());
        assert!(!ui.pending_history_rows(80).iter().any(|row| {
            let text = row.to_string();
            text.contains("lost prefix")
                || text.contains("surviving tail")
                || text.contains("request")
        }));
        // A delayed acceptance after the canonical snapshot must not seed it twice.
        apply(&mut ui, &fixture.control, event(accepted)).unwrap();
        assert!(ui.conversation.live().is_none());
        assert_eq!(
            ui.conversation
                .items()
                .filter(|item| matches!(item, ion_core::TranscriptItem::User(_)))
                .count(),
            1
        );
        fixture.control.shutdown().await;
    }
}
