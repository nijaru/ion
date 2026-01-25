use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::json;
use std::io::{BufRead, BufReader};

/// Maximum number of matches to return.
const MAX_RESULTS: usize = 500;

/// Maximum file size to search (skip larger files).
const MAX_FILE_SIZE: u64 = 1_000_000;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search for a pattern in files (regex supported)"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "The directory or file to search in (defaults to current working directory)"
                }
            },
            "required": ["pattern"]
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
        let pattern_str = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("pattern is required".to_string()))?;

        let search_path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let regex = Regex::new(pattern_str)
            .map_err(|e| ToolError::InvalidArgs(format!("Invalid regex: {}", e)))?;

        let search_path = ctx.working_dir.join(search_path_str);
        let validated_path = ctx
            .check_sandbox(&search_path)
            .map_err(ToolError::PermissionDenied)?;
        let working_dir = ctx.working_dir.clone();

        // Use ignore crate for walking - respects .gitignore, skips hidden files and binaries
        let (results, truncated) = tokio::task::spawn_blocking(move || {
            let mut results = Vec::new();
            let mut truncated = false;

            let walker = WalkBuilder::new(&validated_path)
                .hidden(true)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build();

            'outer: for entry in walker.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                // Skip files that are too large
                if let Ok(meta) = path.metadata() {
                    if meta.len() > MAX_FILE_SIZE {
                        continue;
                    }
                }

                // Use BufReader for memory-efficient line reading
                let file = match std::fs::File::open(path) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                let reader = BufReader::new(file);
                let display_path = path.strip_prefix(&working_dir).unwrap_or(path);

                for (i, line_result) in reader.lines().enumerate() {
                    let line = match line_result {
                        Ok(l) => l,
                        Err(_) => continue, // Skip binary/invalid UTF-8 lines
                    };

                    if regex.is_match(&line) {
                        if results.len() >= MAX_RESULTS {
                            truncated = true;
                            break 'outer;
                        }
                        results.push(format!(
                            "{}:{}: {}",
                            display_path.display(),
                            i + 1,
                            line.trim()
                        ));
                    }
                }
            }
            (results, truncated)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let mut content = if results.is_empty() {
            "No matches found.".to_string()
        } else {
            results.join("\n")
        };

        if truncated {
            content.push_str(&format!("\n\n[Truncated: showing first {} matches]", MAX_RESULTS));
        }

        Ok(ToolResult {
            content,
            is_error: false,
            metadata: Some(json!({ "match_count": results.len(), "truncated": truncated })),
        })
    }
}
