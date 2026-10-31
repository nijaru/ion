//! Provisional terminal projection of shared control updates, not execution custody.
use anyhow::{Context, Result};
use ion_core::{AgentEvent, LiveTranscript};
use ion_host::{
    ControlUpdate, Delivery, InputPhase, Observation, OperationId, OperationOutcome, ReturnReason,
    SessionControl, UserInput,
};

use super::{EditorDraft, Frontend};

#[derive(Clone, Copy)]
pub(super) enum OperationKind {
    Coding,
    Shell,
    Compaction,
    Preparation,
    McpReconnect,
}

#[derive(Clone, Copy)]
pub(super) struct ActiveOperation {
    pub(super) kind: OperationKind,
    pub(super) stop_requested: bool,
    pub(super) pending_follow_ups: usize,
}

impl ActiveOperation {
    pub(super) fn from_state(state: ion_host::ControlState) -> Option<Self> {
        let (kind, stop_requested) = match state.operation {
            Some(operation) => (
                match operation.kind {
                    ion_host::OperationKind::Turn => OperationKind::Coding,
                    ion_host::OperationKind::Shell => OperationKind::Shell,
                    ion_host::OperationKind::Compact => OperationKind::Compaction,
                },
                state.stop_requested,
            ),
            None => match state.mcp_reconnect {
                Some(reconnect) => (OperationKind::McpReconnect, reconnect.stop_requested),
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
                format!("{submit} steers · {follow_up} queues · {cancel} cancels")
            }
            OperationKind::Shell | OperationKind::Compaction | OperationKind::McpReconnect => {
                format!("{submit} queues · {cancel} cancels")
            }
            OperationKind::Preparation => format!("{follow_up} queues · {cancel} cancels"),
        }
    }

    pub(super) fn label(self) -> &'static str {
        match (self.kind, self.stop_requested) {
            (OperationKind::Coding, true) => "Cancelling turn…",
            (OperationKind::Shell, true) => "Cancelling shell…",
            (OperationKind::Compaction, true) => "Cancelling compaction…",
            (OperationKind::Preparation, true) => "Cancelling input…",
            (OperationKind::McpReconnect, true) => "Cancelling MCP reconnect…",
            (OperationKind::Coding, false) => "Working",
            (OperationKind::Shell, false) => "Running shell",
            (OperationKind::Compaction, false) => "Summarizing context",
            (OperationKind::Preparation, false) => "Preparing input",
            (OperationKind::McpReconnect, false) => "Reconnecting MCP",
        }
    }
}

#[derive(Default)]
pub(super) struct Activity {
    live: Option<(OperationId, LiveTranscript)>,
}

impl Activity {
    pub(super) fn live(&self) -> Option<&LiveTranscript> {
        self.live.as_ref().map(|(_, live)| live)
    }

    /// Returned errors have already been made visible in UI status. The caller
    /// may keep interacting after an ordinary operation failure; shutdown instead
    /// combines those failures with its primary I/O error.
    pub(super) fn update(
        &mut self,
        ui: &mut Frontend,
        control: &SessionControl<EditorDraft>,
        update: ControlUpdate<EditorDraft>,
    ) -> Result<Option<anyhow::Error>> {
        match update {
            ControlUpdate::McpReconnectFinished(finished) => match finished.result {
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
            },
            ControlUpdate::Automated { .. } => {
                self.live = None;
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
                    && self.live.as_ref().is_none_or(|(id, _)| *id != operation)
                    && let Some(note) = crate::agent_events::diagnostic(&event)
                {
                    // Manual compaction has no live Turn transcript.
                    ui.note(note);
                }
                if let AgentEvent::TurnAccepted { turn, .. }
                | AgentEvent::CompletionCommitted {
                    turn,
                    new_turn: true,
                    ..
                } = event.as_ref()
                {
                    // A fresh typed Turn start is the only safe start for this strict
                    // projection. It is not a tolerant reader for missing tool or
                    // thinking prefixes after an observation gap.
                    if ui
                        .history
                        .items
                        .iter()
                        .any(|item| super::transcript_item_turn(item) == Some(*turn))
                    {
                        // An earlier inspection can already include the next Turn.
                        // Never layer replayed progress over those same facts.
                        self.live = None;
                        return Ok(None);
                    }
                    let mut live = LiveTranscript::default();
                    live.observe(&event);
                    self.live = Some((operation, live));
                } else if let Some((id, live)) = &mut self.live
                    && *id == operation
                {
                    live.observe(&event);
                } else if matches!(
                    event.as_ref(),
                    AgentEvent::AssistantCommitted { .. }
                        | AgentEvent::SteeringCommitted { .. }
                        | AgentEvent::CompletionCommitted { .. }
                        | AgentEvent::ToolFinished { .. }
                        | AgentEvent::ChildToolAdmitted { .. }
                        | AgentEvent::ChildToolFinished { .. }
                        | AgentEvent::TurnEnded { .. }
                ) {
                    // After a gap, keep showing newly committed work without
                    // rebuilding a strict live stream from a surviving suffix.
                    return Ok(reconcile(ui, control).err());
                }
            }
            ControlUpdate::Activity(Observation::Gap { .. }) => {
                // Drop the whole partial projection. Host may resume observations
                // at a committed boundary, but LiveTranscript's tool/child indexes
                // require an uninterrupted Turn. Stay on committed facts until the
                // next Turn instead of inventing a second full-history snapshot.
                self.live = None;
                ui.note("Live updates skipped; showing committed Session facts".into());
                return Ok(reconcile(ui, control).err());
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
                    ReturnReason::Preparation(error) => format!("preparation failed: {error:#}"),
                };
                ui.note(format!("{kind} returned: {reason}{fault}. Draft retained; {} additional draft(s) · {} cycles without resubmitting", ui.recovered.len(), ui.keys.hint(crate::keybindings::KeyAction::Dequeue)));
            }
            ControlUpdate::Settled(settlement) => {
                self.live = None;
                let result = match settlement.outcome {
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
                    OperationOutcome::Turn(result) => {
                        result.map(|_| String::new()).map_err(Into::into)
                    }
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
                // Direct-operation metadata is correlation, not Returned input.
                // Do not revive an uncertain command in the composer/recovery
                // queue. Explicit command recall remains in editor history.
                drop(settlement.metadata);
                if let Err(refresh) = reconcile(ui, control) {
                    let refresh = match error {
                        Some(error) => {
                            refresh.context(format!("Concurrent settlement failure: {error:#}"))
                        }
                        None => refresh,
                    };
                    // Keep the concise history-failure status visible even in
                    // narrow/fullscreen viewports; retain concurrent details.
                    ui.note(format!("Operation/inspection failure: {refresh:#}"));
                    return Ok(Some(refresh));
                }
                return Ok(error);
            }
            ControlUpdate::QuestionChanged => {}
            ControlUpdate::Idle => {
                self.live = None;
            }
        }
        Ok(None)
    }

