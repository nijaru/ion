//! Request assembly from a frozen basis.
//!
//! Assembly reads durable entries and turns them into the exact provider
//! message list a step promised. It is a pure function of the frozen basis and
//! the stored transcript, so a retry sends the same request, and a recovered
//! process rebuilds the same messages without rereading any external file.
//!
//! It refuses to produce a malformed exchange. Asking a provider to complete an
//! assistant message whose tool calls have no results is not a retry; it is a
//! different request, and the engine will not fabricate the missing results to
//! make it valid.

use std::collections::BTreeMap;

use ion_ai::{Content, Message, Role};
use thiserror::Error;

use crate::attempt::ModelStep;
use crate::entry::Entry;

#[derive(Debug, Clone, PartialEq)]
pub struct AssembledRequest {
    pub instructions: String,
    pub messages: Vec<Message>,
    /// Serialized size of the assembled request, for the byte budget.
    pub bytes: u64,
}

/// Build the provider request for `step` from `entries` (conversation order).
pub fn assemble(step: &ModelStep, entries: &[Entry]) -> Result<AssembledRequest, RequestError> {
    let mut messages: Vec<Message> = step.context.clone();
    // Call ids of the assistant message currently awaiting tool results,
    // together with the entry that introduced them.
    let mut pending: BTreeMap<&str, ()> = BTreeMap::new();
    let mut pending_from: Option<crate::EntryId> = None;
    let mut last_id = None;

    for entry in entries {
        if let Some(cut) = step.cut
            && entry.id > cut
        {
            break;
        }
        if let Some(previous) = last_id
            && entry.id <= previous
        {
            return Err(RequestError::OutOfOrder {
                previous,
                found: entry.id,
            });
        }
        last_id = Some(entry.id);

        for message in &entry.projection {
            match message.role {
                Role::Assistant => {
                    if !pending.is_empty() {
                        return Err(RequestError::UnansweredCalls {
                            entry: entry.id,
                            missing: pending.keys().map(|id| (*id).to_owned()).collect(),
                        });
                    }
                    for block in &message.content {
                        if let Content::ToolCall(call) = block {
                            if call.id.is_empty() {
                                return Err(RequestError::EmptyCallId { entry: entry.id });
                            }
                            if pending.insert(&call.id, ()).is_some() {
                                return Err(RequestError::DuplicateCallId {
                                    entry: entry.id,
                                    call: call.id.clone(),
                                });
                            }
                            pending_from = Some(entry.id);
                        }
                    }
                }
                Role::Tool => {
                    for block in &message.content {
                        if let Content::ToolResult(result) = block
                            && pending.remove(result.call_id.as_str()).is_none()
                        {
                            return Err(RequestError::OrphanToolResult {
                                entry: entry.id,
                                call: result.call_id.clone(),
                            });
                        }
                    }
                }
                Role::User => {
                    if !pending.is_empty() {
                        return Err(RequestError::UnansweredCalls {
                            entry: entry.id,
                            missing: pending.keys().map(|id| (*id).to_owned()).collect(),
                        });
                    }
                }
            }
            messages.push(message.clone());
        }
    }

    if !pending.is_empty() {
        let entry = pending_from.or(last_id).ok_or(RequestError::EmptyBasis)?;
        return Err(RequestError::UnansweredCalls {
            entry,
            missing: pending.keys().map(|id| (*id).to_owned()).collect(),
        });
    }

    let bytes = serde_json::to_vec(&messages).map_or(0, |encoded| encoded.len() as u64);
    Ok(AssembledRequest {
        instructions: step.instructions.clone(),
        messages,
        bytes,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RequestError {
    #[error("entry {found} appears after {previous}; the transcript is not in order")]
    OutOfOrder {
        previous: crate::EntryId,
        found: crate::EntryId,
    },
    #[error("entry {entry} continues an exchange whose calls {missing:?} have no results")]
    UnansweredCalls {
        entry: crate::EntryId,
        missing: Vec<String>,
    },
    #[error("entry {entry} carries a tool result for unknown call {call:?}")]
    OrphanToolResult { entry: crate::EntryId, call: String },
    #[error("entry {entry} repeats the call id {call:?} within one message")]
    DuplicateCallId { entry: crate::EntryId, call: String },
    #[error("entry {entry} carries a tool call with an empty call id")]
    EmptyCallId { entry: crate::EntryId },
    #[error("the frozen basis selects no conversation entry")]
    EmptyBasis,
}
