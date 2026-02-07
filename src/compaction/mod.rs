//! Context compaction for managing conversation length.
#![allow(clippy::cast_precision_loss)] // Intentional for threshold calculations

mod counter;
mod pruning;

pub use counter::{TokenCount, TokenCounter};
pub use pruning::{prune_messages, PruningResult, PruningTier};

use crate::provider::Message;

/// Configuration for context compaction.
#[derive(Debug, Clone)]
pub struct CompactionConfig {
    /// Context window size for the model (default: `200_000`)
    pub context_window: usize,
    /// Trigger compaction at this percentage of available context (default: 0.80)
    pub trigger_threshold: f32,
    /// Target percentage after compaction (default: 0.60)
    pub target_threshold: f32,
    /// Tokens to reserve for output (default: `16_000`)
    pub output_reserve: usize,
    /// Maximum tokens per tool output before truncation (default: `2_000`)
    pub max_tool_output_tokens: usize,
    /// Tokens to keep at head/tail when truncating (default: 250 each)
    pub truncate_keep_tokens: usize,
    /// Number of recent messages protected from Tier 2 pruning (default: 12)
    pub protected_messages: usize,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self {
            context_window: 200_000,
            // Match Claude Code range (70-85%). Modern models handle long contexts well.
            trigger_threshold: 0.80,
            // 20% gap frees ~37k tokens per compaction on 200k context
            target_threshold: 0.60,
            output_reserve: 16_000,
            max_tool_output_tokens: 2_000,
            truncate_keep_tokens: 250,
            // ~4 full turns (user + assistant + tool_result per turn)
            protected_messages: 12,
        }
    }
}

impl CompactionConfig {
    /// Available tokens after reserving output space.
    #[must_use]
    pub fn available_tokens(&self) -> usize {
        self.context_window.saturating_sub(self.output_reserve)
    }

    /// Token count that triggers compaction.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn trigger_tokens(&self) -> usize {
        (self.available_tokens() as f32 * self.trigger_threshold) as usize
    }

    /// Target token count after compaction.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn target_tokens(&self) -> usize {
        (self.available_tokens() as f32 * self.target_threshold) as usize
    }
}

/// Result of checking whether compaction is needed.
#[derive(Debug, Clone)]
pub struct CompactionStatus {
    pub total_tokens: usize,
    pub trigger_tokens: usize,
    pub needs_compaction: bool,
    pub message_count: usize,
}

/// Check if messages need compaction.
#[must_use]
pub fn check_compaction_needed(
    messages: &[Message],
    config: &CompactionConfig,
    counter: &TokenCounter,
) -> CompactionStatus {
    let count = counter.count_messages(messages);
    let trigger = config.trigger_tokens();

    CompactionStatus {
        total_tokens: count.total,
        trigger_tokens: trigger,
        needs_compaction: count.total >= trigger,
        message_count: messages.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = CompactionConfig::default();
        assert_eq!(config.context_window, 200_000);
        assert_eq!(config.trigger_threshold, 0.80);
        assert_eq!(config.target_threshold, 0.60);
        assert_eq!(config.protected_messages, 12);
    }

    #[test]
    fn test_trigger_tokens() {
        let config = CompactionConfig::default();
        // Available: 200k - 16k = 184k
        // Trigger: 184k * 0.80 = 147,200
        assert_eq!(config.trigger_tokens(), 147_200);
    }

    #[test]
    fn test_target_tokens() {
        let config = CompactionConfig::default();
        // Available: 184k
        // Target: 184k * 0.60 = 110,400
        assert_eq!(config.target_tokens(), 110_400);
    }
}
