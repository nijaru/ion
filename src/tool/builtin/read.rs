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

        // If offset/limit specified, use optimized line-based reading
        if offset.is_some() || limit.is_some() {
            let start = offset.unwrap_or(0);
            let count = limit.unwrap_or(DEFAULT_LIMIT);

            let path_clone = validated_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                read_lines_optimized(&path_clone, start, count)
            })
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Task join error: {}", e)))?
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read file: {}", e)))?;

            // Lazy indexing
            if let Some(callback) = &ctx.index_callback {
                callback(validated_path.clone());
            }

            let mut content = result.lines.join("\n");
            let shown_end = (start + result.lines.len()).min(result.total_lines);
            if shown_end < result.total_lines {
                content.push_str(&format!(
                    "\n\n[Showing lines {}-{} of {}. Use offset/limit for more.]",
                    start + 1,
                    shown_end,
                    result.total_lines
                ));
            }

            return Ok(ToolResult {
                content,
                is_error: false,
                metadata: Some(json!({
                    "total_lines": result.total_lines,
                    "offset": start,
                    "limit": count,
                    "shown": result.lines.len()
                })),
            });
        }

        // For full file reads, check size limit
        if file_size > MAX_FILE_SIZE {
            // Get line count quickly using SIMD
            let path_clone = validated_path.clone();
            let total_lines = tokio::task::spawn_blocking(move || count_lines_fast(&path_clone))
                .await
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                .unwrap_or(0);

            return Ok(ToolResult {
                content: format!(
                    "File is too large ({} bytes, {} lines). Use offset and limit to read specific line ranges.",
                    file_size, total_lines
                ),
                is_error: true,
                metadata: Some(json!({
                    "file_size": file_size,
                    "total_lines": total_lines,
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

struct ReadResult {
    lines: Vec<String>,
    total_lines: usize,
}

/// Read specific lines from a file with fast line counting.
fn read_lines_optimized(path: &Path, start: usize, count: usize) -> std::io::Result<ReadResult> {
    // First, get total line count using SIMD (fast, no UTF-8 decode)
    let total_lines = count_lines_fast(path)?;

    // Then read the specific lines we need
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut lines = Vec::with_capacity(count.min(1000));
    for (i, line_result) in reader.lines().enumerate() {
        if i >= start + count {
            break; // Early exit once we have enough
        }
        if i >= start {
            lines.push(line_result?);
        }
    }

    Ok(ReadResult { lines, total_lines })
}

/// Count lines using SIMD-accelerated byte counting (no UTF-8 decode).
fn count_lines_fast(path: &Path) -> std::io::Result<usize> {
    let bytes = std::fs::read(path)?;
    let count = bytecount::count(&bytes, b'\n');
    // Add 1 if file doesn't end with newline but has content
    if !bytes.is_empty() && !bytes.ends_with(&[b'\n']) {
        Ok(count + 1)
    } else {
        Ok(count)
    }
}
