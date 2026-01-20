use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command to execute"
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

        // Spawn child process with kill_on_drop for cancellation safety
        let child = Command::new("bash")
            .arg("-c")
            .arg(command_str)
            .current_dir(&ctx.working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to spawn command: {}", e)))?;

        // Wait for completion or user cancellation
        let output = tokio::select! {
            res = child.wait_with_output() => {
                match res {
                    Ok(out) => out,
                    Err(e) => return Err(ToolError::ExecutionFailed(format!("Failed to read command output: {}", e))),
                }
            }
            _ = ctx.abort_signal.cancelled() => {
                return Err(ToolError::Cancelled);
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        let mut content = stdout;
        if !stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str("STDERR:\n");
            content.push_str(&stderr);
        }

        Ok(ToolResult {
            content,
            is_error: !output.status.success(),
            metadata: Some(json!({
                "exit_code": output.status.code(),
            })),
        })
    }
}
