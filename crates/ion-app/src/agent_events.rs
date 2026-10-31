//! Shared JSONL projection of committed coding events and provisional progress.
use ion_core::AgentEvent;
use serde_json::{Value, json};

pub(super) fn event_record(event: &AgentEvent) -> Value {
    match event {
        AgentEvent::CompletionCommitted {
            turn,
            notices,
            new_turn,
        } => {
            json!({"type":"completion_committed","turn":turn,"notices":notices,"new_turn":new_turn,"automated":true})
        }
        AgentEvent::TurnResumed { point } => json!({"type":"turn_resumed","point":point}),
        // This receipt stream does not echo normalized file contents or image data.
        AgentEvent::TurnAccepted { turn, .. } => json!({"type":"turn_accepted","turn":turn}),
        AgentEvent::TurnEnded { turn, reason } => {
            json!({"type":"turn_ended","turn":turn,"reason":reason})
        }
        AgentEvent::ModelOutputObserved => json!({"type":"model_output_observed"}),
        AgentEvent::TextDelta(text) => json!({"type":"text_delta","text":text}),
        AgentEvent::ThinkingDelta { block, text } => {
            json!({"type":"thinking_delta","block":block,"text":text})
        }
        AgentEvent::AssistantCommitted {
            turn,
            content,
            tool_activities,
            termination,
        } => {
            json!({"type":"assistant_committed","turn":turn,"content":content,"tool_activities":tool_activities,"termination":termination})
        }
        AgentEvent::SteeringCommitted { turn, input } => {
            json!({"type":"steering_committed","turn":turn,"input":input})
        }
        AgentEvent::ProviderRetry {
            attempt,
            max_retries,
            delay_ms,
        } => {
            json!({"type":"provider_retry","attempt":attempt,"max_retries":max_retries,"delay_ms":delay_ms})
        }
        AgentEvent::ToolStarted {
            call_id,
            name,
            arguments,
            activity,
        } => {
            json!({"type":"tool_started","call_id":call_id,"name":name,"arguments":arguments,"activity":activity})
        }
        AgentEvent::ToolFinished {
            call_id,
            name,
            activity,
            outcome,
        } => {
            let state = match outcome {
                ion_core::ToolOutcome::Observed { .. } => "observed",
                ion_core::ToolOutcome::NotDispatched { .. } => "not_dispatched",
                ion_core::ToolOutcome::Unknown => "unknown",
            };
            let projection = outcome.projection();
            let output = outcome.inspection_output();
            json!({"type":"tool_finished","call_id":call_id,"name":name,"activity":activity,"state":state,"output":output.value,"image_mime_types":output.images.iter().map(|image| image.mime_type().as_str()).collect::<Vec<_>>(),"is_error":output.is_error,"model_projection":projection})
        }
        AgentEvent::ChildToolAdmitted {
            parent_call_id,
            intent,
        } => json!({"type":"child_tool_admitted","parent_call_id":parent_call_id,"intent":intent}),
        AgentEvent::ChildToolStarted { parent, child } => {
            json!({"type":"child_tool_started","parent":parent,"child":child})
        }
        AgentEvent::ChildToolFinished {
            parent,
            child,
            outcome,
        } => {
            let state = match outcome {
                ion_core::ChildOutcome::Observed { .. } => "observed",
                ion_core::ChildOutcome::NotDispatched { .. } => "not_dispatched",
                ion_core::ChildOutcome::Unknown => "unknown",
            };
            let output = child_output(outcome);
            json!({"type":"child_tool_finished","parent":parent,"child":child,"state":state,"output":output.value,"is_error":output.is_error,"image_mime_types":output.images.iter().map(|image| image.mime_type().as_str()).collect::<Vec<_>>()})
        }
        AgentEvent::InterruptedCalls(count) => {
            json!({"type":"interrupted_calls","count":count})
        }
        AgentEvent::ContextCompactionRetry {
            through_entry,
            next_through_entry,
        } => {
            json!({"type":"context_compaction_retry","through_entry":through_entry,"next_through_entry":next_through_entry})
        }
        AgentEvent::ContextCompacted { through_entry } => {
            json!({"type":"context_compacted","through_entry":through_entry})
        }
        AgentEvent::ProviderReplayRebased => json!({"type":"provider_replay_rebased"}),
        AgentEvent::ProviderReplayNotice {
            action,
            reason,
            count,
        } => json!({"type":"provider_replay_notice","action":action,"reason":reason,"count":count}),
        AgentEvent::ResponseRestarted => json!({"type":"response_restarted"}),
        AgentEvent::ToolCatalogWarning(message) => {
            json!({"type":"tool_catalog_warning","message":message})
        }
        AgentEvent::Final(text) => json!({"type":"final","text":text}),
    }
}

