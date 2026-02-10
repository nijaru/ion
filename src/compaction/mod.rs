//! Context compaction for managing conversation length.
#![allow(clippy::cast_precision_loss)] // Intentional for threshold calculations

mod counter;
mod pruning;
mod summarization;

pub use counter::{TokenCount, TokenCounter};
pub use pruning::{prune_messages, PruningResult, PruningTier};
pub use summarization::{SummarizationResult, apply_summary, summarize_messages};

use crate::provider::{LlmApi, Message, Usage};

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

/// Result of full compaction pipeline (Tier 1 + 2 + 3).
#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub tokens_before: usize,
    pub tokens_after: usize,
    pub tier_reached: CompactionTier,
    /// Summary text if Tier 3 was applied.
    pub summary: Option<String>,
    /// Provider-reported token usage from the Tier 3 summarization API call.
    pub api_usage: Option<Usage>,
}

/// Which compaction tier was applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTier {
    None,
    /// Tier 1/2: mechanical pruning only
    Mechanical,
    /// Tier 3: LLM-based summarization
    Summarized,
}

/// Run the full compaction pipeline: Tier 1 → 2 → 3.
///
/// Tier 1/2 are synchronous mechanical pruning. If still over target,
/// Tier 3 uses an LLM to summarize old conversation turns.
pub async fn compact_with_summarization(
    messages: &mut Vec<Message>,
    config: &CompactionConfig,
    counter: &TokenCounter,
    provider: &dyn LlmApi,
    model: &str,
) -> CompactionResult {
    let tokens_before = counter.count_messages(messages).total;
    let target = config.target_tokens();

    if tokens_before <= target {
        return CompactionResult {
            tokens_before,
            tokens_after: tokens_before,
            tier_reached: CompactionTier::None,
            summary: None,
            api_usage: None,
        };
    }

    // Tier 1 + 2: mechanical pruning
    let pruning_result = prune_messages(messages, config, counter, target);
    let tokens_after_mechanical = pruning_result.tokens_after;

    if tokens_after_mechanical <= target {
        return CompactionResult {
            tokens_before,
            tokens_after: tokens_after_mechanical,
            tier_reached: CompactionTier::Mechanical,
            summary: None,
            api_usage: None,
        };
    }

    // Tier 3: LLM summarization
    tracing::info!(
        tokens_after_mechanical,
        target,
        "Tier 1/2 insufficient, running Tier 3 LLM summarization"
    );

    match summarize_messages(messages, config.protected_messages, provider, model, counter).await {
        Ok(result) if !result.summary.is_empty() => {
            let new_messages = apply_summary(messages, &result);
            let tokens_after = counter.count_messages(&new_messages).total;

            tracing::info!(
                messages_summarized = result.messages_summarized,
                tokens_after,
                "Tier 3 summarization complete"
            );

            *messages = new_messages;

            CompactionResult {
                tokens_before,
                tokens_after,
                tier_reached: CompactionTier::Summarized,
                summary: Some(result.summary),
                api_usage: Some(result.api_usage),
            }
        }
        Ok(_) => {
            tracing::warn!("Summarization returned empty result, keeping Tier 2 output");
            CompactionResult {
                tokens_before,
                tokens_after: tokens_after_mechanical,
                tier_reached: CompactionTier::Mechanical,
                summary: None,
                api_usage: None,
            }
        }
        Err(e) => {
            tracing::warn!("Summarization failed, keeping Tier 2 output: {e}");
            CompactionResult {
                tokens_before,
                tokens_after: tokens_after_mechanical,
                tier_reached: CompactionTier::Mechanical,
                summary: None,
                api_usage: None,
            }
        }
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
