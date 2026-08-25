use crate::context::ContextPlan;
use crate::ids::{EffectId, InboxId, OperationId, SessionId};
use crate::provider::ModelConfig;
use crate::session::SessionEntry;
use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,
    SettledEffect, UsageRecord,
};
use crate::tool::ToolCall;

use super::ActiveOperation;

/// Reconstruct a tool call from a persisted effect's exact effective
/// input (DESIGN.md §12.1). Returns None for inputs from older schemas
/// that lack the call identity.
pub(super) fn tool_call_from_input(input: &serde_json::Value) -> Option<ToolCall> {
    let operation_id = OperationId::from_uuid(uuid::Uuid::now_v7());
    Some(ToolCall {
        operation_id,
        call_id: input.get("call_id")?.as_u64()?,
        name: input.get("tool")?.as_str()?.to_owned(),
        arguments: input.get("arguments")?.clone(),
    })
}

/// Reconstruct the frozen model snapshot from a persisted provider
/// effect's exact effective input (DESIGN.md §14.8). Recovery replays
/// this identity or fences; it never substitutes a launch default.
pub(super) fn model_from_input(model: &serde_json::Value) -> Option<ModelConfig> {
    Some(ModelConfig {
        model_ref: model.get("model_ref")?.as_str()?.to_owned(),
        context_window: model
            .get("context_window")
            .and_then(serde_json::Value::as_u64),
        capabilities: serde_json::from_value(model.get("capabilities")?.clone()).ok()?,
    })
}

/// `(step, model, plan)` from a persisted model-step effect input.
pub(super) fn model_step_from_input(
    input: &serde_json::Value,
) -> Option<(
    u64,
    ModelConfig,
    ContextPlan,
    String,
    String,
    String,
    String,
)> {
    let step = input.get("step")?.as_u64()?;
    let model = model_from_input(input.get("model")?)?;
    let plan = serde_json::from_value(input.get("plan")?.clone()).ok()?;
    let capability_snapshot_id = input.get("capability_snapshot_id")?.as_str()?.to_owned();
    let context_manifest_id = input.get("context_manifest_id")?.as_str()?.to_owned();
    let prefix_fingerprint = input.get("prefix_fingerprint")?.as_str()?.to_owned();
    let cache_expectation = input.get("cache_expectation")?.as_str()?.to_owned();
    Some((
        step,
        model,
        plan,
        capability_snapshot_id,
        context_manifest_id,
        prefix_fingerprint,
        cache_expectation,
    ))
}

/// `(step, model, plan)` from a persisted compaction effect input.
pub(super) fn compaction_from_input(
    input: &serde_json::Value,
) -> Option<(u64, ModelConfig, ContextPlan)> {
    let step = input.get("step")?.as_u64()?;
    let model = model_from_input(input.get("model")?)?;
    let plan = serde_json::from_value(input.get("plan")?.clone()).ok()?;
    Some((step, model, plan))
}

/// Build the durable record of one staged transition. Entry sequences are
/// computed from the caller's next value and returned so the allocator
/// only advances after the commit succeeds (DESIGN.md §26.2).
#[allow(clippy::too_many_arguments)]
pub(super) fn build_commit_request(
    session_id: SessionId,
    staged: &ActiveOperation,
    state_seq: u64,
    next_entry_seq: u64,
    entries: Vec<SessionEntry>,
    open_effects: Vec<EffectRecord>,
    settled_effects: Vec<SettledEffect>,
    indeterminate_effects: Vec<EffectId>,
    inbox: Vec<InboxRecord>,
    inbox_applied: Vec<InboxId>,
    usage: Vec<UsageRecord>,
) -> (CommitRequest, u64) {
    let capability_snapshot = staged.capability_snapshot.clone();
    let mut seq = next_entry_seq;
    let entries = entries
        .into_iter()
        .map(|entry| {
            let entry_seq = seq;
            seq += 1;
            EntryRecord {
                seq: entry_seq,
                entry,
            }
        })
        .collect();
    let request = CommitRequest {
        session_id,
        operation_id: staged.machine.operation_id(),
        checkpoint: CheckpointRecord {
            state_seq,
            payload: CheckpointPayload {
                state: staged.machine.state().clone(),
                cancel_requested: staged.machine.cancel_requested(),
                prompt: staged.machine.prompt().to_owned(),
                capability_snapshot_id: capability_snapshot.id.clone(),
                open_effect: staged.open_effect.clone(),
            },
            capability_snapshot,
        },
        entries,
        open_effects,
        settled_effects,
        indeterminate_effects,
        inbox,
        inbox_applied,
        usage,
        context_manifests: Vec::new(),
        assistant_frames_delete: Vec::new(),
        tool_progress_delete: Vec::new(),
    };
    (request, seq)
}
