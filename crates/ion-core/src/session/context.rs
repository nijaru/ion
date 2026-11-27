//! Model context uses the resident, committed message projection, not raw audits.
use super::{
    CompactionPlan, CompactionPlanning, Session, SessionError, journal::WorkingJournal,
    summary_message,
};
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

fn inputs_at<'a>(
    working: &'a WorkingJournal,
    entries: &'a [u64],
) -> impl Iterator<Item = &'a ProjectedMessage> {
    entries.iter().map(|entry| {
        let index = working
            .messages
            .binary_search_by_key(entry, |record| record.entry)
            .expect("input coordinates derive from messages on the selected path");
        &working.messages[index]
    })
}

// The summary is generated text, projected separately. Only these committed
// records contribute original content: pinned inputs followed by the cut's suffix.
fn selected_context_records(working: &WorkingJournal) -> impl Iterator<Item = &ProjectedMessage> {
    let through = working
        .state
        .compaction
        .as_ref()
        .map_or(0, |(through, _)| *through);
    let first = working
        .messages
        .partition_point(|record| record.entry <= through);
    inputs_at(working, &working.state.compaction_inputs).chain(working.messages[first..].iter())
}

pub(super) fn context_projection(
    working: &WorkingJournal,
    model: Option<&ModelRef>,
) -> Vec<Message> {
    let mut messages = working
        .state
        .compaction
        .as_ref()
        .map(|(through, summary)| vec![summary_message(*through, summary)])
        .unwrap_or_default();
    let clear_all = model != working.state.last_effective_model.as_ref();
    messages.extend(selected_context_records(working).map(|record| Message {
        role: record.message.role,
        content: record.message.content.clone(),
        provider_replay: if clear_all || record.entry < working.state.replay_epoch_start {
            None
        } else {
            record.message.provider_replay.clone()
        },
    }));
    messages
}

impl Session {
    /// Match the request projection's top-level image semantics without cloning
    /// message content or opaque provider replay. Tool-result images are not
    /// top-level content and remain subject to request preparation's validation.
    pub(crate) fn context_has_images(&self) -> Result<bool, SessionError> {
        let store = self.lock_store()?;
        Ok(selected_context_records(&store.working).any(|record| {
            record
                .message
                .content
                .iter()
                .any(|part| matches!(part, Content::Image(_)))
        }))
    }

