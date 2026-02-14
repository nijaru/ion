//! Shared streaming utilities for LLM providers.

use crate::provider::types::{StreamEvent, ToolBuilder};
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Accumulates streamed tool call deltas across multiple content blocks.
///
/// Anthropic uses `insert` (id/name known upfront on ContentBlockStart) + `remove`
/// (on ContentBlockStop). OpenAI-compat uses `get_or_insert` (incremental deltas)
/// + `drain` (on finish_reason or stream end).
pub struct ToolCallAccumulator {
    builders: HashMap<usize, ToolBuilder>,
}

impl ToolCallAccumulator {
    pub fn new() -> Self {
        Self {
            builders: HashMap::new(),
        }
    }

    /// Get an existing builder or insert a default one at the given index.
    pub fn get_or_insert(&mut self, index: usize) -> &mut ToolBuilder {
        self.builders.entry(index).or_default()
    }

    /// Insert a builder at the given index.
    pub fn insert(&mut self, index: usize, builder: ToolBuilder) {
        self.builders.insert(index, builder);
    }

    /// Remove and return the builder at the given index.
    pub fn remove(&mut self, index: usize) -> Option<ToolBuilder> {
        self.builders.remove(&index)
    }

    /// Drain all accumulated builders, emitting completed tool calls.
    pub async fn drain_all(&mut self, tx: &mpsc::Sender<StreamEvent>) {
        for (idx, builder) in self.builders.drain() {
            if let Some(call) = builder.finish() {
                tracing::debug!(index = idx, id = %call.id, name = %call.name, "Emitting tool call");
                let _ = tx.send(StreamEvent::ToolCall(call)).await;
            }
        }
    }

    /// Drain all remaining builders, emitting completed tool calls.
    pub async fn drain_remaining(self, tx: &mpsc::Sender<StreamEvent>) {
        for (_, builder) in self.builders {
            if let Some(call) = builder.finish() {
                let _ = tx.send(StreamEvent::ToolCall(call)).await;
            }
        }
    }
}
