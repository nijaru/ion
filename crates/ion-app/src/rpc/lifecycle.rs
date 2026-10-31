//! Reliable Host facts and explicitly lossy passive observations.
use super::*;
use crate::agent_events::event_record;
use ion_host::{ControlUpdate, Observation, OperationOutcome, ReturnReason, ReturnedInput};
use tokio::io::AsyncWrite;

impl Control {
    pub(super) fn mark_gap(&mut self, skipped: u64) {
        if !self.suppress_activity {
            self.gap = Some(self.gap.unwrap_or(0).saturating_add(skipped));
        }
        self.suppress_activity = true;
    }
    pub(super) fn publish_gap<W: AsyncWrite + Unpin>(
        &mut self,
        output: &mut Output<W>,
    ) -> Result<()> {
        if self.has_gap() && output.can_acquire() && output.try_enqueue(self.gap_record())? {
            self.reset_gap();
        }
        Ok(())
    }

    pub(super) fn has_gap(&self) -> bool {
        self.gap.is_some() || self.refused != 0 || self.unacknowledged_stops != 0
    }
    fn reset_gap(&mut self) {
        self.gap = None;
        self.refused = 0;
        self.unacknowledged_stops = 0;
    }
    fn gap_record(&self) -> Value {
        json!({"type":"publication_gap","skipped_at_least":self.gap.unwrap_or(0),"commands_not_admitted":self.refused,"stop_requests_without_reply":self.unacknowledged_stops,"resync":"inspect","provisional":if self.gap.is_some() || self.suppress_activity { "discard_until_settlement" } else { "unchanged" }})
    }

