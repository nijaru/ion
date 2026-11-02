//! Native read-only repository discovery: directory listing, file globs and
//! content search. This is ordinary host filesystem access, not a sandbox.
use ion_core::ToolOutput;
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use self::output::preview;
use super::{error, present_input};

mod find;
mod glob;
mod grep;
mod ls;
mod output;
mod walk;

pub(super) const MAX_QUERY_BYTES: usize = 4096;
const DEFAULT_LS_LIMIT: usize = 500;
const MAX_LS_LIMIT: usize = 1000;
const DEFAULT_FIND_LIMIT: usize = 1000;
const MAX_FIND_LIMIT: usize = 1000;
const DEFAULT_GREP_LIMIT: usize = 100;
const MAX_GREP_LIMIT: usize = 1000;

/// Per-call bounds. Production uses `Default`; tests lower individual bounds to
/// exercise truncation without creating tens of thousands of files.
#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub work: usize,
    /// Walker depth, with the root at depth zero. This stays below the
    /// `walkdir` default ten-open-directory cache so a closed ancestor never
    /// buffers all remaining sibling entries behind our visited counter.
    pub max_depth: usize,
    pub grep_file_bytes: usize,
    pub grep_total_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            work: 20_000,
            max_depth: 9,
            grep_file_bytes: 8 * 1024 * 1024,
            grep_total_bytes: 32 * 1024 * 1024,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LsInput {
    #[serde(default, deserialize_with = "present_input")]
    path: Option<String>,
    #[serde(default, deserialize_with = "present_input")]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindInput {
    pattern: String,
    #[serde(default, deserialize_with = "present_input")]
    path: Option<String>,
    #[serde(default, deserialize_with = "present_input")]
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepInput {
    pattern: String,
    #[serde(default, deserialize_with = "present_input")]
    path: Option<String>,
    #[serde(default, deserialize_with = "present_input")]
    glob: Option<String>,
    #[serde(default, deserialize_with = "present_input")]
    ignore_case: Option<bool>,
    #[serde(default, deserialize_with = "present_input")]
    literal: Option<bool>,
    #[serde(default, deserialize_with = "present_input")]
    limit: Option<usize>,
}

fn parse<T: for<'de> Deserialize<'de>>(args: &Value, name: &str) -> Result<T, ToolOutput> {
    serde_json::from_value(args.clone()).map_err(|e| {
        error(format!(
            "invalid {name} arguments: {}",
            preview(&e.to_string(), 512)
        ))
    })
}

fn path_input(value: &str) -> Result<(), String> {
    if value.len() > MAX_QUERY_BYTES {
        return Err(format!(
            "path exceeds the {MAX_QUERY_BYTES}-byte UTF-8 limit"
        ));
    }
    Ok(())
}

fn pattern(value: &str, field: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if value.len() > MAX_QUERY_BYTES {
        return Err(format!(
            "{field} exceeds the {MAX_QUERY_BYTES}-byte UTF-8 limit"
        ));
    }
    Ok(())
}

fn limit(value: Option<usize>, default: usize, max: usize, name: &str) -> Result<usize, String> {
    let value = value.unwrap_or(default);
    if !(1..=max).contains(&value) {
        return Err(format!("{name} limit must be between 1 and {max}"));
    }
    Ok(value)
}

pub(super) fn ls(cwd: &std::path::Path, args: &Value, stop: &CancellationToken) -> ToolOutput {
    ls::run(cwd, args, stop, &Limits::default())
}

pub(super) fn find(cwd: &std::path::Path, args: &Value, stop: &CancellationToken) -> ToolOutput {
    find::run(cwd, args, stop, &Limits::default())
}

pub(super) fn grep(cwd: &std::path::Path, args: &Value, stop: &CancellationToken) -> ToolOutput {
    grep::run(cwd, args, stop, &Limits::default())
}

#[cfg(test)]
mod tests;
