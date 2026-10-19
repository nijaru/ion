//! Recursive regular-file discovery by gitignore-style glob.
use std::path::Path;

use ion_core::CodingToolOutput as ToolOutput;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::super::{error, success};
use super::{
    DEFAULT_FIND_LIMIT, FindInput, Limits, MAX_FIND_LIMIT,
    glob::GlobFilter,
    output::OutputBudget,
    parse, pattern,
    walk::{Root, RootKind, Scan, check_stop, walk},
};

pub(super) fn run(
    cwd: &Path,
    args: &Value,
    stop: &CancellationToken,
    limits: &Limits,
) -> ToolOutput {
    let input: FindInput = match parse(args, "find") {
        Ok(input) => input,
        Err(output) => return output,
    };
    if let Err(message) = pattern(&input.pattern, "pattern") {
        return error(message);
    }
    let limit = match super::limit(input.limit, DEFAULT_FIND_LIMIT, MAX_FIND_LIMIT, "find") {
        Ok(limit) => limit,
        Err(message) => return error(message),
    };
    let raw_path = input.path.as_deref().unwrap_or(".");
    let root = match Root::resolve(cwd, raw_path, RootKind::Directory, stop) {
        Ok(root) => root,
        Err(message) => return error(message),
    };
    let filter = match GlobFilter::new(&root.path, &input.pattern) {
        Ok(filter) => filter,
        Err(message) => return error(message),
    };
    let mut scan = Scan::new(limits);
    let base = json!({
        "root": root.display,
        "files": [],
        "returned": 0,
        "limit": limit,
        "exhaustive": false,
        "visited_entries": scan.visited,
        "work_budget": scan.work_limit,
        "max_depth": scan.max_depth,
        "skipped": {},
        "omissions": [],
        "diagnostics": [],
        "output_limited": false,
    });
    let mut output = OutputBudget::new(&base);
    let mut result_limited = false;
    let mut output_limited = false;
    let mut found = 0usize;
    for entry in walk(&root.path, limits.max_depth) {
        if stop.is_cancelled() {
            return error("cancelled during repository discovery");
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(failure) => {
                let visited = match scan.visit(0, false, stop) {
                    Ok(visited) => visited,
                    Err(message) => return error(message),
                };
                if !visited {
                    break;
                }
                scan.skips.filesystem_errors += 1;
                scan.diagnostics.push(&root.path, failure);
                continue;
            }
        };
        // Ignore-file parsing errors are attached to an otherwise valid entry.
        // Preserve the entry's observation but never claim completeness.
        if let Some(failure) = entry.error() {
            scan.skips.filesystem_errors += 1;
            scan.diagnostics.push(entry.path(), failure);
        }
        let depth = entry.depth();
        let Some(file_type) = entry.file_type() else {
            let visited = match scan.visit(depth, false, stop) {
                Ok(visited) => visited,
                Err(message) => return error(message),
            };
            if !visited {
                break;
            }
            scan.skips.filesystem_errors += 1;
            scan.diagnostics
                .push(entry.path(), "walker entry has no file type");
            continue;
        };
        let visited = match scan.visit(depth, file_type.is_dir(), stop) {
            Ok(visited) => visited,
            Err(message) => return error(message),
        };
        if !visited {
            break;
        }
        if depth == 0 || !file_type.is_file() {
            continue;
        }
        let relative = match root.relative(entry.path()) {
            Ok(relative) => relative,
            Err(issue) => {
                scan.path_issue(issue);
                continue;
            }
        };
        if !filter.matches(&relative) {
            continue;
        }
        if found >= limit {
            result_limited = true;
            break;
        }
        found += 1;
        if !output.push(json!(relative)) {
            output_limited = true;
            break;
        }
    }
    if let Err(message) = check_stop(stop) {
        return error(message);
    }
    let (files, budget_limited) = output.finish();
    output_limited |= budget_limited;
    let returned = files.len();
    let exhaustive = scan.exhaustive(result_limited, output_limited);
    let mut omissions = scan.omissions();
    if result_limited {
        omissions.push(format!(
            "result limit {limit} reached; refine the root or glob"
        ));
    }
    if output_limited {
        omissions
            .push("serialized output byte limit stopped files; refine the root or glob".into());
    }
    if files.is_empty() && !exhaustive {
        omissions.push("no matching file was observed, but this scan is incomplete".into());
    }
    let skipped = json!({
        "non_utf8_paths": scan.skips.non_utf8,
        "path_too_long": scan.skips.path_too_long,
        "filesystem_errors": scan.skips.filesystem_errors,
    });
    let visited = scan.visited;
    let work_limit = scan.work_limit;
    let max_depth = scan.max_depth;
    let diagnostics = scan.finish_diagnostics();
    let mut value = base;
    value["files"] = Value::Array(files);
    value["returned"] = json!(returned);
    value["exhaustive"] = json!(exhaustive);
    value["visited_entries"] = json!(visited);
    value["work_budget"] = json!(work_limit);
    value["max_depth"] = json!(max_depth);
    value["skipped"] = skipped;
    value["omissions"] = json!(omissions);
    value["diagnostics"] = Value::Array(diagnostics);
    value["output_limited"] = json!(output_limited);
    success(super::output::finalize(
        value,
        "files",
        "serialized output byte limit stopped files; refine the root or glob",
    ))
}
