//! Bounded recursive content search with line-oriented snippets.
use std::{io::Read, path::Path};

use ion_core::CodingToolOutput as ToolOutput;
use regex::{Regex, RegexBuilder};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::super::{error, success};
use super::{
    DEFAULT_GREP_LIMIT, GrepInput, Limits, MAX_GREP_LIMIT,
    glob::GlobFilter,
    output::{OutputBudget, preview},
    parse, pattern,
    walk::{Root, RootKind, Scan, check_stop, walk},
};

const SNIPPET_BYTES: usize = 1024;
const REGEX_SIZE_LIMIT: usize = 1024 * 1024;

pub(super) fn run(
    cwd: &Path,
    args: &Value,
    stop: &CancellationToken,
    limits: &Limits,
) -> ToolOutput {
    let input: GrepInput = match parse(args, "grep") {
        Ok(input) => input,
        Err(output) => return output,
    };
    if let Err(message) = pattern(&input.pattern, "pattern") {
        return error(message);
    }
    if let Some(glob) = input.glob.as_deref()
        && let Err(message) = pattern(glob, "glob")
    {
        return error(message);
    }
    let limit = match super::limit(input.limit, DEFAULT_GREP_LIMIT, MAX_GREP_LIMIT, "grep") {
        Ok(limit) => limit,
        Err(message) => return error(message),
    };
    let raw_path = input.path.as_deref().unwrap_or(".");
    let root = match Root::resolve(cwd, raw_path, RootKind::FileOrDirectory, stop) {
        Ok(root) => root,
        Err(message) => return error(message),
    };
    let glob = match input.glob.as_deref() {
        Some(glob) => match GlobFilter::new(&root.path, glob) {
            Ok(glob) => Some(glob),
            Err(message) => return error(message),
        },
        None => None,
    };
    let regex = match compile(
        &input.pattern,
        input.literal.unwrap_or(false),
        input.ignore_case.unwrap_or(false),
    ) {
        Ok(regex) => regex,
        Err(message) => return error(message),
    };
    let base = json!({
        "root": root.display,
        "matches": [],
        "returned": 0,
        "limit": limit,
        "exhaustive": false,
        "visited_entries": 0,
        "work_budget": limits.work,
        "max_depth": limits.max_depth,
        "inspected_bytes": 0,
        "byte_budget": limits.grep_total_bytes,
        "skipped": {},
        "omissions": [],
        "diagnostics": [],
        "output_limited": false,
    });
    let mut search = Search::new(&regex, limit, limits, stop, base);

    if root.is_file {
        let relative = match root.relative(&root.path) {
            Ok(relative) => relative,
            Err(issue) => {
                search.scan.path_issue(issue);
                String::new()
            }
        };
        if !relative.is_empty() && glob.as_ref().is_none_or(|glob| glob.matches(&relative)) {
            match search.scan_file(&root.path, &relative) {
                Ok(()) => {}
                Err(GrepStop::Cancelled) => return error("cancelled during repository discovery"),
                Err(GrepStop::Io) => {}
            }
        }
    } else {
        for entry in walk(&root.path, limits.max_depth) {
            if stop.is_cancelled() {
                return error("cancelled during repository discovery");
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(failure) => {
                    let visited = match search.scan.visit(0, false, stop) {
                        Ok(visited) => visited,
                        Err(message) => return error(message),
                    };
                    if !visited {
                        break;
                    }
                    search.scan.skips.filesystem_errors += 1;
                    search.scan.diagnostics.push(&root.path, failure);
                    continue;
                }
            };
            // Ignore-file parsing errors are attached to an otherwise valid
            // entry. Keep its observations, but never claim completeness.
            if let Some(failure) = entry.error() {
                search.scan.skips.filesystem_errors += 1;
                search.scan.diagnostics.push(entry.path(), failure);
            }
            let depth = entry.depth();
            let Some(file_type) = entry.file_type() else {
                let visited = match search.scan.visit(depth, false, stop) {
                    Ok(visited) => visited,
                    Err(message) => return error(message),
                };
                if !visited {
                    break;
                }
                search.scan.skips.filesystem_errors += 1;
                search
                    .scan
                    .diagnostics
                    .push(entry.path(), "walker entry has no file type");
                continue;
            };
            let visited = match search.scan.visit(depth, file_type.is_dir(), stop) {
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
                    search.scan.path_issue(issue);
                    continue;
                }
            };
            if glob.as_ref().is_some_and(|glob| !glob.matches(&relative)) {
                continue;
            }
            if search.inspected >= limits.grep_total_bytes {
                search.total_byte_limited = true;
                break;
            }
            match search.scan_file(entry.path(), &relative) {
                Ok(()) => {}
                Err(GrepStop::Cancelled) => return error("cancelled during repository discovery"),
                Err(GrepStop::Io) => continue,
            }
            if search.result_limited || search.output_limited || search.total_byte_limited {
                break;
            }
        }
    }
    if stop.is_cancelled() {
        return error("cancelled during repository discovery");
    }
    success(search.finish(
        &root.display,
        root.is_file,
        "serialized output byte limit stopped matches; refine the search",
    ))
}

