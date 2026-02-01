use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Callback type for semantic discovery/search operations.
pub type DiscoveryCallback = Arc<
    dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<Vec<(String, f32)>>> + Send>>
        + Send
        + Sync,
>;

#[derive(Clone)]
pub struct ToolContext {
    pub working_dir: PathBuf,
    pub session_id: String,
    pub abort_signal: CancellationToken,
    /// Allow operations outside CWD (sandbox disabled)
    pub no_sandbox: bool,
    /// Callback to index a file lazily
    pub index_callback: Option<Arc<dyn Fn(PathBuf) + Send + Sync>>,
    /// Callback for semantic discovery/search
    pub discovery_callback: Option<DiscoveryCallback>,
}

impl ToolContext {
    /// Check if a path is within the sandbox (CWD).
    /// Returns `Ok(canonical_path)` if allowed, Err with message if blocked.
    pub fn check_sandbox(&self, path: &std::path::Path) -> Result<PathBuf, String> {
        // If sandbox disabled, allow anything
        if self.no_sandbox {
            return Ok(path.to_path_buf());
        }

        // Resolve the path (handle relative paths)
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.working_dir.join(path)
        };

        // Canonicalize both paths for comparison
        let canonical = resolved
            .canonicalize()
            .or_else(|_| {
                // Path might not exist yet (for writes), check parent
                if let Some(parent) = resolved.parent() {
                    parent
                        .canonicalize()
                        .map(|p| p.join(resolved.file_name().unwrap_or_default()))
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "Invalid path",
                    ))
                }
            })
            .map_err(|e| format!("Failed to resolve path: {e}"))?;

        let cwd_canonical = self
            .working_dir
            .canonicalize()
            .map_err(|e| format!("Failed to resolve working directory: {e}"))?;

        // Check if path is within CWD
        if canonical.starts_with(&cwd_canonical) {
            Ok(canonical)
        } else {
            Err(format!(
                "Path '{}' is outside the sandbox ({}). Use --no-sandbox to allow.",
                path.display(),
                self.working_dir.display()
            ))
        }
    }
}

impl fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolContext")
            .field("working_dir", &self.working_dir)
            .field("session_id", &self.session_id)
            .field("abort_signal", &self.abort_signal)
            .field("no_sandbox", &self.no_sandbox)
            .field(
                "index_callback",
                &self.index_callback.as_ref().map(|_| "Fn(PathBuf)"),
            )
            .field(
                "discovery_callback",
                &self.discovery_callback.as_ref().map(|_| "Fn(String)"),
            )
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    pub metadata: Option<serde_json::Value>,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> serde_json::Value;

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError>;

    fn danger_level(&self) -> DangerLevel {
        DangerLevel::Restricted
    }

    fn requires_sandbox(&self) -> bool {
        false
    }
}

/// Classification of tools based on their potential impact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DangerLevel {
    /// Tool is safe to run (e.g., read, glob).
    Safe,
    /// Tool has side effects or security implications (e.g., write, bash).
    Restricted,
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),

    #[error("Execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Cancelled")]
    Cancelled,
}

/// User's response to an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResponse {
    Yes,
    No,
    AlwaysSession,
    AlwaysPermanent,
}

/// Interface for handling tool approvals. Usually implemented by the TUI.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn ask_approval(&self, tool_name: &str, args: &serde_json::Value) -> ApprovalResponse;
}

/// The active execution mode of the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ToolMode {
    /// Only safe tools (read-only) are allowed.
    Read,
    /// Standard interactive mode with prompts for restricted tools.
    #[default]
    Write,
    /// Full autonomy, no prompts (Bypass mode).
    Agi,
}

/// Source of a tool (where it came from).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSource {
    /// Built-in tool (part of the binary).
    Builtin,
    /// Tool provided by an MCP server.
    Mcp {
        /// The MCP server name or identifier.
        server: String,
    },
    /// Tool loaded from a plugin file.
    Plugin {
        /// Path to the plugin file.
        path: PathBuf,
    },
}

impl Default for ToolSource {
    fn default() -> Self {
        Self::Builtin
    }
}

/// Capabilities that a tool might have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCapability {
    /// Can read files or data.
    Read,
    /// Can write/modify files or data.
    Write,
    /// Can execute commands or code.
    Execute,
    /// Can access the network.
    Network,
    /// Can modify system state.
    System,
}

/// Metadata about a tool for discovery and display.
#[derive(Debug, Clone)]
pub struct ToolMetadata {
    /// Tool name (must be unique within a registry).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Where the tool came from.
    pub source: ToolSource,
    /// What the tool can do.
    pub capabilities: Vec<ToolCapability>,
    /// Whether the tool is currently enabled.
    pub enabled: bool,
}
