//! One bounded, sorted directory listing.
use std::{fs, path::Path};

use ion_core::CodingToolOutput as ToolOutput;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::super::{error, success};
use super::{
    DEFAULT_LS_LIMIT, Limits, LsInput, MAX_LS_LIMIT,
    output::{OutputBudget, preview},
    parse,
    walk::{Root, RootKind, Scan, check_stop},
};

struct Entry {
    name: String,
    kind: &'static str,
}

struct TopEntries {
    limit: usize,
    entries: Vec<Entry>,
    observed: usize,
}

impl TopEntries {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            entries: Vec::with_capacity(limit.min(256)),
            observed: 0,
        }
    }

    fn push(&mut self, entry: Entry) {
        self.observed += 1;
        let position = self
            .entries
            .binary_search_by(|current| current.name.as_str().cmp(entry.name.as_str()))
            .unwrap_or_else(|position| position);
        if position < self.limit {
            self.entries.insert(position, entry);
            if self.entries.len() > self.limit {
                self.entries.pop();
            }
        }
    }
}

pub(super) fn run(
    cwd: &Path,
    args: &Value,
    stop: &CancellationToken,
    limits: &Limits,
) -> ToolOutput {
    let input: LsInput = match parse(args, "ls") {
        Ok(input) => input,
        Err(output) => return output,
    };
    let limit = match super::limit(input.limit, DEFAULT_LS_LIMIT, MAX_LS_LIMIT, "ls") {
        Ok(limit) => limit,
        Err(message) => return error(message),
    };
    let raw_path = input.path.as_deref().unwrap_or(".");
    let root = match Root::resolve(cwd, raw_path, RootKind::Directory, stop) {
        Ok(root) => root,
        Err(message) => return error(message),
    };
    if let Err(message) = check_stop(stop) {
        return error(message);
    }
    let reader = match fs::read_dir(&root.path) {
        Ok(reader) => reader,
        Err(failure) => {
            return error(format!("ls {:?}: {failure}", preview(&root.display, 512)));
        }
    };
    let mut scan = Scan::new(limits);
    let mut top = TopEntries::new(limit);
    for entry in reader {
        // A directory listing is one observed entry at depth one.
        let observed = match scan.visit(1, false, stop) {
            Ok(observed) => observed,
            Err(message) => return error(message),
        };
        if !observed {
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(failure) => {
                scan.skips.filesystem_errors += 1;
                scan.diagnostics.push(&root.path, failure);
                continue;
            }
        };
        let name = match entry.file_name().to_str() {
            Some(name) => name.to_owned(),
            None => {
                scan.skips.non_utf8 += 1;
                continue;
            }
        };
        let kind = match entry.file_type() {
            Ok(file_type) if file_type.is_symlink() => "symlink",
            Ok(file_type) if file_type.is_dir() => "dir",
            Ok(file_type) if file_type.is_file() => "file",
            Ok(_) => "other",
            Err(failure) => {
                scan.skips.filesystem_errors += 1;
                scan.diagnostics.push(&root.path.join(&name), failure);
                continue;
            }
        };
        top.push(Entry { name, kind });
    }
    let observed = top.observed;
    let result_limited = observed > limit;
    let base = json!({
        "root": root.display,
        "entries": [],
        "returned": 0,
        "limit": limit,
        "exhaustive": false,
        "visited_entries": scan.visited,
        "work_budget": scan.work_limit,
        "skipped": {
            "non_utf8_names": scan.skips.non_utf8,
            "filesystem_errors": scan.skips.filesystem_errors,
        },
        "omissions": [],
        "diagnostics": [],
        "output_limited": false,
    });
    let mut output = OutputBudget::new(&base);
    let mut output_limited = false;
    for entry in top.entries {
        if !output.push(json!({"name": entry.name, "type": entry.kind})) {
            output_limited = true;
            break;
        }
    }
    let (entries, budget_limited) = output.finish();
    output_limited |= budget_limited;
    let returned = entries.len();
    let exhaustive = scan.exhaustive(result_limited, output_limited);
    let mut omissions = scan.omissions();
    if result_limited {
        omissions.push(format!(
            "entry limit {limit} reached; observed {observed} entries, so the returned subset is partial"
        ));
    }
    if output_limited {
        omissions.push("serialized output byte limit stopped entries; refine the directory".into());
    }
    if entries.is_empty() && !exhaustive {
        omissions.push("empty listing is not evidence that the directory has no entries".into());
    }
    let diagnostics = scan.finish_diagnostics();
    let mut value = base;
    value["entries"] = Value::Array(entries);
    value["returned"] = json!(returned);
    value["exhaustive"] = json!(exhaustive);
    value["omissions"] = json!(omissions);
    value["diagnostics"] = Value::Array(diagnostics);
    value["output_limited"] = json!(output_limited);
    success(super::output::finalize(
        value,
        "entries",
        "serialized output byte limit stopped entries; refine the directory",
    ))
}
