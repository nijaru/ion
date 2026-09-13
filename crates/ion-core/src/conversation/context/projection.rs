use std::collections::{HashMap, HashSet};

use ion_ai::{Content, Message, Role, ToolResult};
use thiserror::Error;

use super::ContextEdit;
use crate::{Entry, EntryId};

#[derive(Debug, Clone, PartialEq)]
pub struct ContextProjection {
    pub entry_ids: Vec<EntryId>,
    pub messages: Vec<Message>,
}

pub fn project(entries: &[Entry]) -> Result<ContextProjection, ContextError> {
    if entries.is_empty() {
        return Ok(ContextProjection {
            entry_ids: Vec::new(),
            messages: Vec::new(),
        });
    }

    let newest_head = entries
        .iter()
        .rposition(|entry| entry.context.head.is_some());
    let mut selected = if let Some(head_index) = newest_head {
        let head_entry = &entries[head_index];
        let boundary = head_entry.context.head.expect("head entry has boundary");
        let boundary_index = entries
            .iter()
            .position(|entry| entry.id == boundary)
            .ok_or(ContextError::InvisibleHead {
                entry: head_entry.id,
                head: boundary,
            })?;
        if boundary_index > head_index {
            return Err(ContextError::HeadPointsForward {
                entry: head_entry.id,
                head: boundary,
            });
        }

        let mut selected = Vec::with_capacity(entries.len() - boundary_index + 1);
        selected.push(head_entry);
        selected.extend(
            entries[boundary_index..]
                .iter()
                .filter(|entry| entry.context.head.is_none()),
        );
        selected
    } else {
        entries.iter().collect()
    };

    let selected_ids: HashSet<_> = selected.iter().map(|entry| entry.id).collect();
    let mut winning_edits = HashMap::new();
    for entry in entries {
        if selected_ids.contains(&entry.id) {
            for edit in &entry.context.edits {
                winning_edits.insert(edit.target(), edit);
            }
        }
    }

    let entry_ids = selected.iter().map(|entry| entry.id).collect();
    let mut messages = Vec::new();
    for entry in selected.drain(..) {
        match winning_edits.get(&entry.id) {
            Some(ContextEdit::Omit { .. }) => {}
            Some(ContextEdit::Replace {
                messages: replacement,
                ..
            }) => messages.extend(replacement.iter().cloned()),
            None => messages.extend(entry.projection.iter().cloned()),
        }
    }

    Ok(ContextProjection {
        entry_ids,
        messages: normalize_tool_exchanges(messages)?,
    })
}

fn normalize_tool_exchanges(messages: Vec<Message>) -> Result<Vec<Message>, ContextError> {
    let mut output = Vec::with_capacity(messages.len());
    let mut index = 0;

    while index < messages.len() {
        let message = &messages[index];
        // A tool result is only valid inside the exchange of the assistant
        // message that made its call. Encountering one here means the call was
        // omitted or a head/cutoff split the exchange, which is not a valid
        // provider request or safe fork boundary.
        if message.role == Role::Tool {
            return Err(ContextError::OrphanToolResult);
        }
        if message.role != Role::Assistant {
            output.push(message.clone());
            index += 1;
            continue;
        }

        let calls: Vec<_> = message
            .content
            .iter()
            .filter_map(|content| match content {
                Content::ToolCall(call) => Some(call.id.as_str()),
                Content::Text(_) | Content::ToolResult(_) => None,
            })
            .collect();
        if calls.is_empty() {
            output.push(message.clone());
            index += 1;
            continue;
        }

        let mut unique_calls = HashSet::with_capacity(calls.len());
        for call_id in &calls {
            if !unique_calls.insert((*call_id).to_owned()) {
                return Err(ContextError::DuplicateToolCall((*call_id).to_owned()));
            }
        }

        output.push(message.clone());
        index += 1;
        let mut results: HashMap<String, (ToolResult, Option<ion_ai::ProviderReplay>)> =
            HashMap::new();
        while index < messages.len() && messages[index].role == Role::Tool {
            let tool_message = &messages[index];
            for content in &tool_message.content {
                let Content::ToolResult(result) = content else {
                    return Err(ContextError::InvalidToolMessage);
                };
                if results
                    .insert(
                        result.call_id.clone(),
                        (result.clone(), tool_message.provider_replay.clone()),
                    )
                    .is_some()
                {
                    return Err(ContextError::DuplicateToolResult(result.call_id.clone()));
                }
            }
            index += 1;
        }

        for call_id in calls {
            let Some((result, provider_replay)) = results.remove(call_id) else {
                return Err(ContextError::MissingToolResult(call_id.to_owned()));
            };
            output.push(Message {
                role: Role::Tool,
                content: vec![Content::ToolResult(result)],
                provider_replay,
            });
        }
        if let Some(call_id) = results.keys().next() {
            return Err(ContextError::UnknownToolResult(call_id.clone()));
        }
    }

    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContextError {
    #[error("context head {head} on entry {entry} is not visible")]
    InvisibleHead { entry: EntryId, head: EntryId },
    #[error("context head {head} on entry {entry} points forward")]
    HeadPointsForward { entry: EntryId, head: EntryId },
    #[error("assistant contains duplicate tool call id {0}")]
    DuplicateToolCall(String),
    #[error("tool message contains non-result content")]
    InvalidToolMessage,
    #[error("duplicate tool result for call {0}")]
    DuplicateToolResult(String),
    #[error("tool call {0} has no result at this context boundary")]
    MissingToolResult(String),
    #[error("tool result appears without its originating assistant call in this context")]
    OrphanToolResult,
    #[error("tool result references unknown call {0}")]
    UnknownToolResult(String),
}
