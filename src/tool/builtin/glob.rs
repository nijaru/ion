use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern (e.g., 'src/**/*.rs')"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "The glob pattern to search for"
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
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("pattern is required".to_string()))?;

        // Construct absolute pattern from working_dir (no global state mutation)
        let full_pattern = ctx.working_dir.join(pattern);
        let full_pattern_str = full_pattern
            .to_str()
            .ok_or_else(|| ToolError::ExecutionFailed("Invalid UTF-8 in path".to_string()))?;

        let paths: Vec<String> = glob::glob(full_pattern_str)
            .map_err(|e| ToolError::ExecutionFailed(format!("Invalid glob pattern: {}", e)))?
            .filter_map(Result::ok)
            .map(|p| {
                // Return paths relative to working_dir
                p.strip_prefix(&ctx.working_dir)
                    .unwrap_or(&p)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        Ok(ToolResult {
            content: if paths.is_empty() {
                "No files found matching the pattern.".to_string()
            } else {
                paths.join("\n")
            },
            is_error: false,
            metadata: Some(json!({ "count": paths.len() })),
        })
    }
}