enum GrepStop {
    Cancelled,
    Io,
}

/// One content-search call's authoritative mutable state. Keeping read
/// accounting, match/output caps and traversal reporting together prevents a
/// successful return from omitting the global byte charge.
struct Search<'a> {
    regex: &'a Regex,
    limit: usize,
    limits: &'a Limits,
    stop: &'a CancellationToken,
    output: OutputBudget,
    scan: Scan,
    found: usize,
    inspected: usize,
    result_limited: bool,
    output_limited: bool,
    total_byte_limited: bool,
}

impl<'a> Search<'a> {
    fn new(
        regex: &'a Regex,
        limit: usize,
        limits: &'a Limits,
        stop: &'a CancellationToken,
        base: Value,
    ) -> Self {
        Self {
            regex,
            limit,
            limits,
            stop,
            output: OutputBudget::new(&base),
            scan: Scan::new(limits),
            found: 0,
            inspected: 0,
            result_limited: false,
            output_limited: false,
            total_byte_limited: false,
        }
    }

    fn scan_file(&mut self, path: &Path, relative: &str) -> Result<(), GrepStop> {
        check_stop(self.stop).map_err(|_| GrepStop::Cancelled)?;
        let file = crate::file_io::open_regular(path).map_err(|failure| {
            self.scan.skips.filesystem_errors += 1;
            self.scan.diagnostics.push(path, failure);
            GrepStop::Io
        })?;
        let metadata = file.metadata().map_err(|failure| {
            self.scan.skips.filesystem_errors += 1;
            self.scan.diagnostics.push(path, failure);
            GrepStop::Io
        })?;
        if metadata.len() > self.limits.grep_file_bytes as u64 {
            self.scan.skips.oversized += 1;
            return Ok(());
        }
        let available = self.limits.grep_total_bytes.saturating_sub(self.inspected);
        if metadata.len() > available as u64 {
            self.scan.skips.byte_budget += 1;
            return Ok(());
        }
        let read_limit = self.limits.grep_file_bytes.min(available);
        let bytes = match read_file(file, read_limit, self.stop, &mut self.inspected) {
            Ok(bytes) => bytes,
            Err(ReadFailure::Cancelled) => return Err(GrepStop::Cancelled),
            Err(ReadFailure::Io(failure)) => {
                self.scan.skips.filesystem_errors += 1;
                self.scan.diagnostics.push(path, failure);
                return Ok(());
            }
        };
        if bytes.len() > read_limit {
            // A file that grew after metadata was observed is not safely
            // searchable within this call's bound. `read_file` already charged
            // every byte actually read, including the one-byte growth probe.
            if read_limit == self.limits.grep_file_bytes {
                self.scan.skips.oversized += 1;
            } else {
                self.scan.skips.byte_budget += 1;
                self.total_byte_limited = true;
            }
            return Ok(());
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) if !bytes.contains(&0) => text,
            _ => {
                self.scan.skips.binary += 1;
                return Ok(());
            }
        };
        let mut line_number = 0u64;
        let mut line_start = 0usize;
        for segment in text.split_inclusive('\n') {
            if self.stop.is_cancelled() {
                return Err(GrepStop::Cancelled);
            }
            line_number += 1;
            let raw_len = segment.len();
            let line = segment.strip_suffix('\n').unwrap_or(segment);
            let line = line.strip_suffix('\r').unwrap_or(line);
            if let Some(found_match) = self.regex.find(line) {
                if self.found >= self.limit {
                    self.result_limited = true;
                    return Ok(());
                }
                let (text, truncated) = snippet(line, found_match.start(), found_match.end());
                let item = json!({
                    "path": relative,
                    "line": line_number,
                    "byte_offset": line_start.saturating_add(found_match.start()),
                    "text": text,
                    "truncated": truncated,
                });
                self.found += 1;
                if !self.output.push(item) {
                    self.output_limited = true;
                    return Ok(());
                }
            }
            line_start = line_start.saturating_add(raw_len);
        }
        if self.stop.is_cancelled() {
            return Err(GrepStop::Cancelled);
        }
        Ok(())
    }

    fn finish(self, root: &str, root_is_file: bool, output_omission: &str) -> Value {
        let Self {
            output,
            scan,
            inspected,
            result_limited,
            output_limited,
            total_byte_limited,
            limit,
            limits,
            ..
        } = self;
        let (matches, budget_limited) = output.finish();
        let output_limited = output_limited || budget_limited;
        let returned = matches.len();
        // A stopped total-byte scan is incomplete even when the last file was
        // searched normally and no per-file skip was recorded.
        let exhaustive = scan.exhaustive(result_limited, output_limited) && !total_byte_limited;
        let mut omissions = scan.omissions();
        if result_limited {
            omissions.push(format!(
                "match limit {limit} reached; refine the root, glob or pattern"
            ));
        }
        if output_limited {
            omissions.push(output_omission.into());
        }
        if total_byte_limited {
            omissions.push(format!(
                "total inspected-byte budget {} reached; refine the root or glob",
                limits.grep_total_bytes
            ));
        }
        if matches.is_empty() && !exhaustive {
            omissions.push("no matching line was observed, but this search is incomplete".into());
        }
        let skipped = json!({
            "binary": scan.skips.binary,
            "oversized": scan.skips.oversized,
            "byte_budget": scan.skips.byte_budget,
            "non_utf8_paths": scan.skips.non_utf8,
            "path_too_long": scan.skips.path_too_long,
            "filesystem_errors": scan.skips.filesystem_errors,
        });
        let visited = scan.visited;
        let work_limit = scan.work_limit;
        let max_depth = scan.max_depth;
        let diagnostics = scan.finish_diagnostics();
        super::output::finalize(
            json!({
                "root": root,
                "root_kind": if root_is_file { "file" } else { "directory" },
                "matches": Value::Array(matches),
                "returned": returned,
                "limit": limit,
                "exhaustive": exhaustive,
                "visited_entries": visited,
                "work_budget": work_limit,
                "max_depth": max_depth,
                "inspected_bytes": inspected,
                "byte_budget": limits.grep_total_bytes,
                "skipped": skipped,
                "omissions": omissions,
                "diagnostics": Value::Array(diagnostics),
                "output_limited": output_limited,
            }),
            "matches",
            output_omission,
        )
    }
}

