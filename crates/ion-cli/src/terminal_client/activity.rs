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
    pub(super) pending_follow_ups: usize,
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
            pending_follow_ups: state.pending_follow_ups,
        })
    }

    pub(super) fn is_cancelled(self) -> bool {
        self.stop_requested
    }

    pub(super) fn controls(self, keys: &crate::keybindings::KeyBindings) -> String {
        use crate::keybindings::KeyAction;
        let submit = keys.hint(KeyAction::Submit);
        let follow_up = keys.hint(KeyAction::FollowUp);
        let cancel = keys.hint(KeyAction::Interrupt);
        match self.kind {
            OperationKind::Coding => {
                format!("{cancel} cancels · {submit} steers · {follow_up} queues")
            }
            OperationKind::Shell | OperationKind::Compaction | OperationKind::McpReconnect => {
                format!("{cancel} cancels · {submit} queues")
            }
            OperationKind::Preparation => format!("{cancel} cancels · {follow_up} queues"),
            OperationKind::Binding => format!("{cancel} cancels · input waits for Session refresh"),
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
        ControlUpdate::RequestAdmission(decision) => match decision.result {
            Ok(ion_app::RequestAdmission::Started { .. }) => {
                ui.status = "Request durably accepted".into();
                ui.session_io.request_refresh();
            }
            Ok(ion_app::RequestAdmission::Existing { receipt, metadata }) => {
                ui.note(format!("Request already exists in Session {}; not replayed. Original attempt retained separately: {}", receipt.session, metadata.literal));
                ui.existing_requests.push((receipt, metadata));
                ui.session_io.request_refresh();
            }
            Ok(ion_app::RequestAdmission::Unresolved(input)) => {
                ui.status = "Request admission unknown; inspect before retrying".into();
                ui.note(format!("Request admission unresolved in Session {}; original retained separately, not restored: {}", input.session, input.metadata.literal));
                ui.unresolved_requests.push(input);
                ui.session_io.request_refresh();
            }
            Err(rejected) => {
                let rejected = *rejected;
                ui.status = format!("Request was not accepted: {:#}", rejected.reason);
                if let UserInput::Prompt { images, files, .. } = rejected.input {
                    ui.return_draft(rejected.metadata, images, files);
                } else {
                    // A prepared SDK Message is not an editor draft. Keep the
                    // entire input, including media, rather than reconstruct it.
                    ui.note(format!(
                        "Prepared request was not accepted; original retained separately: {}",
                        rejected.metadata.literal
                    ));
                    ui.rejected_requests.push(rejected);
                }
            }
        },
        ControlUpdate::RecoveryAdmission(decision) => match decision.result {
            Ok(_) => {
                ui.status = "Continuation admitted; no input replayed".into();
                ui.session_io.request_refresh();
            }
            Err(failure) => {
                ui.status = format!(
                    "Continuation admission {:?}: {:#}",
                    failure.kind, failure.reason
                );
                if failure.kind == ion_app::AdmissionFailureKind::NotAccepted {
                    ui.restore_editor(failure.metadata);
                } else {
                    ui.note(format!("Continuation outcome unknown; original retained separately, not restored: {}", failure.metadata.literal));
                    ui.unresolved_recoveries.push(failure);
                }
            }
        },
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
            if matches!(event.as_ref(), AgentEvent::ContextCompactionRetry { .. })
                && !ui.conversation.live_for(operation)
                && let Some(note) = crate::agent_events::diagnostic(&event)
            {
                ui.note(note);
            }
            if matches!(event.as_ref(), AgentEvent::ResponseRestarted) {
                ui.view.discard_provisional();
            }
            if matches!(event.as_ref(), AgentEvent::AssistantCommitted { .. })
                && ui.conversation.live_for(operation)
                && let Some(live) = ui.conversation.live()
            {
                ui.view.commit_response(live, &event);
            }
            if ui.conversation.observe(operation, &event) {
                ui.session_io.request_refresh();
            }
        }
        ControlUpdate::Activity(Observation::Gap { .. }) => {
            // Drop the whole partial projection. Host may resume observations
            // at a committed boundary, but LiveTranscript's tool/child indexes
            // require an uninterrupted Turn. Stay on committed facts until the
            // next Turn instead of inventing a second full-history snapshot.
            ui.conversation.clear_live();
            ui.view.discard_provisional();
            ui.note("Live updates skipped; showing committed Session facts".into());
            ui.session_io.request_refresh();
        }
        ControlUpdate::Returned(returned) => {
            let UserInput::Prompt { images, files, .. } = returned.input else {
                unreachable!("terminal submits only raw prompts")
            };
            if matches!(returned.reason, ReturnReason::Cleared) {
                ui.show_draft(super::RecoveredDraft {
                    source: returned.metadata,
                    images,
                    files,
                });
            } else {
                ui.return_draft(returned.metadata, images, files);
            }
            ui.status = "Draft returned".into();
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
            ui.note(format!("{kind} returned: {reason}{fault}. Draft retained; {} additional draft(s) · {} cycles without resubmitting", ui.editor.recovered.len(), ui.keys.hint(crate::keybindings::KeyAction::Dequeue)));
        }
        ControlUpdate::Unresolved(input) => {
            ui.status = "Input commit unresolved; inspect Session before retrying".into();
            ui.note(format!("Input commit is unresolved in Session {}. Original retained separately, not restored for retry:\n{}", input.session, input.metadata.literal));
            ui.unresolved_inputs.push(input);
        }
        ControlUpdate::Settled(mut settlement) => {
            ui.conversation.clear_live();
            let not_dispatched = matches!(&settlement.outcome, OperationOutcome::NotDispatched(_));
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
                        if output.is_error {
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
                    ui.status = format!(
                        "Operation ended: {error:#}; inspect Session and effects before retrying"
                    );
                    Some(error)
                }
            };
            // Only cold preflight proves the executor was never invoked.
            // Restore that exact source, never an uncertain executed command.
            if not_dispatched {
                if let Some(source) = settlement.metadata.take() {
                    ui.restore_editor(source);
                }
            } else {
                drop(settlement.metadata);
            }
            ui.session_io.request_refresh();
            return Ok(error);
        }
        ControlUpdate::QuestionChanged => {}
        ControlUpdate::Idle => {
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
                    images: Vec::new(),
                    files: Vec::new(),
                },
                EditorDraft::at_end(literal.into()),
            )
            .unwrap();
        let mut ui = Frontend::default();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let update = fixture.control.next().await;
                if matches!(update, ControlUpdate::Unresolved(_)) {
                    apply(&mut ui, &fixture.control, update).unwrap();
                    break;
                }
            }
        })
        .await
        .unwrap();
        assert!(ui.editor.draft.is_empty());
        assert!(ui.editor.recovered.is_empty());
        assert_eq!(ui.unresolved_inputs.len(), 1);
        assert_eq!(ui.unresolved_inputs[0].metadata.literal, literal);
        super::super::interaction::submit_editor(
            &mut fixture.control,
            &mut ui,
            Delivery::Start,
            EditorDraft::at_end("do not send".into()),
        );
        assert_eq!(ui.editor.draft, "do not send");
        assert!(ui.status.contains("reconcile this Session"));
        assert_eq!(ui.unresolved_inputs.len(), 1);
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
            assert!(
                !active
                    .controls(&Frontend::default().keys)
                    .contains("steers")
            );
        }
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
                    files: Vec::new(),
                    text: "initial".into(),
                    images: vec![ion_app::image_input::ImageSource::Path(
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
                    files: vec![],
                    images: vec![],
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
            event(Arc::new(AgentEvent::TextDelta("lost prefix".into()))),
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
            AgentEvent::TextDelta("surviving tail".into()),
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
