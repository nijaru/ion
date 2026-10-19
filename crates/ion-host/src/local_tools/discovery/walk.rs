//! Search roots, ignore-aware traversal reports and bounded traversal state.
use std::{
    fs,
    path::{Path, PathBuf},
};

use ignore::WalkBuilder;
use tokio_util::sync::CancellationToken;

use super::{
    Limits,
    output::{Diagnostics, preview},
};

const MAX_ROOT_BYTES: usize = 4096;
const MAX_RESULT_PATH_BYTES: usize = 4096;

pub(super) enum RootKind {
    Directory,
    FileOrDirectory,
}

pub(super) struct Root {
    pub path: PathBuf,
    pub display: String,
    pub is_file: bool,
}

impl Root {
    pub(super) fn resolve(
        cwd: &Path,
        raw: &str,
        kind: RootKind,
        stop: &CancellationToken,
    ) -> Result<Self, String> {
        check_stop(stop)?;
        super::path_input(raw)?;
        let joined = super::super::files::path(cwd, raw)?;
        let raw_preview = preview(raw, 512);
        let path = fs::canonicalize(&joined)
            .map_err(|error| format!("cannot resolve search root {raw_preview:?}: {error}"))?;
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("cannot inspect search root {raw_preview:?}: {error}"))?;
        let is_file = metadata.is_file();
        if matches!(kind, RootKind::Directory) && !metadata.is_dir() {
            return Err(format!("search root {raw_preview:?} is not a directory"));
        }
        let display = path
            .to_str()
            .ok_or_else(|| "search root path is not UTF-8".to_owned())?
            .to_owned();
        if display.len() > MAX_ROOT_BYTES {
            return Err(format!(
                "search root path exceeds the {MAX_ROOT_BYTES}-byte display bound"
            ));
        }
        if !is_file && !metadata.is_dir() {
            return Err(format!(
                "search root {raw_preview:?} is not a regular file or directory"
            ));
        }
        Ok(Self {
            path,
            display,
            is_file,
        })
    }

    /// Slash-normalized path relative to this root. The root file itself maps
    /// to its basename so output remains anchored to the returned root.
    pub(super) fn relative(&self, path: &Path) -> Result<String, PathIssue> {
        let relative = path.strip_prefix(&self.path).unwrap_or(path);
        let mut text = String::new();
        for component in relative.components() {
            let std::path::Component::Normal(part) = component else {
                if matches!(component, std::path::Component::CurDir) {
                    continue;
                }
                return Err(PathIssue::Invalid);
            };
            let part = part.to_str().ok_or(PathIssue::NonUtf8)?;
            if !text.is_empty() {
                text.push('/');
            }
            text.push_str(part);
        }
        if text.is_empty() {
            let name = self
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(PathIssue::NonUtf8)?;
            text.push_str(name);
        }
        if text.len() > MAX_RESULT_PATH_BYTES {
            return Err(PathIssue::TooLong);
        }
        Ok(text)
    }
}

pub(super) enum PathIssue {
    NonUtf8,
    TooLong,
    Invalid,
}