fn compile(pattern: &str, literal: bool, ignore_case: bool) -> Result<Regex, String> {
    let pattern = if literal {
        regex::escape(pattern)
    } else {
        pattern.to_owned()
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(ignore_case)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|failure| format!("invalid regex: {}", preview(&failure.to_string(), 512)))
}

#[derive(Debug)]
enum ReadFailure {
    Cancelled,
    Io(std::io::Error),
}

/// Charge every byte returned by the descriptor, including a growth probe or a
/// partial read that later fails. The caller cannot retry that byte budget.
fn read_file<R: Read>(
    mut file: R,
    limit: usize,
    stop: &CancellationToken,
    inspected: &mut usize,
) -> Result<Vec<u8>, ReadFailure> {
    let bound = limit.saturating_add(1);
    let mut bytes = Vec::with_capacity(bound.min(64 * 1024));
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if stop.is_cancelled() {
            return Err(ReadFailure::Cancelled);
        }
        let remaining = bound.saturating_sub(bytes.len());
        if remaining == 0 {
            break;
        }
        let want = remaining.min(buffer.len());
        let count = file.read(&mut buffer[..want]).map_err(ReadFailure::Io)?;
        if count == 0 {
            break;
        }
        *inspected = inspected.saturating_add(count);
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(bytes)
}

