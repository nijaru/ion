use super::{CompactionConfig, TokenCounter};
use crate::provider::{ContentBlock, Message, Role};

/// Result of a pruning operation.
#[derive(Debug, Clone)]
pub struct PruningResult {
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub messages_modified: usize,
    pub tier_reached: PruningTier,
}

/// Which pruning tier was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruningTier {
    None,
    TruncateOutputs,
    RemoveOldOutputs,
}

/// Prune messages to reduce token count.
///
/// Applies tiers in order until under target:
/// 1. Truncate large tool outputs (>`max_tool_output_tokens`) to head+tail
/// 2. Remove old tool output content entirely (keep reference marker)
pub fn prune_messages(
    messages: &mut [Message],
    config: &CompactionConfig,
    counter: &TokenCounter,
    target_tokens: usize,
) -> PruningResult {
    let tokens_before = counter.count_messages(messages).total;

    if tokens_before <= target_tokens {
        return PruningResult {
            tokens_before,
            tokens_after: tokens_before,
            messages_modified: 0,
            tier_reached: PruningTier::None,
        };
    }

    // Tier 1: Truncate large tool outputs
    let modified_t1 = truncate_large_outputs(messages, config, *counter);
    let tokens_after_t1 = counter.count_messages(messages).total;

    if tokens_after_t1 <= target_tokens {
        return PruningResult {
            tokens_before,
            tokens_after: tokens_after_t1,
            messages_modified: modified_t1,
            tier_reached: PruningTier::TruncateOutputs,
        };
    }

    // Tier 2: Remove old tool output content (keep last N messages protected)
    let modified_t2 = remove_old_output_content(messages, config.protected_messages, *counter);
    let tokens_after_t2 = counter.count_messages(messages).total;

    PruningResult {
        tokens_before,
        tokens_after: tokens_after_t2,
        messages_modified: modified_t1 + modified_t2,
        tier_reached: PruningTier::RemoveOldOutputs,
    }
}

/// Tier 1: Truncate large tool outputs to head + tail.
fn truncate_large_outputs(
    messages: &mut [Message],
    config: &CompactionConfig,
    counter: TokenCounter,
) -> usize {
    let mut modified = 0;
    let max_tokens = config.max_tool_output_tokens;
    let keep_tokens = config.truncate_keep_tokens;

    for message in messages.iter_mut() {
        if message.role != Role::ToolResult {
            continue;
        }

        let mut new_blocks = Vec::new();
        let mut message_modified = false;

        for block in message.content.iter() {
            match block {
                ContentBlock::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                } => {
                    let tokens = counter.count_str(content);

                    if tokens > max_tokens {
                        let truncated = truncate_to_head_tail(content, keep_tokens, counter);
                        new_blocks.push(ContentBlock::ToolResult {
                            tool_call_id: tool_call_id.clone(),
                            content: truncated,
                            is_error: *is_error,
                        });
                        message_modified = true;
                    } else {
                        new_blocks.push(block.clone());
                    }
                }
                _ => new_blocks.push(block.clone()),
            }
        }

        if message_modified {
            message.content = std::sync::Arc::new(new_blocks);
            modified += 1;
        }
    }

    modified
}

/// Truncate content to approximately head + tail tokens.
fn truncate_to_head_tail(content: &str, keep_tokens: usize, _counter: TokenCounter) -> String {
    let lines: Vec<&str> = content.lines().collect();

    if lines.len() <= 10 {
        return content.to_string();
    }

    // Estimate lines to keep (rough: ~10 tokens per line average)
    let lines_per_section = (keep_tokens / 10).max(5);

    let head_lines = lines_per_section.min(lines.len() / 2);
    let tail_lines = lines_per_section.min(lines.len() / 2);

    let head: Vec<&str> = lines.iter().take(head_lines).copied().collect();
    let tail: Vec<&str> = lines.iter().rev().take(tail_lines).rev().copied().collect();

    let omitted = lines.len() - head_lines - tail_lines;

    format!(
        "{}\n\n... [{} lines truncated] ...\n\n{}",
        head.join("\n"),
        omitted,
        tail.join("\n")
    )
}

