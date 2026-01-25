use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;
use ignore::WalkBuilder;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

/// Maximum number of matches to return.
const MAX_RESULTS: usize = 500;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search for a pattern in files (regex supported). Uses ripgrep's optimized search engine."
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

        let matcher = RegexMatcher::new(pattern_str)
            .map_err(|e| ToolError::InvalidArgs(format!("Invalid regex: {}", e)))?;

        let search_path = ctx.working_dir.join(search_path_str);
        let validated_path = ctx
            .check_sandbox(&search_path)
            .map_err(ToolError::PermissionDenied)?;
        let working_dir = ctx.working_dir.clone();

        // Use grep-searcher with ignore crate for walking
        let (results, truncated) = tokio::task::spawn_blocking(move || {
            search_with_grep(&matcher, &validated_path, &working_dir)
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let mut content = if results.is_empty() {
            "No matches found.".to_string()
        } else {
            results.join("\n")
        };

        if truncated {
            content.push_str(&format!(
                "\n\n[Truncated: showing first {} matches]",
                MAX_RESULTS
            ));
        }

        Ok(ToolResult {
            content,
            is_error: false,
            metadata: Some(json!({ "match_count": results.len(), "truncated": truncated })),
        })
    }
}

/// Search using grep-searcher (ripgrep's library).
/// Batches results per-file to minimize lock contention.
fn search_with_grep(
    matcher: &RegexMatcher,
    search_path: &std::path::Path,
    working_dir: &std::path::Path,
) -> (Vec<String>, bool) {
    let results = Mutex::new(Vec::new());
    let result_count = AtomicUsize::new(0);
    let truncated = AtomicBool::new(false);

    // follow_links=false prevents symlink escape from sandbox
    let walker = WalkBuilder::new(search_path)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false)
        .build();

    let mut searcher = Searcher::new();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // Check if we've hit the limit (atomic, no lock needed)
        if result_count.load(Ordering::Relaxed) >= MAX_RESULTS {
            truncated.store(true, Ordering::Relaxed);
            break;
        }

        let display_path = path.strip_prefix(working_dir).unwrap_or(path);
        let display_path_str = display_path.display().to_string();

        // Collect matches for this file locally, then batch-add to results
        let mut file_matches = Vec::new();
        let file_truncated = &truncated;
        let file_count = &result_count;

        // Search this file, collecting results locally
        let _ = searcher.search_path(
            matcher,
            path,
            UTF8(|line_num, line| {
                // Check limit with atomic (cheaper than lock)
                if file_count.load(Ordering::Relaxed) >= MAX_RESULTS {
                    file_truncated.store(true, Ordering::Relaxed);
                    return Ok(false);
                }
                file_matches.push(format!(
                    "{}:{}: {}",
                    display_path_str,
                    line_num,
                    line.trim()
                ));
                file_count.fetch_add(1, Ordering::Relaxed);
                Ok(true)
            }),
        );
        // Note: search errors (binary files, permission issues) are intentionally
        // ignored - we continue searching other files rather than failing entirely

        // Batch-add all matches from this file (single lock acquisition)
        if !file_matches.is_empty() {
            let mut res = results.lock().unwrap();
            res.extend(file_matches);
        }
    }

    let results = results.into_inner().unwrap();
    let truncated = truncated.load(Ordering::Relaxed);
    (results, truncated)
}
