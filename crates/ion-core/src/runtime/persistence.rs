use crate::effect::{CompactionInvocation, DurableEffect, ModelInvocation, ToolInvocation};
use crate::ids::{EffectId, InboxId, OperationId, SessionId};
use crate::session::SessionEntry;
use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,
    SettledEffect, UsageRecord,
};
use crate::tool::ToolCall;

use super::ActiveOperation;

/// Decode one persisted model invocation through the single typed durable
/// effect boundary. Recovery never knows the storage JSON field names.
pub(super) fn model_invocation(effect: &EffectRecord) -> Option<ModelInvocation> {
    match effect.decode().ok()? {
        DurableEffect::Model(invocation) => Some(invocation),
        _ => None,
    }
}

/// Decode one persisted compaction invocation through the durable effect
/// boundary.
pub(super) fn compaction_invocation(effect: &EffectRecord) -> Option<CompactionInvocation> {
    match effect.decode().ok()? {
        DurableEffect::Compaction(invocation) => Some(invocation),
        _ => None,
    }
}

/// Decode one persisted tool invocation, restoring the durable operation id
/// from the owning operation record rather than inventing identity.
pub(super) fn tool_invocation(
    operation_id: OperationId,
    effect: &EffectRecord,
) -> Option<(ToolCall, ToolInvocation)> {
    match effect.decode().ok()? {
        DurableEffect::Tool(invocation) => {
            let call = invocation.clone().into_call(operation_id);
            Some((call, invocation))
        }
        _ => None,
    }
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
