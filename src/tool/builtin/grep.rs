use crate::tool::builtin::validate_path_within_working_dir;
use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use regex::Regex;
use serde_json::json;
use std::path::Path;

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

        // Validate path is within working directory (prevents path traversal via ../)
        let validated_path = validate_path_within_working_dir(&search_path, &ctx.working_dir)?;

        let mut results = Vec::new();
        self.search_recursive(&validated_path, &regex, &mut results, &ctx.working_dir)
            .await?;

        Ok(ToolResult {
            content: if results.is_empty() {
                "No matches found.".to_string()
            } else {
                results.join("\n")
            },
            is_error: false,
            metadata: Some(json!({ "match_count": results.len() })),
        })
    }
}

impl GrepTool {
    #[async_recursion::async_recursion]
    async fn search_recursive(
        &self,
        path: &Path,
        regex: &Regex,
        results: &mut Vec<String>,
        working_dir: &Path,
    ) -> Result<(), ToolError> {
        if path.is_file() {
            let content = tokio::fs::read_to_string(path).await.ok();
            if let Some(content) = content {
                // Show paths relative to working_dir for cleaner output
                let display_path = path.strip_prefix(working_dir).unwrap_or(path);
                for (i, line) in content.lines().enumerate() {
                    if regex.is_match(line) {
                        results.push(format!(
                            "{}:{}: {}",
                            display_path.display(),
                            i + 1,
                            line.trim()
                        ));
                    }
                }
            }
        } else if path.is_dir() {
            let mut entries = tokio::fs::read_dir(path)
                .await
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

            while let Some(entry) = entries.next_entry().await.ok().flatten() {
                let entry_path = entry.path();
                // Skip hidden directories and common noise
                if let Some(name) = entry_path.file_name().and_then(|n| n.to_str()) {
                    let is_ignored =
                        name.starts_with('.') || name == "target" || name == "node_modules";
                    if is_ignored {
                        continue;
                    }
                }
                self.search_recursive(&entry_path, regex, results, working_dir)
                    .await?;
            }
        }
        Ok(())
    }
}