pub(super) fn check_stop(stop: &CancellationToken) -> Result<(), String> {
    if stop.is_cancelled() {
        Err("cancelled during repository discovery".into())
    } else {
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Skips {
    pub binary: usize,
    pub oversized: usize,
    pub byte_budget: usize,
    pub non_utf8: usize,
    pub path_too_long: usize,
    pub filesystem_errors: usize,
}

impl Skips {
    fn any(&self) -> bool {
        self.binary > 0
            || self.oversized > 0
            || self.byte_budget > 0
            || self.non_utf8 > 0
            || self.path_too_long > 0
            || self.filesystem_errors > 0
    }
}

pub(super) struct Scan {
    pub visited: usize,
    pub work_limit: usize,
    pub max_depth: usize,
    work_limited: bool,
    depth_limited: bool,
    pub skips: Skips,
    pub diagnostics: Diagnostics,
}

impl Scan {
    pub(super) fn new(limits: &Limits) -> Self {
        Self {
            visited: 0,
            work_limit: limits.work,
            max_depth: limits.max_depth,
            work_limited: false,
            depth_limited: false,
            skips: Skips::default(),
            diagnostics: Diagnostics::default(),
        }
    }

    /// Count one observed walker entry before doing any work for it. A false
    /// return means the work budget stopped traversal.
    pub(super) fn visit(
        &mut self,
        depth: usize,
        is_dir: bool,
        stop: &CancellationToken,
    ) -> Result<bool, String> {
        check_stop(stop)?;
        if self.visited >= self.work_limit {
            self.work_limited = true;
            return Ok(false);
        }
        self.visited += 1;
        if is_dir && depth >= self.max_depth {
            // The walker will not descend here. Mark even an apparently empty
            // directory because its contents were not observed.
            self.depth_limited = true;
        }
        Ok(true)
    }

    pub(super) fn path_issue(&mut self, issue: PathIssue) {
        match issue {
            PathIssue::NonUtf8 => self.skips.non_utf8 += 1,
            PathIssue::TooLong => self.skips.path_too_long += 1,
            PathIssue::Invalid => self.skips.filesystem_errors += 1,
        }
    }

    pub(super) fn omissions(&self) -> Vec<String> {
        let mut omissions = Vec::new();
        if self.work_limited {
            omissions.push(format!(
                "work budget of {} visited entries reached; refine the root or glob",
                self.work_limit
            ));
        }
        if self.depth_limited {
            omissions.push(format!(
                "maximum traversal depth {} reached; refine the root",
                self.max_depth
            ));
        }
        if self.skips.binary > 0 {
            omissions.push(format!(
                "{} binary or invalid UTF-8 file(s) excluded",
                self.skips.binary
            ));
        }
        if self.skips.oversized > 0 {
            omissions.push(format!(
                "{} file(s) exceeded the per-file byte bound",
                self.skips.oversized
            ));
        }
        if self.skips.byte_budget > 0 {
            omissions.push(format!(
                "{} file(s) skipped for the total inspected-byte budget",
                self.skips.byte_budget
            ));
        }
        if self.skips.non_utf8 > 0 {
            omissions.push(format!(
                "{} non-UTF-8 path(s) skipped; refine the root",
                self.skips.non_utf8
            ));
        }
        if self.skips.path_too_long > 0 {
            omissions.push(format!(
                "{} result path(s) exceeded the display bound",
                self.skips.path_too_long
            ));
        }
        if self.skips.filesystem_errors > 0 {
            omissions.push(format!(
                "{} descendant filesystem error(s); partial results preserved",
                self.skips.filesystem_errors
            ));
        }
        if self.diagnostics.omitted() > 0 {
            omissions.push(format!(
                "{} additional diagnostic(s) omitted",
                self.diagnostics.omitted()
            ));
        }
        omissions
    }

    pub(super) fn exhaustive(&self, result_limited: bool, output_limited: bool) -> bool {
        !result_limited
            && !output_limited
            && !self.work_limited
            && !self.depth_limited
            && !self.skips.any()
    }

    pub(super) fn finish_diagnostics(self) -> Vec<serde_json::Value> {
        self.diagnostics.finish()
    }
}

pub(super) fn walk(
    root: &Path,
    max_depth: usize,
) -> impl Iterator<Item = Result<ignore::DirEntry, ignore::Error>> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .follow_links(false)
        // Apply local ignore files even outside a Git checkout, like fd's
        // --no-require-git. Ignore rules are traversal convenience, not a
        // confidentiality boundary.
        .require_git(false)
        .git_global(false)
        .max_depth(Some(max_depth))
        .filter_entry(|entry| {
            // A hidden directory is otherwise included. The repository metadata
            // tree is explicitly outside discovery.
            entry.depth() == 0 || entry.file_name() != std::ffi::OsStr::new(".git")
        });
    builder.build()
}
