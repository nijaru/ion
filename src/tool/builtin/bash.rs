use super::guard::{CommandRisk, analyze_command};
use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;

/// Maximum output size in bytes (100KB).
const MAX_OUTPUT_SIZE: usize = 100_000;

pub struct BashTool;

impl BashTool {
    /// Check if command is dangerous and return metadata about the risk.
    fn check_danger(&self, command: &str) -> Option<CommandRisk> {
        let risk = analyze_command(command);
        if risk.is_dangerous() {
            Some(risk)
        } else {
            None
        }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &'static str {
        "bash"
    }

    fn description(&self) -> &'static str {
        "Execute a shell command. Use for git, build tools, package managers, and system operations. Prefer specialized tools (glob, grep, read, edit) for file operations."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
                },
                "directory": {
                    "type": "string",
                    "description": "Working directory for this command (default: project root)"
                }
            },
            "required": ["command"]
        })
    }

    fn danger_level(&self) -> DangerLevel {
        DangerLevel::Restricted
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let command_str = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("command is required".to_string()))?;

        // Resolve working directory
        let dir = args
            .get("directory")
            .and_then(|v| v.as_str())
            .map(|d| {
                let p = std::path::Path::new(d);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    ctx.working_dir.join(d)
                }
            })
            .unwrap_or_else(|| ctx.working_dir.clone());
        let dir = ctx
            .check_sandbox(&dir)
            .map_err(ToolError::PermissionDenied)?;

        // Check for destructive command patterns
        if let Some(risk) = self.check_danger(command_str)
            && let Some(reason) = risk.reason()
        {
            return Ok(ToolResult {
                content: format!(
                    "BLOCKED: Destructive command detected.\n\n\
                    Reason: {reason}\n\n\
                    If you need to run this command, explain why it's safe \
                    and ask the user to run it manually."
                ),
                is_error: true,
                metadata: Some(json!({
                    "blocked": true,
                    "reason": reason,
                    "command": command_str,
                })),
            });
        }

        // Spawn child process with kill_on_drop for cancellation safety
        // Set environment variables to force color output in non-TTY context
        let child = Command::new("bash")
            .arg("-c")
            .arg(command_str)
            .current_dir(&dir)
            .env("CLICOLOR_FORCE", "1")
            .env("FORCE_COLOR", "1")
            .env("TERM", "xterm-256color")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to spawn command: {e}")))?;

        // Wait for completion or user cancellation (no timeout - user can Ctrl+C)
        let output = tokio::select! {
            res = child.wait_with_output() => {
                match res {
                    Ok(out) => out,
                    Err(e) => return Err(ToolError::ExecutionFailed(format!("Failed to read command output: {e}"))),
                }
            }
            () = ctx.abort_signal.cancelled() => {
                return Err(ToolError::Cancelled);
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);

        // Build raw output (stdout + stderr)
        let mut raw_output = stdout;
        if !stderr.is_empty() {
            if !raw_output.is_empty() {
                raw_output.push('\n');
            }
            raw_output.push_str("STDERR:\n");
            raw_output.push_str(&stderr);
        }

        // Truncate large output to prevent context overflow
        let truncated = raw_output.len() > MAX_OUTPUT_SIZE;
        if truncated {
            let truncate_at = raw_output
                .char_indices()
                .take_while(|(i, _)| *i < MAX_OUTPUT_SIZE)
                .last()
                .map_or(MAX_OUTPUT_SIZE, |(i, c)| i + c.len_utf8());
            raw_output.truncate(truncate_at);
            raw_output.push_str("\n\n[Output truncated]");
        }

        // Structured output with metadata for model signal
        let total_lines = raw_output.lines().count();
        let content = format!("Exit code: {exit_code}\nOutput lines: {total_lines}\n\n{raw_output}");

        Ok(ToolResult {
            content,
            is_error: !output.status.success(),
            metadata: Some(json!({
                "exit_code": exit_code,
                "truncated": truncated,
            })),
        })
    }
}
