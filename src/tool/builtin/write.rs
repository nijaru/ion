use crate::tool::builtin::validate_path_within_working_dir;
use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write content to a file. Overwrites existing content."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The absolute path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["file_path", "content"]
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
        let file_path_str = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("file_path is required".to_string()))?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("content is required".to_string()))?;

        let file_path = Path::new(file_path_str);
        let validated_path = validate_path_within_working_dir(file_path, &ctx.working_dir)?;

        // Read old content if exists for diffing
        let old_content = if validated_path.exists() {
            tokio::fs::read_to_string(&validated_path).await.ok()
        } else {
            None
        };

        // Ensure parent directory exists
        if let Some(parent) = validated_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to create directories: {}", e))
            })?;
        }

        tokio::fs::write(&validated_path, content)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write file: {}", e)))?;

        // Lazy indexing
        if let Some(callback) = &ctx.index_callback {
            callback(validated_path.clone());
        }

        let result_msg = if let Some(old) = old_content {
            let diff = similar::TextDiff::from_lines(old.as_str(), content);
            let mut diff_output = String::new();
            for change in diff
                .unified_diff()
                .header(file_path_str, file_path_str)
                .iter_hunks()
            {
                diff_output.push_str(&format!("{}", change));
            }

            if diff_output.is_empty() {
                format!("Successfully wrote to {} (no changes)", file_path_str)
            } else {
                format!(
                    "Successfully wrote to {}:\n\n```diff\n{}```",
                    file_path_str, diff_output
                )
            }
        } else {
            format!(
                "Successfully created new file {}:\n\n```\n{}```",
                file_path_str, content
            )
        };

        Ok(ToolResult {
            content: result_msg,
            is_error: false,
            metadata: None,
        })
    }
}
