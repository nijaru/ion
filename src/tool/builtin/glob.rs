use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use globset::Glob;
use ignore::WalkBuilder;
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

        // Compile glob pattern
        let glob = Glob::new(pattern)
            .map_err(|e| ToolError::InvalidArgs(format!("Invalid glob pattern: {}", e)))?;
        let matcher = glob.compile_matcher();

        let working_dir = ctx.working_dir.clone();

        // Use ignore crate for walking, globset for matching
        let paths = tokio::task::spawn_blocking(move || {
            let walker = WalkBuilder::new(&working_dir)
                .hidden(true)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .build();

            let mut paths = Vec::new();
            for entry in walker.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }

                // Match against relative path
                if let Ok(rel_path) = path.strip_prefix(&working_dir) {
                    if matcher.is_match(rel_path) {
                        paths.push(rel_path.to_string_lossy().into_owned());
                    }
                }
            }
            paths
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

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
