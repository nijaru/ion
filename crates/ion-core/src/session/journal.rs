//! Prepare a validated append from committed facts, without owning storage.
//! A writer commits the encoded batch at `expected_tail` before publishing projections.
use super::*;
use crate::ToolOccurrence;

// An 8 MiB raw prompt or streamed response can grow up to sixfold when JSON
// escapes control characters. Keep the storage bound above that encoded size.
pub(crate) const MAX_ENTRY_BYTES: usize = 64 * 1024 * 1024;

pub(super) struct AppendBasis<'a> {
    pub state: &'a State,
    pub context_index: &'a ContextIndex,
    pub settled: &'a BTreeSet<u64>,
    pub message_count: usize,
}

pub(super) struct PreparedAppend {
    pub expected_tail: u64,
    pub encoded: Vec<Vec<u8>>,
    pub candidate: State,
    pub resolved_tools: Vec<Option<ToolOccurrence>>,
    pub replacement: Option<context_index::WorkingContext>,
    pub replace_pending_message: bool,
    pub new_messages: Vec<ProjectedMessage>,
    pub new_settled: Vec<u64>,
    pub new_context_changes: Vec<ModelContextChange>,
}

impl AppendBasis<'_> {
    pub fn prepare(
        &self,
        entries: &[SessionEntry],
        read: impl Fn(u64) -> Result<SessionEntry, SessionError>,
        validate_artifact: impl Fn(&crate::ArtifactRef) -> Result<(), SessionError>,
    ) -> Result<PreparedAppend, SessionError> {
        let mut replacement = None;
        let mut candidate = self.state.clone();
        // A shared pending shell owns the final context message. Seed only that
        // slot so candidate settlement can replace it without rebuilding history.
        let pending_message = self
            .state
            .pending_shell
            .as_ref()
            .filter(|shell| !shell.exclude_from_context)
            .map(|shell| {
                ProjectedMessage::new(
                    shell.admission_entry,
                    shell_message(
                        &shell.command,
                        &UserShellOutcome::Unknown,
                        false,
                        shell.admission_entry,
                    ),
                )
            });
        let replace_pending_message = pending_message.is_some();
        let mut new_messages: Vec<_> = pending_message.into_iter().collect();
        let message_start = self.message_count - usize::from(replace_pending_message);
        let mut new_context_changes = Vec::new();
        let mut new_settled = Vec::new();
        let mut resolved_tools = Vec::with_capacity(entries.len());
        let encoded = entries
            .iter()
            .enumerate()
            .map(|(offset, entry)| {
                for reference in artifacts::references(entry) {
                    validate_artifact(reference)?;
                }
                let transition = candidate.apply(entry, &|coordinate| {
                    self.settled.contains(&coordinate) || new_settled.contains(&coordinate)
                })?;
                resolved_tools.push(transition.resolved_tool);
                if transition.settled {
                    new_settled.push(candidate.sequence);
                }
                projection::append_message(
                    &mut new_messages,
                    candidate.sequence,
                    entry,
                    transition.closed_shell.as_ref(),
                );
                if let SessionEntry::ContextSelected { head_entry } = entry {
                    if replacement.is_some() || offset + 1 != entries.len() {
                        return Err(SessionError::InvalidHistory);
                    }
                    if !self.context_index.can_select(*head_entry) {
                        return Err(SessionError::InvalidContextPoint);
                    }
                    candidate.restore_context(self.context_index.read_context(*head_entry, &read)?);
                    replacement = Some(working_context(
                        self.context_index,
                        *head_entry,
                        &candidate,
                        &read,
                    )?);
                }
                if let SessionEntry::ModelContextChanged { context, .. } = entry
                    && candidate.compaction.is_none()
                    && candidate.replay_epoch_start == 0
                {
                    new_context_changes.push(ModelContextChange {
                        after_message: message_start + new_messages.len(),
                        context: model_context_state(context),
                    });
                }
                encode_entry(entry)
            })
            .collect::<Result<Vec<_>, SessionError>>()?;
        Ok(PreparedAppend {
            expected_tail: self.state.sequence,
            encoded,
            candidate,
            resolved_tools,
            replacement,
            replace_pending_message,
            new_messages,
            new_settled,
            new_context_changes,
        })
    }
}

pub(super) fn encode_entry(entry: &SessionEntry) -> Result<Vec<u8>, SessionError> {
    let bytes = serde_json::to_vec(entry)?;
    if bytes.len() > MAX_ENTRY_BYTES {
        return Err(SessionError::EntryTooLarge);
    }
    Ok(bytes)
}