    /// Find the earliest settled cut that fits a useful recent suffix, or
    /// distinguish absent settled history from history that cannot fit the bounds.
    /// The planned prefix includes the previous summary, if any.
    pub(crate) fn compaction_plan(
        &self,
        instructions: Option<&str>,
        keep_bytes: usize,
        max_summary_bytes: usize,
        summary_images: bool,
        max_through_entry: u64,
    ) -> Result<CompactionPlanning, SessionError> {
        let store = self.lock_store()?;
        let previous = store
            .working
            .state
            .compaction
            .as_ref()
            .map_or(0, |(through, _)| *through);
        let boundaries = store.working.settled.range((previous + 1)..);
        let Some(latest) = boundaries.clone().next_back().copied() else {
            return Ok(CompactionPlanning::NothingToCompact);
        };
        let first = store
            .working
            .messages
            .partition_point(|record| record.entry <= previous);
        let projected = &store.working.messages[first..];
        let summary_bytes = store
            .working
            .state
            .compaction
            .as_ref()
            .map(|(through, summary)| {
                crate::json_size::encoded_len(&summary_message(*through, summary))
            })
            .transpose()?
            .map_or(0, |bytes| bytes + 1);
        let suffix_budget = keep_bytes.saturating_sub(summary_bytes);
        let mut prefix_bytes = crate::summary::framing_bytes(instructions)?;
        if let Some((through, summary)) = &store.working.state.compaction {
            prefix_bytes = prefix_bytes.saturating_add(crate::summary::content_bytes(
                &summary_message(*through, summary),
                summary_images,
            )?);
        }
        for record in inputs_at(&store.working, &store.working.state.compaction_inputs) {
            prefix_bytes = prefix_bytes.saturating_add(crate::summary::content_bytes(
                &record.message,
                summary_images,
            )?);
        }
        let mut prefix_fits = Vec::new();
        let mut prefix = projected.iter().peekable();
        for &boundary in boundaries {
            if boundary > max_through_entry || prefix_bytes > max_summary_bytes {
                break;
            }
            while let Some(record) = prefix.next_if(|record| record.entry <= boundary) {
                prefix_bytes = prefix_bytes.saturating_add(crate::summary::content_bytes(
                    &record.message,
                    summary_images,
                )?);
                if prefix_bytes > max_summary_bytes {
                    break;
                }
            }
            // Prefix size and coordinates only increase. Do not serialize later
            // prefixes once either bound rules out every remaining boundary.
            if prefix_bytes > max_summary_bytes {
                break;
            }
            prefix_fits.push(boundary);
        }
        let Some(&prefix_fit) = prefix_fits.last() else {
            return Ok(CompactionPlanning::CannotFitBounds);
        };
        // The suffix target still uses all settled cuts, not just bounded ones:
        // choosing a smaller prefix must retain the original chunking policy.
        let mut suffix_bytes = 2usize;
        for record in inputs_at(&store.working, &store.working.state.active_inputs) {
            suffix_bytes = suffix_bytes
                .saturating_add(crate::json_size::encoded_len(
                    &crate::prompt_size::NeutralMessage::from(&record.message),
                )?)
                .saturating_add(1);
        }
        let mut through = None;
        let mut suffix = projected.iter().rev().peekable();
        for &boundary in store.working.settled.range((previous + 1)..).rev() {
            while let Some(record) = suffix.next_if(|record| record.entry > boundary) {
                if store
                    .working
                    .state
                    .active_inputs
                    .binary_search(&record.entry)
                    .is_ok()
                {
                    continue;
                }
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
        let through = through.unwrap_or(latest);
        // New input does not consume the active assistant's call/result batch.
        // Keep it even when only an empty recent suffix would fit, but never
        // pin an ended Turn's batch merely because the next Turn has started.
        let through = if keep_bytes > 0 && store.working.state.active.is_some() {
            projected
                .iter()
                .rev()
                .find(|record| record.message.role == Role::Assistant)
                .filter(|record| {
                    store
                        .working
                        .state
                        .active_inputs
                        .first()
                        .is_some_and(|&input| record.entry > input)
                        && record
                            .message
                            .content
                            .iter()
                            .any(|part| matches!(part, Content::ToolCall(_)))
                })
                .and_then(|call| {
                    store
                        .working
                        .settled
                        .range((previous + 1)..call.entry)
                        .next_back()
                        .copied()
                })
                .filter(|&cut| through > cut)
                .unwrap_or(through)
        } else {
            through
        };
        let suffix_target = through;
        let through_entry = suffix_target.min(prefix_fit);
        let smaller_count = prefix_fits.partition_point(|&cut| cut < through_entry);
        let smaller_through_entry = smaller_count
            .checked_sub(1)
            .map(|last| prefix_fits[last / 2]);
        let mut messages = Vec::new();
        if let Some((through, summary)) = &store.working.state.compaction {
            messages.push(summary_message(*through, summary));
        }
        messages.extend(
            inputs_at(&store.working, &store.working.state.compaction_inputs)
                .map(|record| record.message.clone()),
        );
        let mut retained_input_bytes = 0usize;
        for record in inputs_at(&store.working, &store.working.state.active_inputs)
            .take_while(|record| record.entry <= through_entry)
        {
            retained_input_bytes = retained_input_bytes
                .saturating_add(crate::json_size::encoded_len(
                    &crate::prompt_size::NeutralMessage::from(&record.message),
                )?)
                .saturating_add(1);
        }
        messages.extend(
            projected
                .iter()
                .take_while(|record| record.entry <= through_entry)
                .map(|record| record.message.clone()),
        );
        Ok(CompactionPlanning::Plan(CompactionPlan {
            through_entry,
            messages,
            chunked: through_entry < suffix_target,
            smaller_through_entry,
            retained_input_bytes,
        }))
    }
}
