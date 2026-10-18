//! Model context uses the resident, committed message projection, not raw audits.
use super::{CompactionPlan, Session, SessionError, Store, summary_message};
use ion_ai::{Content, Message, ModelRef, Role};

/// A model-visible message and its committed coordinate. Shell settlement
/// replaces the message at its admission coordinate; it never adds a new slot.
pub(super) struct ProjectedMessage {
    pub(super) entry: u64,
    pub(super) message: Message,
}

impl ProjectedMessage {
    pub(super) fn new(entry: u64, message: Message) -> Self {
        Self { entry, message }
    }
}

pub(super) fn context_projection(store: &Store, model: Option<&ModelRef>) -> Vec<Message> {
    let through = store
        .state
        .compaction
        .as_ref()
        .map_or(0, |(through, _)| *through);
    let mut messages = store
        .state
        .compaction
        .as_ref()
        .map(|(_, summary)| vec![summary_message(summary)])
        .unwrap_or_default();
    let clear_all = model != store.state.last_effective_model.as_ref();
    let first = store
        .messages
        .partition_point(|record| record.entry <= through);
    messages.extend(store.messages[first..].iter().map(|record| Message {
        role: record.message.role,
        content: record.message.content.clone(),
        provider_replay: if clear_all || record.entry < store.state.replay_epoch_start {
            None
        } else {
            record.message.provider_replay.clone()
        },
    }));
    messages
}

impl Session {
    /// Find the earliest settled cut that fits a useful recent suffix.
    /// The returned prefix includes the previous summary, if any.
    pub(crate) fn compaction_plan(
        &self,
        keep_bytes: usize,
        max_summary_bytes: usize,
        summary_images: bool,
        max_through_entry: u64,
    ) -> Result<Option<CompactionPlan>, SessionError> {
        let store = self.store.lock().map_err(|_| SessionError::Poisoned)?;
        let previous = store
            .state
            .compaction
            .as_ref()
            .map_or(0, |(through, _)| *through);
        let first = store
            .messages
            .partition_point(|record| record.entry <= previous);
        let projected = &store.messages[first..];
        let summary_bytes = store
            .state
            .compaction
            .as_ref()
            .map(|(_, summary)| crate::json_size::encoded_len(&summary_message(summary)))
            .transpose()?
            .map_or(0, |bytes| bytes + 1);
        let suffix_budget = keep_bytes.saturating_sub(summary_bytes);
        let mut prefix_bytes = match &store.state.compaction {
            Some((_, summary)) => {
                crate::summary::content_bytes(&summary_message(summary), summary_images)?
                    .saturating_add(1)
            }
            None => 1,
        };
        let mut prefix_fits = Vec::new();
        let mut prefix = projected.iter().peekable();
        for &boundary in store.settled.range((previous + 1)..) {
            while let Some(record) = prefix.next_if(|record| record.entry <= boundary) {
                prefix_bytes = prefix_bytes.saturating_add(crate::summary::content_bytes(
                    &record.message,
                    summary_images,
                )?);
            }
            if boundary <= max_through_entry && prefix_bytes <= max_summary_bytes {
                prefix_fits.push(boundary);
            }
        }
        let mut suffix_bytes = 2usize;
        let mut through = None;
        let mut suffix = projected.iter().rev().peekable();
        for &boundary in store.settled.range((previous + 1)..).rev() {
            while let Some(record) = suffix.next_if(|record| record.entry > boundary) {
                suffix_bytes = suffix_bytes
                    .saturating_add(crate::json_size::encoded_len(
                        &crate::prompt_size::NeutralMessage::from(&record.message),
                    )?)
                    .saturating_add(1);
            }
            if suffix_bytes <= suffix_budget {
                through = Some(boundary);
            }
        }
        let through =
            through.or_else(|| store.settled.range((previous + 1)..).next_back().copied());
        // Preserve the latest call/result batch when an older settled cut exists,
        // even if its large result makes only an empty recent suffix fit.
        let through = if keep_bytes > 0 {
            projected
                .last()
                .filter(|record| record.message.role == Role::Tool)
                .and_then(|recent| {
                    projected.iter().rev().find(|record| {
                        record.entry < recent.entry
                            && record.message.role == Role::Assistant
                            && record
                                .message
                                .content
                                .iter()
                                .any(|part| matches!(part, Content::ToolCall(_)))
                    })
                })
                .and_then(|call| {
                    store
                        .settled
                        .range((previous + 1)..call.entry)
                        .next_back()
                        .copied()
                })
                .filter(|&cut| through.is_some_and(|current| current > cut))
                .or(through)
        } else {
            through
        };
        let Some((suffix_target, prefix_fit)) = through.zip(prefix_fits.last().copied()) else {
            return Ok(None);
        };
        let through_entry = suffix_target.min(prefix_fit);
        let smaller_count = prefix_fits.partition_point(|&cut| cut < through_entry);
        let smaller_through_entry = smaller_count
            .checked_sub(1)
            .map(|last| prefix_fits[last / 2]);
        let mut messages = Vec::new();
        if let Some((_, summary)) = &store.state.compaction {
            messages.push(summary_message(summary));
        }
        messages.extend(
            projected
                .iter()
                .take_while(|record| record.entry <= through_entry)
                .map(|record| record.message.clone()),
        );
        Ok(Some(CompactionPlan {
            through_entry,
            messages,
            chunked: through_entry < suffix_target,
            smaller_through_entry,
        }))
    }
}
