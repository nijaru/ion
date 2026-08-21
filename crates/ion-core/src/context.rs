//! Model-facing context projection (DESIGN.md §14).
//!
//! Local semantic state is canonical; the model sees only a
//! deterministic projection of it (P7, §31 invariant 15): the same
//! entries and configuration always yield the same
//! [`ContextPlan`]. This module is the v0 projector; the
//! ContextManifest and compaction machinery extend it in later slices.

use crate::session::SessionEntry;
use crate::tool::ToolCall;

/// The small, stable system section every model step sees (DESIGN.md
/// §14.4: no timestamps or random values in early prompt sections).
pub const SYSTEM_SECTION: &str = "You are Ion, a terminal coding agent. \
You work inside the user's project directory. \
Use the provided tools to read, write, edit, search, and run commands. \
Prefer tools over guessing; report failures plainly.";

/// One model-facing message in the projected conversation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContextMessage {
    User {
        content: String,
    },
    Assistant {
        content: String,
        tool_calls: Vec<ToolCall>,
    },
    /// A model-visible tool result, paired by call id.
    Tool {
        call_id: u64,
        content: String,
    },
}

/// The exact semantic projection for one model step (DESIGN.md §6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextPlan {
    pub system: String,
    pub messages: Vec<ContextMessage>,
}

/// Project session entries into a model-neutral plan. Pure and
/// deterministic.
#[must_use]
pub fn project(entries: &[SessionEntry]) -> ContextPlan {
    let mut messages: Vec<ContextMessage> = Vec::new();
    for entry in entries {
        match entry {
            SessionEntry::UserMessage { text } => {
                messages.push(ContextMessage::User {
                    content: text.clone(),
                });
            }
            SessionEntry::AssistantMessage { text } => {
                messages.push(ContextMessage::Assistant {
                    content: text.clone(),
                    tool_calls: Vec::new(),
                });
            }
            SessionEntry::ToolCall { call } => {
                // Attach to the preceding assistant message; synthesize
                // one if the stream starts with a call.
                match messages.last_mut() {
                    Some(ContextMessage::Assistant { tool_calls, .. }) => {
                        tool_calls.push(call.clone());
                    }
                    _ => {
                        messages.push(ContextMessage::Assistant {
                            content: String::new(),
                            tool_calls: vec![call.clone()],
                        });
                    }
                }
            }
            SessionEntry::ToolResult { result } => {
                let (call_id, content) = match result {
                    crate::tool::ToolResult::Ok { call_id, output } => (*call_id, output.clone()),
                    crate::tool::ToolResult::Err { call_id, error } => (*call_id, error.clone()),
                };
                messages.push(ContextMessage::Tool { call_id, content });
            }
        }
    }
    ContextPlan {
        system: SYSTEM_SECTION.to_owned(),
        messages,
    }
}