    /// All encoding is synchronous: once borrowed next() yields a reliable
    /// update it is owned here before any further await or selectable write.
    pub(super) fn update<W: AsyncWrite + Unpin>(
        &mut self,
        update: ControlUpdate<RpcMetadata>,
        output: &mut Output<W>,
    ) -> Result<()> {
        match update {
            ControlUpdate::McpReconnectFinished(finished) => {
                let mut record = json!({"type":"mcp_reconnect_finished","id":finished.metadata.id,
                    "operation":finished.operation,"server":finished.server,"stop_requested":finished.stop_requested});
                match finished.result {
                    Ok(report) => {
                        record["success"] = json!(true);
                        record["diagnostics"] = json!(report.diagnostics);
                    }
                    Err(error) => {
                        record["success"] = json!(false);
                        record["error"] = json!(format!("{error:#}"));
                    }
                }
                if output.is_healthy() {
                    output.enqueue_reliable(record, finished.metadata.credit)?;
                }
            }
            ControlUpdate::Automated { operation, turn } => {
                ensure!(
                    self.turn_reply.is_none(),
                    "automated Turn cannot replace a client reply"
                );
                self.turn_reply = Some(OperationReply {
                    operation,
                    turn,
                    id: None,
                    credit: None,
                });
                if let Some(turn) = turn
                    && !output.try_enqueue(json!({"type":"automated_turn_started","operation":operation,"turn":turn,"resync":"inspect"}))?
                {
                    self.mark_gap(1);
                }
            }
            ControlUpdate::Input { id, phase, .. } => {
                let item = self
                    .correlations
                    .get_mut(&id)
                    .context("Host input has protocol correlation")?;
                if phase == InputPhase::Queued && !item.acknowledged && item.command != "prompt" {
                    item.acknowledged = true;
                    let mut data = json!({"disposition":"queued"});
                    if item.command == "follow_up" {
                        data["position"] = json!(item.position);
                    }
                    if output.is_healthy() {
                        output.enqueue_reliable(
                            success(item.id.clone(), item.command, data),
                            item.credit.clone(),
                        )?;
                    }
                }
            }
            ControlUpdate::Dispatched { id, operation } => {
                let item = self
                    .correlations
                    .get(&id)
                    .context("dispatched Host input has protocol correlation")?;
                self.turn_reply = Some(OperationReply {
                    operation,
                    id: item.id.clone(),
                    credit: Some(item.credit.clone()),
                    turn: item.turn,
                });
            }
            ControlUpdate::Accepted {
                id,
                operation,
                turn,
            } => {
                let item = self
                    .correlations
                    .get_mut(&id)
                    .context("accepted Host input has protocol correlation")?;
                if item.command != "steer" {
                    self.turn_reply = Some(OperationReply {
                        operation,
                        id: item.id.clone(),
                        credit: Some(item.credit.clone()),
                        turn: Some(turn),
                    });
                    let record = match item.command {
                        "submit_request" => {
                            ensure!(item.turn == Some(turn), "request receipt Turn mismatch");
                            // The synchronous durable receipt already acknowledged
                            // this input; the callback must not issue a second reply.
                            None
                        }
                        "prompt" => Some(success(
                            item.id.clone(),
                            "prompt",
                            json!({"disposition":"started","turn":turn}),
                        )),
                        _ => Some(json!({"type":"follow_up_started","id":item.id,"turn":turn})),
                    };
                    item.acknowledged = true;
                    if let Some(record) = record
                        && output.is_healthy()
                    {
                        output.enqueue_reliable(record, item.credit.clone())?;
                    }
                }
            }
            ControlUpdate::Returned(returned) => self.returned(returned, output)?,
            ControlUpdate::Settled(settlement) => {
                // Settlement is authoritative even if no provisional event fit.
                let progress_complete = !self.suppress_activity && self.gap.is_none();
                self.suppress_activity = false;
                let mut fatal = None;
                let (record, credit) = match settlement.outcome {
                    OperationOutcome::Turn(result) => {
                        if let Err(error) = &result {
                            fatal = unknown_agent_task(error);
                        }
                        let reply = self
                            .turn_reply
                            .take()
                            .context("dispatched Turn retains protocol reply correlation")?;
                        ensure!(
                            reply.operation == settlement.operation,
                            "Turn reply operation identity mismatch"
                        );
                        let status = match &result {
                            Ok(_) => "completed",
                            Err(ion_core::AgentError::Cancelled) => "cancelled",
                            Err(_) => "failed",
                        };
                        let mut record = if let Some(turn) = reply.turn {
                            json!({"type":"turn_end","turn":turn,"status":status})
                        } else {
                            json!({"type":"operation_failed","operation":"turn","id":reply.id,"status":status})
                        };
                        match result {
                            Ok(text) => record["text"] = json!(text),
                            Err(error) => record["error"] = json!(format!("{error:#}")),
                        }
                        (record, reply.credit)
                    }
                    OperationOutcome::Compact(result) => {
                        if let Err(error) = &result {
                            fatal = unknown_agent_task(error);
                        }
                        let metadata = settlement
                            .metadata
                            .context("compaction retains protocol metadata")?;
                        let mut record = json!({"type":"compact_end","id":metadata.id,"status":match &result {
                            Ok(_) => "completed", Err(ion_core::AgentError::Cancelled) => "cancelled", Err(_) => "failed",
                        }});
                        match result {
                            Ok(changed) => record["changed"] = json!(changed),
                            Err(error) => record["error"] = json!(format!("{error:#}")),
                        }
                        (record, Some(metadata.credit))
                    }
                    OperationOutcome::Shell(result) => {
                        if let Err(error) = &result {
                            fatal = unknown_session_task(error);
                        }
                        let metadata = settlement
                            .metadata
                            .context("shell retains protocol metadata")?;
                        let record = match result {
                            Ok(output) => {
                                let status = if output.value["cancelled"] == true {
                                    "cancelled"
                                } else if output.is_error {
                                    "failed"
                                } else {
                                    "completed"
                                };
                                json!({"type":"shell_end","id":metadata.id,"status":status,"outcome":ion_core::UserShellOutcome::Observed { output: output.value, is_error: output.is_error }})
                            }
                            Err(error) => {
                                json!({"type":"shell_end","id":metadata.id,"status":"failed","error":format!("{error:#}")})
                            }
                        };
                        (record, Some(metadata.credit))
                    }
                };
                for accepted in settlement.accepted {
                    self.correlations.remove(&accepted.id);
                }
                let mut record = record;
                record["stop_requested"] = json!(settlement.stop_requested);
                record["progress_complete"] = json!(progress_complete);
                record["resync"] = json!("inspect");
                if output.is_healthy() {
                    if let Some(credit) = credit {
                        if self.has_gap() {
                            output.enqueue_reliable(self.gap_record(), credit.clone())?;
                            self.reset_gap();
                        }
                        output.enqueue_reliable(record, credit)?;
                    } else {
                        record["automated"] = json!(true);
                        record
                            .as_object_mut()
                            .expect("lifecycle object")
                            .remove("id");
                        if !output.try_enqueue(record)? {
                            self.mark_gap(1);
                        }
                    }
                }
                if let Some(error) = fatal {
                    return Err(error);
                }
            }
            ControlUpdate::Activity(Observation::Gap { skipped }) => self.mark_gap(skipped),
            ControlUpdate::Activity(Observation::Event { event, .. }) => {
                if self.suppress_activity {
                    return Ok(());
                }
                if self.gap.is_some() || !output.can_acquire() {
                    self.mark_gap(1);
                    return Ok(());
                }
                let mut record = event_record(&event);
                if let Some(turn) = self.turn_reply.as_ref().and_then(|reply| reply.turn) {
                    record["turn"] = json!(turn);
                }
                if !output.try_enqueue(record)? {
                    self.mark_gap(1);
                }
            }
            ControlUpdate::QuestionChanged => self.question_changed = true,
            ControlUpdate::Idle => {
                self.turn_reply = None;
            }
        }
        Ok(())
    }

