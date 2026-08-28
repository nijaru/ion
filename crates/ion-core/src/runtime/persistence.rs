use crate::effect::{CompactionInvocation, ModelStepPlan, ToolInvocation};
use crate::ids::{EffectId, InboxId, OperationId, SessionId};
use crate::provider::ModelConfig;
use crate::session::SessionEntry;
use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,
    SettledEffect, UsageRecord,
};
use crate::tool::ToolCall;

use super::ActiveOperation;

/// Decode one persisted model-step plan through the typed durable boundary.
/// The owner module still consumes the historical tuple shape; JSON field
/// knowledge and harness-profile validation stay here.
pub(super) fn model_step_from_input(
    input: &serde_json::Value,
) -> Option<(
    u64,
    ModelConfig,
    crate::context::ContextPlan,
    String,
    String,
    String,
    String,
)> {
    let model_step: ModelStepPlan = serde_json::from_value(input.clone()).ok()?;
    if !model_step.harness_profile.is_supported() {
        return None;
    }
    Some((
        model_step.step,
        model_step.model,
        model_step.plan,
        model_step.capability_snapshot_id,
        model_step.context_manifest_id,
        model_step.prefix_fingerprint,
        model_step.cache_expectation.as_str().to_owned(),
    ))
}

/// Decode one persisted harness-owned compaction invocation.
pub(super) fn compaction_from_input(
    input: &serde_json::Value,
) -> Option<(u64, ModelConfig, crate::context::ContextPlan)> {
    let invocation: CompactionInvocation = serde_json::from_value(input.clone()).ok()?;
    if !invocation.harness_profile.is_supported() {
        return None;
    }
    Some((invocation.step, invocation.model, invocation.plan))
}

/// Decode one persisted tool invocation. The operation id is authoritative on
/// the owning durable operation and is intentionally not reconstructed from
/// effect payload bytes. Return the typed invocation too so recovery never
/// reaches back into raw effect JSON for reconciliation or call identity.
pub(super) fn tool_call_from_input(
    operation_id: OperationId,
    input: &serde_json::Value,
) -> Option<(ToolCall, ToolInvocation)> {
    let invocation: ToolInvocation = serde_json::from_value(input.clone()).ok()?;
    let call = invocation.clone().into_call(operation_id);
    Some((call, invocation))
}

/// Build the durable record of one staged transition. Entry sequences are
/// computed from the caller's next value and returned so the allocator only
/// advances after the commit succeeds (DESIGN.md §26.2).
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
