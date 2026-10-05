//! Shared JSONL projection of committed coding events and provisional progress.
use ion_core::CodingAgentEvent;
use serde_json::{Value, json};

pub(super) fn event_record(event: CodingAgentEvent) -> Value {
    match event {
        CodingAgentEvent::TurnAccepted { turn } => json!({"type":"turn_accepted","turn":turn}),
        CodingAgentEvent::TextDelta(text) => json!({"type":"text_delta","text":text}),
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
        } => {
            json!({"type":"tool_finished","call_id":call_id,"name":name,"activity":activity,"output":output.value,"image_mime_types":output.images.iter().map(|image| image.mime_type().as_str()).collect::<Vec<_>>(),"is_error":output.is_error})
        }
        CodingAgentEvent::ToolRejected {
            call_id,
            name,
            activity,
            output,
        } => {
            json!({"type":"tool_rejected","call_id":call_id,"name":name,"activity":activity,"output":output.value,"is_error":output.is_error})
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