/// Visible, UTF-8-safe line context of at most 1024 bytes around the first match.
fn snippet(line: &str, start: usize, end: usize) -> (String, bool) {
    if line.len() <= SNIPPET_BYTES {
        return (line.to_owned(), false);
    }
    let marker_bytes = 6;
    let budget = SNIPPET_BYTES.saturating_sub(marker_bytes * 2);
    let match_len = end.saturating_sub(start);
    let (visible_start, visible_end) = if match_len >= budget {
        (start, floor_boundary(line, start.saturating_add(budget)))
    } else {
        let extra = budget - match_len;
        let mut before = start.min(extra / 2);
        let mut after = (line.len() - end).min(extra - before);
        if after < extra - before {
            before = start.min(before + (extra - before - after));
            after = (line.len() - end).min(extra - before);
        }
        (
            floor_boundary(line, start - before),
            floor_boundary(line, end + after),
        )
    };
    let mut text = String::with_capacity(SNIPPET_BYTES.saturating_add(9));
    if visible_start > 0 {
        text.push('…');
    }
    text.push_str(&line[visible_start..visible_end]);
    if visible_end < line.len() {
        text.push('…');
    }
    let truncated = visible_start > 0 || visible_end < line.len();
    (text, truncated)
}

fn floor_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::super::output::MAX_OUTPUT_BYTES;
    use super::*;
    use std::io::Cursor;

    struct PartialFailure {
        remaining: usize,
    }

    impl Read for PartialFailure {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("fault"));
            }
            let count = self.remaining.min(buffer.len());
            buffer[..count].fill(b'x');
            self.remaining -= count;
            Ok(count)
        }
    }

    #[test]
    fn chunk_reader_charges_probe_and_partial_failure_bytes() {
        let stop = CancellationToken::new();
        let mut inspected = 0;
        let bytes = read_file(Cursor::new(b"12345678".to_vec()), 3, &stop, &mut inspected).unwrap();
        assert_eq!(bytes, b"1234");
        assert_eq!(inspected, 4);

        let mut inspected = 0;
        let result = read_file(PartialFailure { remaining: 5 }, 10, &stop, &mut inspected);
        assert!(matches!(result, Err(ReadFailure::Io(_))));
        assert_eq!(inspected, 5);
    }

    #[test]
    fn finish_enforces_the_serialized_bound_after_max_diagnostics_and_full_items() {
        let stop = CancellationToken::new();
        let regex = Regex::new("needle").unwrap();
        let limits = Limits::default();
        let limit = 1000;
        let base = json!({
            "root": "/repo",
            "matches": [],
            "returned": 0,
            "limit": limit,
            "exhaustive": false,
            "visited_entries": 0,
            "work_budget": limits.work,
            "max_depth": limits.max_depth,
            "inspected_bytes": 0,
            "byte_budget": limits.grep_total_bytes,
            "skipped": {},
            "omissions": [],
            "diagnostics": [],
            "output_limited": false,
        });
        let mut search = Search::new(&regex, limit, &limits, &stop, base);

        // Four legal diagnostics with path and message at the 512-byte preview
        // bound expand sixfold when JSON escapes every control character.
        let control = "\u{1}".repeat(512);
        for _ in 0..4 {
            search.scan.diagnostics.push(Path::new(&control), &control);
        }

        let item = json!({
            "path": "src/file.rs",
            "line": 1,
            "byte_offset": 0,
            "text": "x".repeat(SNIPPET_BYTES),
            "truncated": false,
        });
        let mut pushed = 0;
        while search.output.push(item.clone()) {
            pushed += 1;
        }

        let value = search.finish(
            "/repo",
            false,
            "serialized output byte limit stopped matches; refine the search",
        );
        let returned = value["matches"].as_array().unwrap().len();
        let serialized = serde_json::to_vec(&value).unwrap();
        assert!(
            serialized.len() <= MAX_OUTPUT_BYTES,
            "{} > {MAX_OUTPUT_BYTES}",
            serialized.len()
        );
        assert!(
            returned < pushed,
            "expected dropped matches: {returned} of {pushed}"
        );
        assert_eq!(value["returned"], json!(returned));
        assert_eq!(value["exhaustive"], false);
        assert_eq!(value["output_limited"], true);
        assert_eq!(value["diagnostics"].as_array().unwrap().len(), 4);
        assert!(
            value["omissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|omission| omission
                    == "serialized output byte limit stopped matches; refine the search")
        );
    }
}
