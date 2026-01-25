use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::path::Path;

/// Maximum old file size to read for diffing (1MB).
const MAX_DIFF_SOURCE_SIZE: u64 = 1_000_000;

/// Maximum diff output size (50KB).
const MAX_DIFF_SIZE: usize = 50_000;

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
        let validated_path = ctx
            .check_sandbox(file_path)
            .map_err(ToolError::PermissionDenied)?;

        // Read old content if exists for diffing (skip if too large)
        let old_content = if validated_path.exists() {
            let metadata = tokio::fs::metadata(&validated_path).await.ok();
            if metadata.is_some_and(|m| m.len() <= MAX_DIFF_SOURCE_SIZE) {
                tokio::fs::read_to_string(&validated_path).await.ok()
            } else {
                None // Skip diff for large files
            }
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

        let line_count = content.lines().count();
        let result_msg = if let Some(old) = old_content {
            let diff = similar::TextDiff::from_lines(old.as_str(), content);
            let mut diff_output = String::new();
            use std::fmt::Write;
            for change in diff
                .unified_diff()
                .header(file_path_str, file_path_str)
                .iter_hunks()
            {
                let _ = write!(diff_output, "{}", change);
            }

            if diff_output.is_empty() {
                format!("Wrote {} (no changes)", file_path_str)
            } else {
                // Truncate large diffs at char boundary
                if diff_output.len() > MAX_DIFF_SIZE {
                    let truncate_at = diff_output
                        .char_indices()
                        .take_while(|(i, _)| *i < MAX_DIFF_SIZE)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(MAX_DIFF_SIZE);
                    diff_output.truncate(truncate_at);
                    diff_output.push_str("\n\n[Diff truncated]");
                }
                format!("Wrote {}:\n{}", file_path_str, diff_output)
            }
        } else {
            // New file: just show line count, not full content
            format!("Created {} ({} lines)", file_path_str, line_count)
        };

        Ok(ToolResult {
            content: result_msg,
            is_error: false,
            metadata: None,
        })
    }
}
