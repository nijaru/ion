//! Tier 3: LLM-based summarization for context compaction.
//!
//! When mechanical pruning (Tier 1/2) cannot reach the target token count,
//! this module uses a small/fast LLM to produce a structured summary of
//! older conversation turns, replacing them with a single system message.

use crate::provider::{ChatRequest, ContentBlock, LlmApi, Message, Role};
use std::sync::Arc;

const SUMMARIZATION_PROMPT: &str = "\
Summarize the following conversation for seamless continuation.
Be thorough with technical details. Organize into these sections:

1. TASK STATE: Current goal, progress, remaining work items
2. FILES: All file paths read, written, or edited (full paths, list format)
3. TOOL HISTORY: Tools called and key outcomes (tool name + key result, condensed)
4. ERRORS: Problems encountered and resolutions
5. DECISIONS: Architectural/design choices made and rationale
6. USER GUIDANCE: Corrections, preferences, constraints from the user
7. NEXT STEPS: Immediate action to resume work

Preserve exact file paths, error messages, and code patterns.
Focus on information needed to continue without re-asking the user.";

/// Result of LLM-based summarization.
#[derive(Debug, Clone)]
pub struct SummarizationResult {
    /// The structured summary text.
    pub summary: String,
    /// Number of messages that were summarized (replaced).
    pub messages_summarized: usize,
    /// Token count of the summary message.
    pub summary_tokens: usize,
}

/// Summarize old messages using an LLM.
///
/// Takes messages before the protected cutoff, sends them to a small model
/// with a structured prompt, and returns the summary. The caller is responsible
/// for replacing the old messages with the summary.
pub async fn summarize_messages(
    messages: &[Message],
    protected_count: usize,
    provider: &dyn LlmApi,
    model: &str,
    counter: &crate::compaction::TokenCounter,
) -> Result<SummarizationResult, crate::provider::Error> {
    let len = messages.len();
    if len <= protected_count {
        return Ok(SummarizationResult {
            summary: String::new(),
            messages_summarized: 0,
            summary_tokens: 0,
        });
    }

    let cutoff = len - protected_count;
    let old_messages = &messages[..cutoff];

    // Build conversation text for summarization
    let conversation_text = format_messages_for_summary(old_messages);

    // Create summarization request
    let request = ChatRequest {
        model: model.to_string(),
        messages: Arc::new(vec![Message {
            role: Role::User,
            content: Arc::new(vec![ContentBlock::Text {
                text: format!("{SUMMARIZATION_PROMPT}\n\n---\n\n{conversation_text}"),
            }]),
        }]),
        system: None,
        tools: Arc::new(vec![]),
        max_tokens: Some(8_000),
        temperature: Some(0.0),
        thinking: None,
    };

    let response = provider.complete(request).await?;

    // Extract text from response
    let summary = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let summary_tokens = counter.count_str(&summary);

    Ok(SummarizationResult {
        summary,
        messages_summarized: cutoff,
        summary_tokens,
    })
}

/// Apply a summarization result to messages, replacing old turns with the summary.
///
/// Returns the new message list: `[summary_message, ...protected_messages]`
pub fn apply_summary(messages: &[Message], result: &SummarizationResult) -> Vec<Message> {
    if result.messages_summarized == 0 || result.summary.is_empty() {
        return messages.to_vec();
    }

    let summary_message = Message {
        role: Role::User,
        content: Arc::new(vec![ContentBlock::Text {
            text: format!(
                "<context-summary>\n{}\n</context-summary>\n\n\
                 The above is a summary of the earlier conversation. \
                 Continue from where we left off without re-asking the user.",
                result.summary
            ),
        }]),
    };

    let protected = &messages[result.messages_summarized..];
    let mut new_messages = Vec::with_capacity(1 + protected.len());
    new_messages.push(summary_message);
    new_messages.extend_from_slice(protected);
    new_messages
}