/// Build a human diagnostic without performing I/O inside the effect callback.
/// The headless owner handles a failed write by stopping admission and waiting.
pub(super) fn diagnostic(event: &AgentEvent) -> Option<String> {
    let text = match event {
        AgentEvent::ProviderRetry {
            attempt,
            max_retries,
            delay_ms,
        } => format!("[provider retry {attempt}/{max_retries} in {delay_ms}ms]"),
        AgentEvent::ToolStarted { name, .. } => format!("[tool: {name}]"),
        AgentEvent::ToolFinished { name, outcome, .. } => {
            let label = match outcome {
                ion_core::ToolOutcome::Observed { .. } => "tool",
                ion_core::ToolOutcome::NotDispatched { .. } => "tool skipped",
                ion_core::ToolOutcome::Unknown => "tool effect unknown",
            };
            let projection = outcome.projection();
            let mut text =
                output_diagnostic(&format!("{label}: {name}"), &outcome.inspection_output());
            if let Some(notice) = projection.and_then(ion_core::ToolResultProjection::notice) {
                text.push_str(&format!("\n[tool: {name}] {notice}"));
            }
            text
        }
        AgentEvent::ChildToolAdmitted { intent, .. } => {
            format!("[child tool: {}]", intent.call.name)
        }
        AgentEvent::ChildToolFinished { outcome, .. } => {
            output_diagnostic("child result", &child_output(outcome))
        }
        AgentEvent::CompletionCommitted {
            notices, new_turn, ..
        } => format!(
            "[{}managed-work readiness: {} report(s) ready; not acknowledged]",
            if *new_turn { "automated Turn · " } else { "" },
            notices.len()
        ),
        AgentEvent::TurnResumed { point } => format!(
            "[continued interrupted Turn {} at checkpoint {}]",
            point.turn, point.checkpoint
        ),
        AgentEvent::InterruptedCalls(count) => {
            format!("[recovered {count} incomplete tool call(s); effects unknown]")
        }
        AgentEvent::ContextCompactionRetry {
            next_through_entry, ..
        } => format!("[summary request too large; retrying through entry {next_through_entry}]"),
        AgentEvent::ContextCompacted { through_entry } => {
            format!("[context summarized through entry {through_entry}]")
        }
        AgentEvent::ProviderReplayRebased => "[provider reasoning context reset]".into(),
        AgentEvent::ProviderReplayNotice {
            action,
            reason,
            count,
        } => format!("[provider reasoning {action}: {count} block(s), {reason}]"),
        AgentEvent::ResponseRestarted => "[incomplete response discarded; retrying]".into(),
        AgentEvent::ToolCatalogWarning(message) => format!("[tool catalog: {message}]"),
        AgentEvent::TurnAccepted { .. }
        | AgentEvent::TurnEnded { .. }
        | AgentEvent::ModelOutputObserved
        | AgentEvent::TextDelta(_)
        | AgentEvent::ThinkingDelta { .. }
        | AgentEvent::AssistantCommitted { .. }
        | AgentEvent::SteeringCommitted { .. }
        | AgentEvent::ChildToolStarted { .. }
        | AgentEvent::Final(_) => return None,
    };
    Some(
        text.chars()
            .map(|ch| {
                if ch == '\n' || ch == '\t' || !ch.is_control() {
                    ch
                } else {
                    '�'
                }
            })
            .collect(),
    )
}

fn child_output(outcome: &ion_core::ChildOutcome) -> std::borrow::Cow<'_, ion_core::ToolOutput> {
    match outcome {
        ion_core::ChildOutcome::Observed { output } => std::borrow::Cow::Borrowed(output),
        _ => std::borrow::Cow::Owned(outcome.clone().inspection_output()),
    }
}

fn output_diagnostic(label: &str, output: &ion_core::ToolOutput) -> String {
    let mut text = format!("[{label}] {}", output.value);
    for image in &output.images {
        text.push_str(&format!(
            "\n[{label} image: {}]",
            image.mime_type().as_str()
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceptance_receipt_does_not_disclose_normalized_input() {
        let event = AgentEvent::TurnAccepted {
            turn: 7,
            input: ion_ai::Message::user_input("PRIVATE_NORMALIZED_FILE_CONTENT".into(), []),
        };
        assert_eq!(
            event_record(&event),
            json!({"type":"turn_accepted","turn":7})
        );
    }
}
