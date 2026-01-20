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

#[derive(Clone)]
pub struct ToolContext {
    pub working_dir: PathBuf,
    pub session_id: String,
    pub abort_signal: CancellationToken,
    /// Callback to index a file lazily
    pub index_callback: Option<Arc<dyn Fn(PathBuf) + Send + Sync>>,
    /// Callback for semantic discovery/search
    pub discovery_callback: Option<
        Arc<
            dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<Vec<(String, f32)>>> + Send>>
                + Send
                + Sync,
        >,
    >,
}

impl fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolContext")
            .field("working_dir", &self.working_dir)
            .field("session_id", &self.session_id)
            .field("abort_signal", &self.abort_signal)
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
