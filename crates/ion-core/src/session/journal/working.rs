//! The live projection of an authoritative journal. Native SQLite and the
//! Worker writer prepare here and install only the same acknowledged batch.
use super::{AppendProjection, CommittedAppend, JournalIdentity};
use crate::session::*;
use context_index::WorkingContext;

pub(in crate::session) struct WorkingJournal {
    pub(super) identity: JournalIdentity,
    pub(in crate::session) state: State,
    pub(in crate::session) settled: BTreeSet<u64>,
    pub(in crate::session) messages: Vec<ProjectedMessage>,
    // Sparse, rebuildable projection of committed context boundaries.
    pub(in crate::session) context_changes: Vec<ModelContextChange>,
    pub(in crate::session) child_records: ChildRecordIndex,
    pub(in crate::session) context_index: ContextIndex,
    // Process-local capacity evidence; never serialized.
    pub(in crate::session) calibration: Option<RequestCalibration>,
    // Process-local task custody only; never a replay grant.
    pub(in crate::session) live_backgrounds: BTreeMap<uuid::Uuid, std::sync::Weak<()>>,
    #[cfg(all(
        target_arch = "wasm32",
        target_os = "unknown",
        feature = "cloudflare-journal"
    ))]
    pub(super) keys: projection::JournalKeys,
}

impl WorkingJournal {
    #[cfg(any(
        unix,
        test,
        all(
            target_arch = "wasm32",
            target_os = "unknown",
            feature = "cloudflare-journal"
        )
    ))]
    pub(in crate::session) fn open(
        identity: JournalIdentity,
        projection: HistoryProjection,
    ) -> Self {
        let mut journal = Self {
            identity,
            state: projection.state,
            settled: projection.settled,
            messages: projection.messages,
            context_changes: projection.context_changes,
            child_records: projection.child_records,
            context_index: projection.context_index,
            calibration: None,
            live_backgrounds: BTreeMap::new(),
            #[cfg(all(
                target_arch = "wasm32",
                target_os = "unknown",
                feature = "cloudflare-journal"
            ))]
            keys: projection.keys,
        };
        journal.prune_context();
        journal
    }

    #[cfg(all(
        target_arch = "wasm32",
        target_os = "unknown",
        feature = "cloudflare-journal"
    ))]
    pub(in crate::session) fn projected_request(
        &self,
        key: &RequestKey,
    ) -> Option<(u64, RequestReceipt)> {
        let (entry, turn, end) = self.keys.request(key)?;
        Some((
            entry,
            RequestReceipt {
                session: self.identity.session,
                key: key.clone(),
                turn,
                end,
            },
        ))
    }

    pub(in crate::session) fn identity(&self) -> JournalIdentity {
        self.identity
    }

    pub(in crate::session) fn publish(&mut self, committed: CommittedAppend<'_>) {
        let AppendProjection {
            entries,
            candidate,
            resolved_tools,
            replacement,
            replace_pending_message,
            new_messages,
            new_settled,
            new_context_changes,
            #[cfg(all(
                target_arch = "wasm32",
                target_os = "unknown",
                feature = "cloudflare-journal"
            ))]
            new_keys,
        } = committed.0;
        let mut sequence = self.state.sequence;
        for (entry, resolved) in entries.iter().zip(resolved_tools) {
            sequence += 1;
            self.child_records.observe(sequence, entry);
            self.context_index.observe(entry);
            self.context_index.link_display(entry, resolved);
            if let SessionEntry::DelegationSettled { run, .. } = entry {
                self.live_backgrounds.remove(run);
            }
        }
        if entries.iter().any(|entry| {
            matches!(
                entry,
                SessionEntry::ContextSelected { .. }
                    | SessionEntry::Compacted { .. }
                    | SessionEntry::ProviderReplayRebased { .. }
                    | SessionEntry::EffectiveModelChanged { .. }
                    | SessionEntry::ModelSelected { .. }
                    | SessionEntry::ReasoningSelected { .. }
                    | SessionEntry::ModelContextChanged { .. }
            )
        }) {
            self.calibration = None;
        }
        self.state = candidate;
        if let Some(WorkingContext {
            messages,
            settled,
            changes,
        }) = replacement
        {
            self.messages = messages;
            self.settled = settled;
            self.context_changes = changes;
        } else {
            self.settled.extend(new_settled);
            if replace_pending_message {
                self.messages.pop();
            }
            self.messages.extend(new_messages);
            self.context_changes.extend(new_context_changes);
        }
        #[cfg(all(
            target_arch = "wasm32",
            target_os = "unknown",
            feature = "cloudflare-journal"
        ))]
        self.keys.extend(new_keys);
        self.prune_context();
    }

    fn prune_context(&mut self) {
        if let Some((through, _)) = &self.state.compaction {
            self.messages.retain(|record| {
                record.entry > *through
                    || self
                        .state
                        .compaction_inputs
                        .binary_search(&record.entry)
                        .is_ok()
                    || self
                        .state
                        .active_inputs
                        .binary_search(&record.entry)
                        .is_ok()
            });
            self.settled.retain(|entry| entry > through);
        }
        if self.state.compaction.is_some() || self.state.replay_epoch_start != 0 {
            self.context_changes.clear();
        }
    }

    pub(in crate::session) fn orphaned_backgrounds(
        &self,
        session: uuid::Uuid,
    ) -> Vec<SessionEntry> {
        self.state
            .backgrounds
            .iter()
            .filter(|(run, link)| {
                link.site.session == session
                    && self
                        .live_backgrounds
                        .get(run)
                        .is_none_or(|lease| lease.strong_count() == 0)
            })
            .map(|(run, _)| SessionEntry::DelegationSettled {
                run: *run,
                outcome: crate::DelegationOutcome::Unknown,
            })
            .collect()
    }
}
