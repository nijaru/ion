//! Hook system for extensible behavior at key execution points.
//!
//! Hooks allow custom code to run at specific points in the agent lifecycle,
//! enabling features like logging, rate limiting, content filtering, etc.

use async_trait::async_trait;
use std::sync::Arc;

/// Points in the execution lifecycle where hooks can be triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookPoint {
    /// Before a tool is executed. Can modify or reject the tool call.
    PreToolUse,
    /// After a tool completes. Can inspect or modify the result.
    PostToolUse,
    /// When an error occurs. Can handle, transform, or suppress errors.
    OnError,
    /// After receiving a response from the model.
    OnResponse,
}

/// Context passed to hooks during execution.
#[derive(Debug, Clone)]
pub struct HookContext {
    /// The hook point being triggered.
    pub point: HookPoint,
    /// Tool name (for tool-related hooks).
    pub tool_name: Option<String>,
    /// Tool input (for `PreToolUse`).
    pub tool_input: Option<serde_json::Value>,
    /// Tool output (for `PostToolUse`).
    pub tool_output: Option<String>,
    /// Error message (for `OnError`).
    pub error: Option<String>,
    /// Model response text (for `OnResponse`).
    pub response: Option<String>,
}

impl HookContext {
    /// Create a new hook context for a given hook point.
    #[must_use]
    pub fn new(point: HookPoint) -> Self {
        Self {
            point,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            error: None,
            response: None,
        }
    }

    /// Set the tool name.
    #[must_use]
    pub fn with_tool_name(mut self, name: impl Into<String>) -> Self {
        self.tool_name = Some(name.into());
        self
    }

    /// Set the tool input.
    #[must_use]
    pub fn with_tool_input(mut self, input: serde_json::Value) -> Self {
        self.tool_input = Some(input);
        self
    }

    /// Set the tool output.
    #[must_use]
    pub fn with_tool_output(mut self, output: impl Into<String>) -> Self {
        self.tool_output = Some(output.into());
        self
    }

    /// Set the error message.
    #[must_use]
    pub fn with_error(mut self, error: impl Into<String>) -> Self {
        self.error = Some(error.into());
        self
    }

    /// Set the response text.
    #[must_use]
    pub fn with_response(mut self, response: impl Into<String>) -> Self {
        self.response = Some(response.into());
        self
    }
}

/// Result of a hook execution.
#[derive(Debug, Clone, Default)]
pub enum HookResult {
    /// Continue normal execution.
    #[default]
    Continue,
    /// Skip the current operation (e.g., skip tool execution).
    Skip,
    /// Replace the tool input with new input (`PreToolUse` only).
    ReplaceInput(serde_json::Value),
    /// Replace the tool output with new output (`PostToolUse` only).
    ReplaceOutput(String),
    /// Abort with an error message.
    Abort(String),
}

/// Trait for implementing hooks.
#[async_trait]
pub trait Hook: Send + Sync {
    /// The hook point this hook responds to.
    fn hook_point(&self) -> HookPoint;

    /// Execute the hook with the given context.
    async fn execute(&self, ctx: &HookContext) -> HookResult;

    /// Optional name for debugging/logging.
    fn name(&self) -> &'static str {
        "unnamed_hook"
    }

    /// Priority for ordering (lower = runs first). Default is 100.
    fn priority(&self) -> u32 {
        100
    }
}

/// Registry for managing hooks.
#[derive(Default)]
pub struct HookRegistry {
    hooks: Vec<Arc<dyn Hook>>,
}

impl HookRegistry {
    /// Create a new empty hook registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a hook.
    pub fn register(&mut self, hook: Arc<dyn Hook>) {
        self.hooks.push(hook);
        // Sort by priority (stable sort preserves registration order for equal priorities)
        self.hooks.sort_by_key(|h| h.priority());
    }

    /// Execute all hooks for a given hook point.
    /// Returns the final result after all hooks have run.
    pub async fn execute(&self, ctx: &HookContext) -> HookResult {
        let mut result = HookResult::Continue;

        for hook in &self.hooks {
            if hook.hook_point() != ctx.point {
                continue;
            }

            match hook.execute(ctx).await {
                HookResult::Continue => {}
                HookResult::Skip => {
                    result = HookResult::Skip;
                    break;
                }
                HookResult::Abort(msg) => {
                    result = HookResult::Abort(msg);
                    break;
                }
                other => {
                    result = other;
                }
            }
        }

        result
    }

    /// Get the number of registered hooks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    /// Check if the registry is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHook {
        point: HookPoint,
        result: HookResult,
    }

    #[async_trait]
    impl Hook for TestHook {
        fn hook_point(&self) -> HookPoint {
            self.point
        }

        async fn execute(&self, _ctx: &HookContext) -> HookResult {
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn test_hook_registry_executes_matching_hooks() {
        let mut registry = HookRegistry::new();
        registry.register(Arc::new(TestHook {
            point: HookPoint::PreToolUse,
            result: HookResult::Continue,
        }));

        let ctx = HookContext::new(HookPoint::PreToolUse);
        let result = registry.execute(&ctx).await;
        assert!(matches!(result, HookResult::Continue));
    }

    #[tokio::test]
    async fn test_hook_registry_skips_non_matching_hooks() {
        let mut registry = HookRegistry::new();
        registry.register(Arc::new(TestHook {
            point: HookPoint::PostToolUse,
            result: HookResult::Skip,
        }));

        let ctx = HookContext::new(HookPoint::PreToolUse);
        let result = registry.execute(&ctx).await;
        assert!(matches!(result, HookResult::Continue)); // PostToolUse hook not triggered
    }

    #[tokio::test]
    async fn test_hook_abort_stops_execution() {
        let mut registry = HookRegistry::new();
        registry.register(Arc::new(TestHook {
            point: HookPoint::PreToolUse,
            result: HookResult::Abort("stopped".to_string()),
        }));
        registry.register(Arc::new(TestHook {
            point: HookPoint::PreToolUse,
            result: HookResult::Continue,
        }));

        let ctx = HookContext::new(HookPoint::PreToolUse);
        let result = registry.execute(&ctx).await;
        assert!(matches!(result, HookResult::Abort(msg) if msg == "stopped"));
    }
}
