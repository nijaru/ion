use crate::provider::{ContentBlock, Message, Role};
use tiktoken_rs::cl100k_base;

/// Token counter using tiktoken for accurate counting.
use std::fmt;

pub struct TokenCounter {
    bpe: tiktoken_rs::CoreBPE,
}

impl Clone for TokenCounter {
    fn clone(&self) -> Self {
        Self::new()
    }
}

impl fmt::Debug for TokenCounter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenCounter").finish()
    }
}

impl Default for TokenCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl TokenCounter {
    pub fn new() -> Self {
        Self {
            bpe: cl100k_base().expect("Failed to load cl100k_base tokenizer"),
        }
    }

    /// Count tokens in a string.
    pub fn count_str(&self, text: &str) -> usize {
        self.bpe.encode_with_special_tokens(text).len()
    }

    /// Fast estimate without tokenization (~4 chars per token).
    pub fn estimate_str(text: &str) -> usize {
        text.len() / 4
    }

    /// Count tokens in a single message.
    pub fn count_message(&self, message: &Message) -> MessageTokenCount {
        let mut text_tokens = 0;
        let mut tool_tokens = 0;

        for block in message.content.iter() {
            match block {
                ContentBlock::Text { text } => {
                    text_tokens += self.count_str(text);
                }
                ContentBlock::Thinking { thinking } => {
                    text_tokens += self.count_str(thinking);
                }
                ContentBlock::ToolCall {
                    name, arguments, ..
                } => {
                    tool_tokens += self.count_str(name);
                    tool_tokens += self.count_str(&arguments.to_string());
                }
                ContentBlock::ToolResult { content, .. } => {
                    tool_tokens += self.count_str(content);
                }
                ContentBlock::Image { data, .. } => {
                    // Images are base64, estimate ~0.75 tokens per char
                    tool_tokens += data.len() * 3 / 4 / 4;
                }
            }
        }

        // Add overhead for message structure (~4 tokens per message)
        let overhead = 4;

        MessageTokenCount {
            role: message.role,
            text_tokens,
            tool_tokens,
            total: text_tokens + tool_tokens + overhead,
        }
    }

    /// Count tokens across all messages.
    pub fn count_messages(&self, messages: &[Message]) -> TokenCount {
        let mut total = 0;
        let mut by_role = RoleTokens::default();
        let mut tool_output_tokens = 0;

        for message in messages {
            let count = self.count_message(message);
            total += count.total;

            match count.role {
                Role::System => by_role.system += count.total,
                Role::User => by_role.user += count.total,
                Role::Assistant => by_role.assistant += count.total,
                Role::ToolResult => {
                    by_role.tool_result += count.total;
                    tool_output_tokens += count.tool_tokens;
                }
            }
        }

        TokenCount {
            total,
            by_role,
            tool_output_tokens,
            message_count: messages.len(),
        }
    }
}

/// Token count for a single message.
#[derive(Debug, Clone)]
pub struct MessageTokenCount {
    pub role: Role,
    pub text_tokens: usize,
    pub tool_tokens: usize,
    pub total: usize,
}

/// Aggregated token count across messages.
#[derive(Debug, Clone, Default)]
pub struct TokenCount {
    pub total: usize,
    pub by_role: RoleTokens,
    pub tool_output_tokens: usize,
    pub message_count: usize,
}

/// Token counts by role.
#[derive(Debug, Clone, Default)]
pub struct RoleTokens {
    pub system: usize,
    pub user: usize,
    pub assistant: usize,
    pub tool_result: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_text_message(role: Role, text: &str) -> Message {
        Message {
            role,
            content: Arc::new(vec![ContentBlock::Text {
                text: text.to_string(),
            }]),
        }
    }

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

    #[test]
    fn test_count_str() {
        let counter = TokenCounter::new();
        // "Hello, world!" is typically 4 tokens
        let count = counter.count_str("Hello, world!");
        assert!(count > 0 && count < 10);
    }

    #[test]
    fn test_estimate_str() {
        // 100 chars should be ~25 tokens
        let text = "a".repeat(100);
        assert_eq!(TokenCounter::estimate_str(&text), 25);
    }

    #[test]
    fn test_count_message() {
        let counter = TokenCounter::new();
        let msg = make_text_message(Role::User, "Hello, how are you?");
        let count = counter.count_message(&msg);

        assert_eq!(count.role, Role::User);
        assert!(count.text_tokens > 0);
        assert_eq!(count.tool_tokens, 0);
        assert!(count.total > count.text_tokens); // includes overhead
    }

    #[test]
    fn test_count_messages() {
        let counter = TokenCounter::new();
        let messages = vec![
            make_text_message(Role::User, "What is 2 + 2?"),
            make_text_message(Role::Assistant, "The answer is 4."),
            make_tool_result("File contents here..."),
        ];

        let count = counter.count_messages(&messages);

        assert_eq!(count.message_count, 3);
        assert!(count.by_role.user > 0);
        assert!(count.by_role.assistant > 0);
        assert!(count.by_role.tool_result > 0);
        assert!(count.tool_output_tokens > 0);
    }

    #[test]
    fn test_large_tool_output() {
        let counter = TokenCounter::new();
        // Repeated single chars compress heavily in BPE
        // Use more realistic content
        let large_content = (0..1000)
            .map(|i| format!("line {}: some content here\n", i))
            .collect::<String>();
        let msg = make_tool_result(&large_content);
        let count = counter.count_message(&msg);

        // ~28 chars per line * 1000 = 28k chars, should be several thousand tokens
        assert!(count.tool_tokens > 1000, "got {} tokens", count.tool_tokens);
    }
}
