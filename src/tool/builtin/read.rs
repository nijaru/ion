use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// Maximum file size to read in bytes (1MB).
const MAX_FILE_SIZE: u64 = 1_000_000;

/// Default number of lines when using offset/limit.
const DEFAULT_LIMIT: usize = 500;

/// Buffer size for streaming line count (64KB).
const COUNT_BUFFER_SIZE: usize = 64 * 1024;

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn description(&self) -> &'static str {
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

        #[allow(clippy::cast_possible_truncation)] // JSON u64 values are user-provided sizes
        let offset = args
            .get("offset")
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as usize);

        #[allow(clippy::cast_possible_truncation)]
        let limit = args
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map(|v| v as usize);

        let file_path = Path::new(file_path_str);

        // Check sandbox restrictions
        let validated_path = ctx
            .check_sandbox(file_path)
            .map_err(ToolError::PermissionDenied)?;

        // Check file size first
        let metadata = tokio::fs::metadata(&validated_path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read file: {e}")))?;

        let file_size = metadata.len();

        // If offset/limit specified, use optimized line-based reading
        if offset.is_some() || limit.is_some() {
            let start = offset.unwrap_or(0);
            let count = limit.unwrap_or(DEFAULT_LIMIT);

            let path_clone = validated_path.clone();
            let result = tokio::task::spawn_blocking(move || {
                read_lines_single_pass(&path_clone, start, count)
            })
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Task join error: {e}")))?
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read file: {e}")))?;

            // Lazy indexing
            if let Some(callback) = &ctx.index_callback {
                callback(validated_path.clone());
            }

            let mut content = result.lines.join("\n");
            let shown_end = (start + result.lines.len()).min(result.total_lines);
            if shown_end < result.total_lines {
                let _ = write!(
                    content,
                    "\n\n[Showing lines {}-{} of {}. Use offset/limit for more.]",
                    start + 1,
                    shown_end,
                    result.total_lines
                );
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
            // Get line count using streaming (constant memory)
            let path_clone = validated_path.clone();
            let total_lines =
                tokio::task::spawn_blocking(move || count_lines_streaming(&path_clone))
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .unwrap_or(0);

            return Ok(ToolResult {
                content: format!(
                    "File is too large ({file_size} bytes, {total_lines} lines). Use offset and limit to read specific line ranges."
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
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read file: {e}")))?;

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

/// Read specific lines in a single pass, counting total lines as we go.
fn read_lines_single_pass(path: &Path, start: usize, count: usize) -> std::io::Result<ReadResult> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut lines = Vec::with_capacity(count.min(1000));
    let mut total_lines = 0;

    for (i, line_result) in reader.lines().enumerate() {
        total_lines = i + 1;
        if i >= start && lines.len() < count {
            lines.push(line_result?);
        } else if i >= start + count {
            // After collecting needed lines, just count remaining (skip UTF-8 decode)
            let line = line_result?;
            drop(line);
        }
    }

    Ok(ReadResult { lines, total_lines })
}

/// Count lines using streaming with SIMD (constant memory, handles huge files).
fn count_lines_streaming(path: &Path) -> std::io::Result<usize> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut count = 0;
    let mut buf = [0u8; COUNT_BUFFER_SIZE];
    let mut last_byte = b'\n';

    loop {
        let bytes_read = reader.read(&mut buf)?;
        if bytes_read == 0 {
            break;
        }
        count += bytecount::count(&buf[..bytes_read], b'\n');
        last_byte = buf[bytes_read - 1];
    }

    // Add 1 if file doesn't end with newline but has content
    if last_byte != b'\n' {
        count += 1;
    }

    Ok(count)
}
