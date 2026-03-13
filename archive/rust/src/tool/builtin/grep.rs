use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use grep_regex::RegexMatcher;
use grep_searcher::Searcher;
use grep_searcher::sinks::UTF8;
use ignore::WalkBuilder;
use serde_json::json;
use std::fmt::Write as _;

/// Maximum number of matches to return.
const MAX_RESULTS: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Content,
    Files,
    Count,
}

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents for a text pattern (regex supported). Use this to find where code, strings, or patterns appear inside files. For finding files by name, use glob instead."
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
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files", "count"],
                    "description": "Output: content (matching lines, default), files (file paths only), count (match count per file)"
                },
                "context_before": {
                    "type": "integer",
                    "description": "Lines to show before each match (like grep -B)"
                },
                "context_after": {
                    "type": "integer",
                    "description": "Lines to show after each match (like grep -A)"
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

        let output_mode = match args.get("output_mode").and_then(|v| v.as_str()) {
            Some("files") => OutputMode::Files,
            Some("count") => OutputMode::Count,
            _ => OutputMode::Content,
        };

        let context_before = args
            .get("context_before")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        let context_after = args
            .get("context_after")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;

        let matcher = RegexMatcher::new(pattern_str)
            .map_err(|e| ToolError::InvalidArgs(format!("Invalid regex: {e}")))?;

        let search_path = ctx.working_dir.join(search_path_str);
        let validated_path = ctx
            .check_sandbox(&search_path)
            .map_err(ToolError::PermissionDenied)?;
        let working_dir = ctx.working_dir.clone();

        let (results, truncated) = tokio::task::spawn_blocking(move || {
            search_with_grep(
                &matcher,
                &validated_path,
                &working_dir,
                output_mode,
                context_before,
                context_after,
            )
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let result_count = results.len();

        let mut content = if results.is_empty() {
            "No matches found.".to_string()
        } else {
            results.join("\n")
        };

        if truncated {
            let unit = match output_mode {
                OutputMode::Files => "files",
                OutputMode::Count => "files",
                OutputMode::Content => "matches",
            };
            let _ = write!(
                content,
                "\n\n[Truncated: showing first {MAX_RESULTS} {unit}]"
            );
        }

        let unit = match output_mode {
            OutputMode::Files | OutputMode::Count => "files",
            OutputMode::Content => "matches",
        };

        Ok(ToolResult {
            content,
            is_error: false,
            metadata: Some(json!({
                "match_count": result_count,
                "truncated": truncated,
                "unit": unit,
            })),
        })
    }
}

/// Search using grep-searcher (ripgrep's library).
fn search_with_grep(
    matcher: &RegexMatcher,
    search_path: &std::path::Path,
    working_dir: &std::path::Path,
    output_mode: OutputMode,
    context_before: usize,
    context_after: usize,
) -> (Vec<String>, bool) {
    let mut results = Vec::new();
    let mut result_count: usize = 0;
    let mut truncated = false;

    // follow_links=false prevents symlink escape from sandbox
    let walker = WalkBuilder::new(search_path)
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false)
        .build();

    let use_context = context_before > 0 || context_after > 0;

    let mut searcher = if use_context {
        grep_searcher::SearcherBuilder::new()
            .before_context(context_before)
            .after_context(context_after)
            .line_number(true)
            .build()
    } else {
        Searcher::new()
    };

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        if result_count >= MAX_RESULTS {
            truncated = true;
            break;
        }

        let display_path = path.strip_prefix(working_dir).unwrap_or(path);
        let display_path_str = display_path.display().to_string();

        match output_mode {
            OutputMode::Files => {
                search_file_mode(
                    &mut searcher,
                    matcher,
                    path,
                    &display_path_str,
                    &mut results,
                    &mut result_count,
                    &mut truncated,
                );
            }
            OutputMode::Count => {
                search_count_mode(
                    &mut searcher,
                    matcher,
                    path,
                    &display_path_str,
                    &mut results,
                    &mut result_count,
                    &mut truncated,
                );
            }
            OutputMode::Content if use_context => {
                search_content_with_context(
                    &mut searcher,
                    matcher,
                    path,
                    &display_path_str,
                    &mut results,
                    &mut result_count,
                    &mut truncated,
                );
            }
            OutputMode::Content => {
                search_content_mode(
                    &mut searcher,
                    matcher,
                    path,
                    &display_path_str,
                    &mut results,
                    &mut result_count,
                    &mut truncated,
                );
            }
        }
    }

    (results, truncated)
}

