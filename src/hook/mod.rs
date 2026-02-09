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
    ///
    /// Hook execution follows these rules:
    /// - Hooks are executed in priority order (lower priority number = earlier)
    /// - `Continue` allows the next hook to run
    /// - `Skip` and `Abort` stop execution immediately
    /// - `ReplaceInput`/`ReplaceOutput` are kept but don't stop execution,
    ///   so later hooks can override earlier transformations (last wins)
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

/// A hook that executes a shell command.
///
/// Context is passed via environment variables:
/// - `ION_HOOK_EVENT` — "pre_tool_use" or "post_tool_use"
/// - `ION_TOOL_NAME` — tool name
/// - `ION_WORKING_DIR` — working directory
///
/// Exit 0 = Continue, non-zero = Abort(stderr).
pub struct CommandHook {
    point: HookPoint,
    command: String,
    tool_pattern: Option<regex::Regex>,
}

impl CommandHook {
    /// Create a `CommandHook` from config values.
    ///
    /// Returns `None` if the event string is invalid.
    pub fn from_config(
        event: &str,
        command: String,
        tool_pattern: Option<&str>,
    ) -> Option<Self> {
        let point = match event {
            "pre_tool_use" => HookPoint::PreToolUse,
            "post_tool_use" => HookPoint::PostToolUse,
            _ => return None,
        };
        let tool_pattern = match tool_pattern {
            Some(p) => match regex::Regex::new(p) {
                Ok(re) => Some(re),
                Err(e) => {
                    tracing::warn!("Invalid regex in hook tool_pattern '{p}': {e}");
                    return None;
                }
            },
            None => None,
        };
        Some(Self {
            point,
            command,
            tool_pattern,
        })
    }
}

#[async_trait]
impl Hook for CommandHook {
    fn hook_point(&self) -> HookPoint {
        self.point
    }

    async fn execute(&self, ctx: &HookContext) -> HookResult {
        // Check tool pattern filter
        if let Some(ref pattern) = self.tool_pattern {
            match ctx.tool_name {
                Some(ref tool_name) if pattern.is_match(tool_name) => {}
                // No match or no tool name — skip this hook
                _ => return HookResult::Continue,
            }
        }

        let event_str = match self.point {
            HookPoint::PreToolUse => "pre_tool_use",
            HookPoint::PostToolUse => "post_tool_use",
        };

        let tool_name = ctx.tool_name.as_deref().unwrap_or("");
        let working_dir =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        let child = match tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&self.command)
            .env("ION_HOOK_EVENT", event_str)
            .env("ION_TOOL_NAME", tool_name)
            .env("ION_WORKING_DIR", working_dir.to_string_lossy().as_ref())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => return HookResult::Abort(format!("Hook spawn failed: {e}")),
        };

        let result = child.wait_with_output();
        match tokio::time::timeout(std::time::Duration::from_secs(10), result).await {
            Ok(Ok(output)) => {
                if output.status.success() {
                    HookResult::Continue
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    HookResult::Abort(if stderr.is_empty() {
                        format!("Hook command failed: {}", self.command)
                    } else {
                        stderr
                    })
                }
            }
            Ok(Err(e)) => HookResult::Abort(format!("Hook I/O error: {e}")),
            Err(_) => HookResult::Abort(format!("Hook timed out: {}", self.command)),
        }
    }

    fn name(&self) -> &'static str {
        "command_hook"
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

    #[test]
    fn test_command_hook_from_config() {
        let hook = CommandHook::from_config("pre_tool_use", "echo ok".into(), None);
        assert!(hook.is_some());
        assert_eq!(hook.unwrap().hook_point(), HookPoint::PreToolUse);

        let hook = CommandHook::from_config("post_tool_use", "echo ok".into(), Some("write|edit"));
        assert!(hook.is_some());

        let hook = CommandHook::from_config("invalid_event", "echo ok".into(), None);
        assert!(hook.is_none());
    }

    #[tokio::test]
    async fn test_command_hook_success() {
        let hook = CommandHook::from_config("pre_tool_use", "true".into(), None).unwrap();
        let ctx = HookContext::new(HookPoint::PreToolUse).with_tool_name("write");
        let result = hook.execute(&ctx).await;
        assert!(matches!(result, HookResult::Continue));
    }

    #[tokio::test]
    async fn test_command_hook_failure() {
        let hook = CommandHook::from_config("pre_tool_use", "false".into(), None).unwrap();
        let ctx = HookContext::new(HookPoint::PreToolUse).with_tool_name("write");
        let result = hook.execute(&ctx).await;
        assert!(matches!(result, HookResult::Abort(_)));
    }

    #[tokio::test]
    async fn test_command_hook_tool_pattern_match() {
        let hook =
            CommandHook::from_config("pre_tool_use", "true".into(), Some("write|edit")).unwrap();
        let ctx = HookContext::new(HookPoint::PreToolUse).with_tool_name("write");
        let result = hook.execute(&ctx).await;
        assert!(matches!(result, HookResult::Continue));
    }

    #[tokio::test]
    async fn test_command_hook_tool_pattern_no_match() {
        let hook =
            CommandHook::from_config("pre_tool_use", "false".into(), Some("write|edit")).unwrap();
        // Tool name "read" doesn't match pattern, so hook should skip (Continue)
        let ctx = HookContext::new(HookPoint::PreToolUse).with_tool_name("read");
        let result = hook.execute(&ctx).await;
        assert!(matches!(result, HookResult::Continue));
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

    #[test]
    fn test_command_hook_invalid_regex_rejected() {
        // Invalid regex should cause from_config to return None
        let hook = CommandHook::from_config("pre_tool_use", "echo ok".into(), Some("write[edit"));
        assert!(hook.is_none());
    }

    #[tokio::test]
    async fn test_command_hook_pattern_skips_when_tool_name_none() {
        // Hook with tool_pattern should NOT fire when tool_name is None
        let hook =
            CommandHook::from_config("pre_tool_use", "false".into(), Some("write|edit")).unwrap();
        let ctx = HookContext::new(HookPoint::PreToolUse); // no tool_name
        let result = hook.execute(&ctx).await;
        assert!(matches!(result, HookResult::Continue));
    }

    #[tokio::test]
    async fn test_command_hook_no_pattern_fires_when_tool_name_none() {
        // Hook WITHOUT tool_pattern should still fire when tool_name is None
        let hook = CommandHook::from_config("pre_tool_use", "true".into(), None).unwrap();
        let ctx = HookContext::new(HookPoint::PreToolUse); // no tool_name
        let result = hook.execute(&ctx).await;
        assert!(matches!(result, HookResult::Continue));
    }
}