/// Tier 2: Remove content from old tool outputs, keeping just a reference.
fn remove_old_output_content(
    messages: &mut [Message],
    protected_count: usize,
    _counter: TokenCounter,
) -> usize {
    let mut modified = 0;
    let len = messages.len();

    if len <= protected_count {
        return 0;
    }

    let cutoff = len - protected_count;

    for message in &mut messages[..cutoff] {
        if message.role != Role::ToolResult {
            continue;
        }

        let mut new_blocks = Vec::new();
        let mut message_modified = false;

        for block in message.content.iter() {
            match block {
                ContentBlock::ToolResult {
                    tool_call_id,
                    content,
                    is_error,
                } => {
                    // Already pruned?
                    if content.starts_with("[Output removed") {
                        new_blocks.push(block.clone());
                        continue;
                    }

                    // Extract first line as summary hint
                    let first_line = content
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(100)
                        .collect::<String>();
                    let line_count = content.lines().count();

                    let placeholder = format!(
                        "[Output removed: {} lines, starting with: {}...]",
                        line_count,
                        first_line.trim()
                    );

                    new_blocks.push(ContentBlock::ToolResult {
                        tool_call_id: tool_call_id.clone(),
                        content: placeholder,
                        is_error: *is_error,
                    });
                    message_modified = true;
                }
                _ => new_blocks.push(block.clone()),
            }
        }

        if message_modified {
            message.content = std::sync::Arc::new(new_blocks);
            modified += 1;
        }
    }

    modified
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_tool_result(content: &str) -> Message {
        Message {
            role: Role::ToolResult,
            content: Arc::new(vec![ContentBlock::ToolResult {
                tool_call_id: "test".to_string(),
                content: content.to_string(),
                is_error: false,
            }]),
        }
    }

    fn make_text_message(role: Role, text: &str) -> Message {
        Message {
            role,
            content: Arc::new(vec![ContentBlock::Text {
                text: text.to_string(),
            }]),
        }
    }

    #[test]
    fn test_truncate_to_head_tail() {
        let counter = TokenCounter::new();
        let content = (0..100)
            .map(|i| format!("Line {}: some content", i))
            .collect::<Vec<_>>()
            .join("\n");

        let truncated = truncate_to_head_tail(&content, 100, counter);

        assert!(truncated.contains("Line 0"));
        assert!(truncated.contains("lines truncated"));
        assert!(truncated.contains("Line 99"));
        assert!(truncated.len() < content.len());
    }

    #[test]
    fn test_prune_large_output() {
        let config = CompactionConfig {
            max_tool_output_tokens: 100,
            truncate_keep_tokens: 20,
            ..Default::default()
        };
        let counter = TokenCounter::new();

        let large_content = (0..500)
            .map(|i| format!("Line {}: content here", i))
            .collect::<Vec<_>>()
            .join("\n");

        let mut messages = vec![
            make_text_message(Role::User, "Read the file"),
            make_tool_result(&large_content),
        ];

        let result = prune_messages(&mut messages, &config, &counter, 50);

        assert!(result.tokens_after < result.tokens_before);
        assert!(result.messages_modified > 0);
    }

    #[test]
    fn test_remove_old_outputs() {
        let config = CompactionConfig {
            protected_messages: 6,
            ..Default::default()
        };
        let counter = TokenCounter::new();

        // Need more messages than protected_messages (6) to trigger tier 2
        let mut messages = vec![
            make_text_message(Role::User, "First request"),
            make_tool_result("Old tool output content that should be removed"),
            make_text_message(Role::Assistant, "First response"),
            make_text_message(Role::User, "Second request"),
            make_tool_result("Second old tool output"),
            make_text_message(Role::Assistant, "Second response"),
            make_text_message(Role::User, "Third request"),
            make_tool_result("Third tool output - recent"),
            make_text_message(Role::Assistant, "Third response"),
            make_text_message(Role::User, "Fourth request"),
            make_tool_result("Fourth tool output - recent"),
            make_text_message(Role::Assistant, "Fourth response"),
        ];

        // Set very low target to force tier 2
        let _result = prune_messages(&mut messages, &config, &counter, 10);

        // Old output (index 1) should be replaced with placeholder
        if let ContentBlock::ToolResult { content, .. } = &messages[1].content[0] {
            assert!(
                content.starts_with("[Output removed"),
                "Old output should be pruned, got: {}",
                content
            );
        } else {
            panic!("Expected tool result");
        }

        // Recent output (index 10, within last 6) should be preserved
        if let ContentBlock::ToolResult { content, .. } = &messages[10].content[0] {
            assert!(
                !content.starts_with("[Output removed"),
                "Recent output should not be pruned"
            );
        }
    }

    #[test]
    fn test_no_pruning_needed() {
        let config = CompactionConfig::default();
        let counter = TokenCounter::new();

        let mut messages = vec![
            make_text_message(Role::User, "Hello"),
            make_text_message(Role::Assistant, "Hi there"),
        ];

        let result = prune_messages(&mut messages, &config, &counter, 100_000);

        assert_eq!(result.tier_reached, PruningTier::None);
        assert_eq!(result.messages_modified, 0);
    }
}
