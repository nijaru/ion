use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use globset::Glob;
use ignore::WalkBuilder;
use serde_json::json;
use std::sync::Mutex;

/// Maximum number of results to return.
const MAX_RESULTS: usize = 1000;

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

        // Use parallel walker for better performance on large directories
        let (paths, truncated) = tokio::task::spawn_blocking(move || {
            let paths = Mutex::new(Vec::new());
            let truncated = Mutex::new(false);

            // Build parallel walker (follow_links=false prevents symlink escape)
            let walker = WalkBuilder::new(&working_dir)
                .hidden(true)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .follow_links(false)
                .build_parallel();

            walker.run(|| {
                let matcher = &matcher;
                let working_dir = &working_dir;
                let paths = &paths;
                let truncated = &truncated;

                Box::new(move |entry| {
                    // Check if we've hit the limit
                    if *truncated.lock().unwrap() {
                        return ignore::WalkState::Quit;
                    }

                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => return ignore::WalkState::Continue,
                    };

                    let path = entry.path();
                    if !path.is_file() {
                        return ignore::WalkState::Continue;
                    }

                    // Match against relative path
                    if let Ok(rel_path) = path.strip_prefix(working_dir)
                        && matcher.is_match(rel_path) {
                            let mut paths_guard = paths.lock().unwrap();
                            if paths_guard.len() >= MAX_RESULTS {
                                *truncated.lock().unwrap() = true;
                                return ignore::WalkState::Quit;
                            }
                            paths_guard.push(rel_path.to_string_lossy().into_owned());
                        }

                    ignore::WalkState::Continue
                })
            });

            let mut paths = paths.into_inner().unwrap();
            paths.sort(); // Sort for consistent output
            let truncated = *truncated.lock().unwrap();
            (paths, truncated)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let mut content = if paths.is_empty() {
            "No files found matching the pattern.".to_string()
        } else {
            paths.join("\n")
        };

        if truncated {
            content.push_str(&format!(
                "\n\n[Truncated: showing first {} results]",
                MAX_RESULTS
            ));
        }

        Ok(ToolResult {
            content,
            is_error: false,
            metadata: Some(json!({ "count": paths.len(), "truncated": truncated })),
        })
    }
}