    pub(super) fn cleanup_update(
        &mut self,
        ui: &mut Frontend,
        control: &SessionControl<EditorDraft>,
        update: ControlUpdate<EditorDraft>,
        outcome: &mut Result<()>,
    ) {
        match self.update(ui, control, update) {
            Ok(Some(error)) | Err(error) => combine_error(outcome, error),
            Ok(None) => {}
        }
    }
}

fn reconcile(ui: &mut Frontend, control: &SessionControl<EditorDraft>) -> Result<()> {
    let view = control
        .session()
        .view()
        .context("Session history refresh failed")
        .inspect_err(|error| ui.status = format!("{error:#}"))?;
    ui.load_history(control.session(), &view);
    ui.scroll = 0;
    Ok(())
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
    async fn maintenance_is_not_input_preparation_or_stale_core_stop_intent() {
        let mut fixture = super::super::tests::Fixture::new();
        let id = fixture
            .control
            .compact(None, EditorDraft::at_end(String::new()))
            .unwrap();
        for stopping in [false, true] {
            let active = ActiveOperation::from_state(ion_host::ControlState {
                operation: None,
                mcp_reconnect: Some(ion_host::McpReconnectState {
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
        let mut activity = Activity::default();
        let mut ui = Frontend::default();
        activity
            .update(
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
        assert!(activity.live().is_none());
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
                    images: vec![ion_host::image_input::ImageSource::Path(
                        fixture.root.join("missing.png"),
                    )],
                },
                EditorDraft::at_end("initial".into()),
            )
            .unwrap();
        let mut activity = Activity::default();
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
            activity
                .update(&mut Frontend::default(), &fixture.control, update)
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
        let id = fixture
            .control
            .compact(None, super::super::EditorDraft::at_end("compact".into()))
            .unwrap();
        let mut live = LiveTranscript::default();
        live.observe(&AgentEvent::TurnAccepted {
            turn: 1,
            input: ion_ai::Message::user_input("request".into(), []),
        });
        live.observe(&AgentEvent::TextDelta("lost prefix".into()));
        let mut activity = Activity {
            live: Some((id, live)),
        };
        let mut ui = Frontend::default();
        assert!(
            activity
                .update(
                    &mut ui,
                    &fixture.control,
                    ControlUpdate::Activity(Observation::Gap { skipped: 2 })
                )
                .unwrap()
                .is_none()
        );
        assert!(activity.live().is_none());
        for event in [
            AgentEvent::ResponseRestarted,
            AgentEvent::TextDelta("surviving tail".into()),
        ] {
            assert!(
                activity
                    .update(
                        &mut ui,
                        &fixture.control,
                        ControlUpdate::Activity(Observation::Event {
                            operation: id,
                            event: Arc::new(event)
                        })
                    )
                    .unwrap()
                    .is_none()
            );
        }
        assert!(activity.live().is_none());
        assert_eq!(
            ui.history,
            ion_core::TranscriptProjection::from_session(
                &fixture.control.session().view().unwrap()
            )
        );
        ui.history.items.push(ion_core::TranscriptItem::User(
            ion_core::TranscriptMessage {
                turn: Some(2),
                steering: false,
                parts: Vec::new(),
            },
        ));
        assert!(
            activity
                .update(
                    &mut ui,
                    &fixture.control,
                    ControlUpdate::Activity(Observation::Event {
                        operation: id,
                        event: Arc::new(AgentEvent::TurnAccepted {
                            turn: 2,
                            input: ion_ai::Message::user_input("next request".into(), []),
                        }),
                    })
                )
                .unwrap()
                .is_none()
        );
        assert!(activity.live().is_none());
        fixture.control.shutdown().await;
    }
}
