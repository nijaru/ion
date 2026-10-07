//! Shared JSONL projection of committed coding events and provisional progress.
use ion_core::CodingAgentEvent;
use serde_json::{Value, json};

pub(super) fn event_record(event: CodingAgentEvent) -> Value {
    match event {
        CodingAgentEvent::TurnAccepted { turn } => json!({"type":"turn_accepted","turn":turn}),
        CodingAgentEvent::TextDelta(text) => json!({"type":"text_delta","text":text}),
        CodingAgentEvent::ThinkingDelta { block, text } => {
            json!({"type":"thinking_delta","block":block,"text":text})
        }
        CodingAgentEvent::AssistantCommitted {
            turn,
            content,
            tool_activities,
            termination,
        } => {
            json!({"type":"assistant_committed","turn":turn,"content":content,"tool_activities":tool_activities,"termination":termination})
        }
        CodingAgentEvent::SteeringCommitted { turn, input } => {
            json!({"type":"steering_committed","turn":turn,"input":input})
        }
        CodingAgentEvent::ProviderRetry {
            attempt,
            max_retries,
            delay_ms,
        } => {
            json!({"type":"provider_retry","attempt":attempt,"max_retries":max_retries,"delay_ms":delay_ms})
        }
        CodingAgentEvent::ToolStarted {
            call_id,
            name,
            arguments,
            activity,
        } => {
            json!({"type":"tool_started","call_id":call_id,"name":name,"arguments":arguments,"activity":activity})
        }
        CodingAgentEvent::ToolFinished {
            call_id,
            name,
            activity,
            output,
            projection,
        } => {
            json!({"type":"tool_finished","call_id":call_id,"name":name,"activity":activity,"output":output.value,"image_mime_types":output.images.iter().map(|image| image.mime_type().as_str()).collect::<Vec<_>>(),"is_error":output.is_error,"model_projection":projection})
        }
        CodingAgentEvent::ToolRejected {
            call_id,
            name,
            activity,
            output,
        } => {
            json!({"type":"tool_rejected","call_id":call_id,"name":name,"activity":activity,"output":output.value,"is_error":output.is_error})
        }
        CodingAgentEvent::ChildToolAdmitted {
            parent_call_id,
            intent,
        } => json!({"type":"child_tool_admitted","parent_call_id":parent_call_id,"intent":intent}),
        CodingAgentEvent::ChildToolStarted { parent, child } => {
            json!({"type":"child_tool_started","parent":parent,"child":child})
        }
        CodingAgentEvent::ChildToolFinished {
            parent,
            child,
            outcome,
        } => {
            let state = match &outcome {
                ion_core::ChildOutcome::Observed { .. } => "observed",
                ion_core::ChildOutcome::NotDispatched { .. } => "not_dispatched",
                ion_core::ChildOutcome::Unknown => "unknown",
            };
            let output = outcome.inspection_output();
            json!({"type":"child_tool_finished","parent":parent,"child":child,"state":state,"output":output.value,"is_error":output.is_error,"image_mime_types":output.images.iter().map(|image| image.mime_type().as_str()).collect::<Vec<_>>()})
        }
        CodingAgentEvent::InterruptedCalls(count) => {
            json!({"type":"interrupted_calls","count":count})
        }
        CodingAgentEvent::ContextCompacted { through_entry } => {
            json!({"type":"context_compacted","through_entry":through_entry})
        }
        CodingAgentEvent::ProviderReplayRebased => json!({"type":"provider_replay_rebased"}),
        CodingAgentEvent::ProviderReplayNotice {
            action,
            reason,
            count,
        } => json!({"type":"provider_replay_notice","action":action,"reason":reason,"count":count}),
        CodingAgentEvent::ResponseRestarted => json!({"type":"response_restarted"}),
        CodingAgentEvent::ToolCatalogWarning(message) => {
            json!({"type":"tool_catalog_warning","message":message})
        }
        CodingAgentEvent::Final(text) => json!({"type":"final","text":text}),
    }
}

/// Build a human diagnostic without performing I/O inside the effect callback.
/// The headless owner handles a failed write by stopping admission and waiting.
pub(super) fn diagnostic(event: CodingAgentEvent) -> Option<String> {
    let text = match event {
        CodingAgentEvent::ProviderRetry {
            attempt,
            max_retries,
            delay_ms,
        } => format!("[provider retry {attempt}/{max_retries} in {delay_ms}ms]"),
        CodingAgentEvent::ToolStarted { name, .. } => format!("[tool: {name}]"),
        CodingAgentEvent::ToolFinished {
            name,
            output,
            projection,
            ..
        } => {
            let mut text = output_diagnostic(&format!("tool: {name}"), output);
            if let Some(notice) = projection.notice() {
                text.push_str(&format!("\n[tool: {name}] {notice}"));
            }
            text
        }
        CodingAgentEvent::ToolRejected { name, output, .. } => {
            output_diagnostic(&format!("tool skipped: {name}"), output)
        }
        CodingAgentEvent::ChildToolAdmitted { intent, .. } => {
            format!("[child tool: {}]", intent.call.name)
        }
        CodingAgentEvent::ChildToolFinished { outcome, .. } => {
            output_diagnostic("child result", outcome.inspection_output())
        }
        CodingAgentEvent::InterruptedCalls(count) => {
            format!("[recovered {count} incomplete tool call(s); effects unknown]")
        }
        CodingAgentEvent::ContextCompacted { through_entry } => {
            format!("[context summarized through entry {through_entry}]")
        }
        CodingAgentEvent::ProviderReplayRebased => "[provider reasoning context reset]".into(),
        CodingAgentEvent::ProviderReplayNotice {
            action,
            reason,
            count,
        } => format!("[provider reasoning {action}: {count} block(s), {reason}]"),
        CodingAgentEvent::ResponseRestarted => "[incomplete response discarded; retrying]".into(),
        CodingAgentEvent::ToolCatalogWarning(message) => format!("[tool catalog: {message}]"),
        CodingAgentEvent::TurnAccepted { .. }
        | CodingAgentEvent::TextDelta(_)
        | CodingAgentEvent::ThinkingDelta { .. }
        | CodingAgentEvent::AssistantCommitted { .. }
        | CodingAgentEvent::SteeringCommitted { .. }
        | CodingAgentEvent::ChildToolStarted { .. }
        | CodingAgentEvent::Final(_) => return None,
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

fn output_diagnostic(label: &str, output: ion_core::CodingToolOutput) -> String {
    let mut text = format!("[{label}] {}", output.value);
    for image in &output.images {
        text.push_str(&format!(
            "\n[{label} image: {}]",
            image.mime_type().as_str()
        ));
    }
    text
}