/// Content mode: return matching lines with file:line: prefix (default, fast path).
fn search_content_mode(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &std::path::Path,
    display_path: &str,
    results: &mut Vec<String>,
    result_count: &mut usize,
    truncated: &mut bool,
) {
    let mut file_matches = Vec::new();
    let count = *result_count;
    let mut local_count = count;
    let mut local_truncated = false;

    let _ = searcher.search_path(
        matcher,
        path,
        UTF8(|line_num, line| {
            if local_count >= MAX_RESULTS {
                local_truncated = true;
                return Ok(false);
            }
            file_matches.push(format!("{}:{}: {}", display_path, line_num, line.trim()));
            local_count += 1;
            Ok(true)
        }),
    );

    *result_count = local_count;
    if local_truncated {
        *truncated = true;
    }
    results.extend(file_matches);
}

/// Files mode: return only file paths that contain matches.
fn search_file_mode(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &std::path::Path,
    display_path: &str,
    results: &mut Vec<String>,
    result_count: &mut usize,
    truncated: &mut bool,
) {
    let mut has_match = false;

    let _ = searcher.search_path(
        matcher,
        path,
        UTF8(|_line_num, _line| {
            has_match = true;
            Ok(false) // Stop after first match
        }),
    );

    if has_match {
        if *result_count >= MAX_RESULTS {
            *truncated = true;
            return;
        }
        *result_count += 1;
        results.push(display_path.to_string());
    }
}

/// Count mode: return per-file match counts.
fn search_count_mode(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &std::path::Path,
    display_path: &str,
    results: &mut Vec<String>,
    result_count: &mut usize,
    truncated: &mut bool,
) {
    let mut count = 0usize;

    let _ = searcher.search_path(
        matcher,
        path,
        UTF8(|_line_num, _line| {
            count += 1;
            Ok(true)
        }),
    );

    if count > 0 {
        if *result_count >= MAX_RESULTS {
            *truncated = true;
            return;
        }
        *result_count += 1;
        results.push(format!("{display_path}: {count}"));
    }
}

/// Content mode with context lines: uses custom Sink for before/after context.
fn search_content_with_context(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &std::path::Path,
    display_path: &str,
    results: &mut Vec<String>,
    result_count: &mut usize,
    truncated: &mut bool,
) {
    let mut file_matches = Vec::new();
    let mut need_separator = false;
    let mut local_count = *result_count;
    let mut local_truncated = false;

    let mut sink = ContextSink {
        file_path: display_path,
        matches: &mut file_matches,
        count: &mut local_count,
        max: MAX_RESULTS,
        truncated: &mut local_truncated,
        need_separator: &mut need_separator,
    };

    let _ = searcher.search_path(matcher, path, &mut sink);

    *result_count = local_count;
    if local_truncated {
        *truncated = true;
    }
    results.extend(file_matches);
}

/// Custom Sink that handles context lines (before/after).
struct ContextSink<'a> {
    file_path: &'a str,
    matches: &'a mut Vec<String>,
    count: &'a mut usize,
    max: usize,
    truncated: &'a mut bool,
    need_separator: &'a mut bool,
}

impl grep_searcher::Sink for ContextSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        mat: &grep_searcher::SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        if *self.count >= self.max {
            *self.truncated = true;
            return Ok(false);
        }
        if *self.need_separator {
            self.matches.push("--".to_string());
            *self.need_separator = false;
        }
        let line = String::from_utf8_lossy(mat.bytes());
        if let Some(n) = mat.line_number() {
            self.matches
                .push(format!("{}:{}: {}", self.file_path, n, line.trim()));
        }
        *self.count += 1;
        Ok(true)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &grep_searcher::SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        if *self.need_separator {
            self.matches.push("--".to_string());
            *self.need_separator = false;
        }
        let line = String::from_utf8_lossy(ctx.bytes());
        if let Some(n) = ctx.line_number() {
            self.matches
                .push(format!("{}:{}: {}", self.file_path, n, line.trim()));
        }
        Ok(true)
    }

    fn context_break(&mut self, _searcher: &Searcher) -> Result<bool, Self::Error> {
        *self.need_separator = true;
        Ok(true)
    }
}
