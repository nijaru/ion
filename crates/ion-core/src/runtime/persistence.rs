use crate::ids::{EffectId, InboxId, SessionId};
use crate::operation::SessionEntry;
use crate::store::{
    CheckpointPayload, CheckpointRecord, CommitRequest, EffectRecord, EntryRecord, InboxRecord,
    SettledEffect, UsageRecord,
};

use super::ActiveOperation;

/// Build the durable record of one staged transition. Semantic entry identity
/// is provisioned here, before the request crosses into the store; sequences
/// advance only after the commit succeeds (DESIGN.md §26.2).
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
            EntryRecord::provision(entry_seq, entry)
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
