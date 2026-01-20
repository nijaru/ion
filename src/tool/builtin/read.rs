use crate::tool::builtin::validate_path_within_working_dir;
use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file from the filesystem"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from"
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of lines to read"
                }
            },
            "required": ["file_path"]
        })
    }

    fn danger_level(&self) -> DangerLevel {
        DangerLevel::Safe
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let file_path_str = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("file_path is required".to_string()))?;

        let file_path = Path::new(file_path_str);

        // Validate path is within working directory (prevents path traversal)
        let validated_path = validate_path_within_working_dir(file_path, &ctx.working_dir)?;

        let content = tokio::fs::read_to_string(&validated_path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read file: {}", e)))?;

        // Lazy indexing
        if let Some(callback) = &ctx.index_callback {
            callback(validated_path.clone());
        }

        Ok(ToolResult {
            content,
            is_error: false,
            metadata: None,
        })
    }
}
