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

/// The instruction sent with a compaction step (DESIGN.md §14.7).
/// Part of the deterministic plan, so recovery replays identically.
pub const SUMMARIZE_INSTRUCTION: &str = "Summarize the conversation above into a compact \
handoff for a future assistant instance. Preserve: the user's goal, \
decisions made, files touched, and the exact next step. Omit pleasantries.";

/// The exact semantic projection for one model step (DESIGN.md §6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ContextPlan {
    pub system: String,
    pub messages: Vec<ContextMessage>,
}

impl ContextMessage {
    /// The readable text content of this message, whatever its role.
    #[must_use]
    pub fn prompt_text(&self) -> &str {
        match self {
            Self::User { content } | Self::Tool { content, .. } => content,
            Self::Assistant { content, .. } => content,
        }
    }
}

/// Project session entries into a model-neutral plan. Pure and
/// deterministic. `first_seq` is the durable seq of `entries[0]`, so a
/// compaction baseline can name what it covers (§14.7).
#[must_use]
pub fn project(entries: &[SessionEntry], first_seq: u64) -> ContextPlan {
    let mut messages: Vec<ContextMessage> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        match entry {
            SessionEntry::Compaction {
                covers_through_seq,
                summary,
            } => {
                // Lossy projection maintenance (§14.7): the summary
                // replaces everything through its coverage boundary;
                // canonical entries stay durable.
                if (*covers_through_seq + 1) >= first_seq + index as u64 {
                    messages.clear();
                }
                messages.push(ContextMessage::User {
                    content: format!("[Context summary of the earlier conversation]\n{summary}"),
                });
            }
            SessionEntry::UserMessage { text } => {
                messages.push(ContextMessage::User {
                    content: text.clone(),
                });
            }
            SessionEntry::ModelChanged { .. } => {
                // Configuration lineage is canonical session state, not
                // a conversational message.
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

/// Compact token count in pi hint style: `1m`, `128k`, or the raw
/// number below a thousand.
fn fmt_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 && tokens.is_multiple_of(1_000_000) {
        format!("{}m", tokens / 1_000_000)
    } else if tokens >= 1_000 && tokens.is_multiple_of(1_000) {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// The context-usage hint for one model step, if one is due (§14.7.2).
///
/// Data only, never an instruction: the model reasons about its own
/// budget. The first hint fires at `min(50% window, 128k)` tokens;
/// re-hints are throttled to material growth (5% of the window or
/// 25k tokens, whichever is smaller). An unknown window hints at the
/// absolute threshold with no denominator. Returns the message text
/// for a trailing synthetic user message; the caller never persists it.
#[must_use]
pub fn usage_hint(
    context_tokens: u64,
    context_window: Option<u64>,
    last_hint_tokens: Option<u64>,
) -> Option<String> {
    let first_threshold = match context_window {
        Some(window) => (window / 2).min(128_000),
        None => 128_000,
    };
    if context_tokens < first_threshold {
        return None;
    }
    let delta = match context_window {
        Some(window) => (window * 5 / 100).clamp(1, 25_000),
        None => 25_000,
    };
    if let Some(last) = last_hint_tokens
        && context_tokens < last.saturating_add(delta)
    {
        return None;
    }
    let mut text = match context_window {
        Some(window) => format!(
            "[ctx {}/{}]",
            fmt_tokens(context_tokens),
            fmt_tokens(window)
        ),
        None => format!("[ctx {}/?]", fmt_tokens(context_tokens)),
    };
    if context_tokens > 200_000 {
        text.push_str(" [>200k]");
    }
    Some(text)
}

/// Append the given hint as a trailing synthetic user message on the
/// plan (trailing edge so prefix-cache reuse of earlier content is
/// preserved).
pub fn push_hint(plan: &mut ContextPlan, hint: String) {
    plan.messages.push(ContextMessage::User { content: hint });
}

/// The hidden recovery turn's prompt after a compaction with
/// `continue_after_compaction` (DESIGN.md §14.7.3): resume only
/// unfinished work without repeating settled effects.
pub const RESUME_MESSAGE: &str = "The conversation context was just compacted. Resume only the \
unfinished work from before the compaction; do not repeat any action that already completed. \
When nothing remains, give a final response.";

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn first_hint_at_half_window() {
        // Small window: hint at 50%.
        assert_eq!(usage_hint(499, Some(1_000), None), None);
        let hint = usage_hint(500, Some(1_000), None).expect("hint due");
        assert_eq!(hint, "[ctx 500/1k]");
    }

    #[test]
    fn large_windows_hint_at_the_128k_cap() {
        // A 1m window still hints by 128k: models degrade and cost
        // more well before the window fills (14.7.2).
        assert_eq!(usage_hint(127_999, Some(1_000_000), None), None);
        let hint = usage_hint(128_000, Some(1_000_000), None).expect("hint due");
        assert_eq!(hint, "[ctx 128k/1m]");
    }

    #[test]
    fn first_hint_capped_at_128k_for_large_windows() {
        let hint = usage_hint(128_000, Some(10_000_000), None).expect("hint due");
        assert_eq!(hint, "[ctx 128k/10m]");
    }

    #[test]
    fn small_window_hints_early() {
        let hint = usage_hint(600, Some(1_000), None).expect("hint due");
        assert_eq!(hint, "[ctx 600/1k]");
    }

    #[test]
    fn unknown_window_uses_absolute_threshold_and_no_denominator() {
        assert_eq!(usage_hint(127_999, None, None), None);
        let hint = usage_hint(150_000, None, None).expect("hint due");
        assert_eq!(hint, "[ctx 150k/?]");
    }

    #[test]
    fn cost_marker_above_200k() {
        let hint = usage_hint(240_000, Some(1_000_000), None).expect("hint due");
        assert_eq!(hint, "[ctx 240k/1m] [>200k]");
    }

    #[test]
    fn rehints_throttled_by_window_delta() {
        // 5% of 1m = 25k... capped at 25k: growth from 500k to 520k is
        // below the delta, so no new hint.
        assert_eq!(usage_hint(520_000, Some(1_000_000), Some(500_000)), None);
        let hint = usage_hint(530_000, Some(1_000_000), Some(500_000)).expect("rehint due");
        assert_eq!(hint, "[ctx 530k/1m] [>200k]");
    }

    #[test]
    fn rehints_throttled_by_absolute_delta_without_window() {
        assert_eq!(usage_hint(160_000, None, Some(150_000)), None);
        assert!(usage_hint(176_000, None, Some(150_000)).is_some());
    }
}