/// Format messages into readable text for the summarization model.
fn format_messages_for_summary(messages: &[Message]) -> String {
    let mut parts = Vec::new();

    for msg in messages {
        let role_label = match msg.role {
            Role::System => "System",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::ToolResult => "Tool Result",
        };

        for block in msg.content.iter() {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(format!("[{role_label}]: {text}"));
                }
                ContentBlock::Thinking { thinking } => {
                    // Include abbreviated thinking
                    let abbreviated = if thinking.len() > 500 {
                        format!("{}... [truncated]", &thinking[..500])
                    } else {
                        thinking.clone()
                    };
                    parts.push(format!("[{role_label} thinking]: {abbreviated}"));
                }
                ContentBlock::ToolCall {
                    name, arguments, ..
                } => {
                    // Compact tool call representation
                    let args_str = arguments.to_string();
                    let args_display = if args_str.len() > 200 {
                        format!("{}...", &args_str[..200])
                    } else {
                        args_str
                    };
                    parts.push(format!("[Tool call: {name}({args_display})"));
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let prefix = if *is_error { "Error" } else { "Result" };
                    // Already-pruned outputs are short; include full
                    let display = if content.len() > 500 {
                        format!("{}... [truncated]", &content[..500])
                    } else {
                        content.clone()
                    };
                    parts.push(format!("[Tool {prefix}]: {display}"));
                }
                ContentBlock::Image { .. } => {
                    parts.push(format!("[{role_label}]: [image]"));
                }
            }
        }
    }

    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_text(role: Role, text: &str) -> Message {
        Message {
            role,
            content: Arc::new(vec![ContentBlock::Text {
                text: text.to_string(),
            }]),
        }
    }

    #[test]
    fn test_format_messages_for_summary() {
        let messages = vec![
            make_text(Role::User, "Read main.rs"),
            make_text(Role::Assistant, "I'll read that file."),
            Message {
                role: Role::ToolResult,
                content: Arc::new(vec![ContentBlock::ToolResult {
                    tool_call_id: "1".to_string(),
                    content: "fn main() { println!(\"hello\"); }".to_string(),
                    is_error: false,
                }]),
            },
        ];

        let formatted = format_messages_for_summary(&messages);
        assert!(formatted.contains("[User]: Read main.rs"));
        assert!(formatted.contains("[Assistant]: I'll read that file."));
        assert!(formatted.contains("[Tool Result]: fn main()"));
    }

    #[test]
    fn test_apply_summary_empty() {
        let messages = vec![make_text(Role::User, "hello")];
        let result = SummarizationResult {
            summary: String::new(),
            messages_summarized: 0,
            summary_tokens: 0,
        };

        let new_messages = apply_summary(&messages, &result);
        assert_eq!(new_messages.len(), 1);
    }

    #[test]
    fn test_apply_summary_replaces_old() {
        let messages = vec![
            make_text(Role::User, "old message 1"),
            make_text(Role::Assistant, "old response 1"),
            make_text(Role::User, "old message 2"),
            make_text(Role::Assistant, "old response 2"),
            make_text(Role::User, "recent message"),    // protected
            make_text(Role::Assistant, "recent response"), // protected
        ];

        let result = SummarizationResult {
            summary: "Summary of earlier work".to_string(),
            messages_summarized: 4, // first 4 messages summarized
            summary_tokens: 10,
        };

        let new_messages = apply_summary(&messages, &result);
        // 1 summary + 2 protected
        assert_eq!(new_messages.len(), 3);

        // First message is the summary
        if let ContentBlock::Text { text } = &new_messages[0].content[0] {
            assert!(text.contains("Summary of earlier work"));
            assert!(text.contains("context-summary"));
        } else {
            panic!("Expected text block");
        }

        // Protected messages preserved
        if let ContentBlock::Text { text } = &new_messages[1].content[0] {
            assert_eq!(text, "recent message");
        }
    }
}
