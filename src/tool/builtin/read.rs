use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Maximum file size to read in bytes (1MB).
const MAX_FILE_SIZE: u64 = 1_000_000;

/// Default number of lines when using offset/limit.
const DEFAULT_LIMIT: usize = 500;

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a file from the filesystem. For large files, use offset and limit to read specific line ranges."
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
                    "description": "Line number to start reading from (0-indexed)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (default: 500)"
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

        let offset = args
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let file_path = Path::new(file_path_str);

        // Check sandbox restrictions
        let validated_path = ctx
            .check_sandbox(file_path)
            .map_err(ToolError::PermissionDenied)?;

        // Check file size first
        let metadata = tokio::fs::metadata(&validated_path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read file: {}", e)))?;

        let file_size = metadata.len();

        // If offset/limit specified, use streaming line-based reading
        if offset.is_some() || limit.is_some() {
            let start = offset.unwrap_or(0);
            let count = limit.unwrap_or(DEFAULT_LIMIT);

            let path_clone = validated_path.clone();
            let (lines, total_lines) = tokio::task::spawn_blocking(move || {
                let file = std::fs::File::open(&path_clone)?;
                let reader = BufReader::new(file);

                let mut lines = Vec::with_capacity(count.min(1000));
                let mut total = 0usize;
                let mut current = 0usize;

                for line_result in reader.lines() {
                    let line = line_result?;
                    if current >= start && lines.len() < count {
                        lines.push(line);
                    }
                    current += 1;
                    total += 1;
                }

                Ok::<_, std::io::Error>((lines, total))
            })
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Task join error: {}", e)))?
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read file: {}", e)))?;

            let shown_end = (start + lines.len()).min(total_lines);

            // Lazy indexing
            if let Some(callback) = &ctx.index_callback {
                callback(validated_path.clone());
            }

            let mut result = lines.join("\n");
            if shown_end < total_lines {
                result.push_str(&format!(
                    "\n\n[Showing lines {}-{} of {}. Use offset/limit for more.]",
                    start + 1,
                    shown_end,
                    total_lines
                ));
            }

            return Ok(ToolResult {
                content: result,
                is_error: false,
                metadata: Some(json!({
                    "total_lines": total_lines,
                    "offset": start,
                    "limit": count,
                    "shown": lines.len()
                })),
            });
        }

        // For full file reads, check size limit
        if file_size > MAX_FILE_SIZE {
            return Ok(ToolResult {
                content: format!(
                    "File is too large ({} bytes, max {} bytes). Use offset and limit to read specific line ranges.",
                    file_size, MAX_FILE_SIZE
                ),
                is_error: true,
                metadata: Some(json!({
                    "file_size": file_size,
                    "max_size": MAX_FILE_SIZE
                })),
            });
        }

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
            metadata: Some(json!({ "file_size": file_size })),
        })
    }
}