    fn returned<W: AsyncWrite + Unpin>(
        &mut self,
        returned: ReturnedInput<RpcMetadata>,
        output: &mut Output<W>,
    ) -> Result<()> {
        let correlation = self
            .correlations
            .remove(&returned.id)
            .context("returned Host input has protocol correlation")?;
        let metadata = returned.metadata;
        ensure!(
            metadata.command != "submit_request",
            "durably admitted request was returned; inspect committed acceptance"
        );
        let (reason, error) = match returned.reason {
            ReturnReason::Stopped => ("stopped", None),
            ReturnReason::Cleared => ("cleared", None),
            ReturnReason::TurnEnded => ("turn_ended", None),
            ReturnReason::NotAccepted => ("not_accepted", None),
            ReturnReason::Preparation(error) => ("preparation_failed", Some(format!("{error:#}"))),
        };
        if !output.is_healthy() {
            return Ok(());
        }
        if !correlation.acknowledged {
            output.enqueue_reliable(
                failure(
                    metadata.id.clone(),
                    metadata.command,
                    error.as_deref().unwrap_or(reason),
                ),
                metadata.credit.clone(),
            )?;
        }
        let kind = match metadata.command {
            "prompt" => "uncommitted_prompt",
            "steer" => "uncommitted_steering",
            _ => "uncommitted_follow_up",
        };
        let mut record = json!({"type":kind,"kind":metadata.command,"id":metadata.id,"input":returned.input,"reason":reason});
        if let Some(error) = error {
            record["error"] = json!(error);
        }
        if let Some(error) = returned.preparation_error {
            record["preparation_error"] = json!(format!("{error:#}"));
        }
        output.enqueue_reliable(record, metadata.credit)
    }
}

fn unknown_agent_task(error: &ion_core::AgentError) -> Option<anyhow::Error> {
    match error {
        ion_core::AgentError::Session(error) => unknown_session_task(error),
        _ => None,
    }
}
fn unknown_session_task(error: &ion_core::SessionError) -> Option<anyhow::Error> {
    matches!(
        error,
        ion_core::SessionError::Operation(ion_core::OperationError::TaskFailed(_))
    )
    .then(|| anyhow!("Core operation task failed; unfinished effects remain unknown: {error:#}"))
}
