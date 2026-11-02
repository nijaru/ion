//! Bounded filesystem metadata discovery. Workers never borrow editor state.
use super::picker::PickerOutput;
use crate::terminal_completion::FileScope;
use anyhow::{Context, Result};
use ignore::WalkBuilder;
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub(super) const MAX_PATHS: usize = 20_000;
pub(super) const MAX_ENTRIES: usize = 100_000;

pub(super) fn discover(scope: &FileScope, stop: &CancellationToken) -> Result<PickerOutput> {
    if stop.is_cancelled() {
        return Ok(PickerOutput::Cancelled);
    }
    match scope {
        FileScope::Project(cwd) => discover_project(cwd, stop),
        FileScope::Directory(path) => discover_directory(path, stop),
        FileScope::HomeUnavailable => {
            anyhow::bail!(
                "HOME is unavailable or not absolute; cannot complete a home-relative path"
            )
        }
    }
}

fn discover_directory(path: &Path, stop: &CancellationToken) -> Result<PickerOutput> {
    let mut entries =
        std::fs::read_dir(path).with_context(|| format!("list directory {}", path.display()))?;
    let mut paths = Vec::new();
    let mut visited = 0;
    let limited = loop {
        if stop.is_cancelled() {
            return Ok(PickerOutput::Cancelled);
        }
        if paths.len() >= MAX_PATHS || visited >= MAX_ENTRIES {
            break true;
        }
        let Some(entry) = entries.next() else {
            break false;
        };
        let entry = entry.context("read directory entry")?;
        visited += 1;
        let kind = entry.file_type().context("inspect directory entry")?;
        // Explicit directory lookup classifies symlinks, but never walks their
        // targets recursively. Entering a directory requests a new scoped scan.
        let kind = if kind.is_symlink() {
            match std::fs::metadata(entry.path()) {
                Ok(metadata) => metadata.file_type(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error).context("inspect completion symlink"),
            }
        } else {
            kind
        };
        if stop.is_cancelled() {
            return Ok(PickerOutput::Cancelled);
        }
        if !kind.is_file() && !kind.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name
            .to_str()
            .with_context(|| format!("directory entry is not valid UTF-8: {name:?}"))?;
        paths.push(if kind.is_dir() {
            format!("{name}/")
        } else {
            name.to_owned()
        });
    };
    paths.sort();
    Ok(PickerOutput::Files { paths, limited })
}

fn discover_project(cwd: &Path, stop: &CancellationToken) -> Result<PickerOutput> {
    if stop.is_cancelled() {
        return Ok(PickerOutput::Cancelled);
    }
    let mut walker = WalkBuilder::new(cwd)
        .follow_links(false)
        .require_git(false)
        .build();
    let mut paths = Vec::new();
    let mut visited = 0;
    let limited = loop {
        // An in-flight filesystem call cannot be killed. Check before and after
        // each traversal step; cap admission even for trees containing no files.
        if stop.is_cancelled() {
            return Ok(PickerOutput::Cancelled);
        }
        if paths.len() >= MAX_PATHS || visited >= MAX_ENTRIES {
            break true;
        }
        let entry = walker.next();
        // Preserve errors even when dismissal raced with this traversal step.
        let entry = match entry {
            Some(entry) => entry.context("traverse project files")?,
            None => break false,
        };
        if let Some(error) = entry.error() {
            return Err(anyhow::anyhow!("project ignore rules: {error}"));
        }
        if stop.is_cancelled() {
            return Ok(PickerOutput::Cancelled);
        }
        visited += 1;
        if entry.depth() > 0
            && entry
                .file_type()
                .is_some_and(|kind| kind.is_file() || kind.is_dir())
        {
            let relative = entry
                .path()
                .strip_prefix(cwd)
                .context("project file escaped traversal root")?;
            let path = relative
                .to_str()
                .with_context(|| format!("project file path is not valid UTF-8: {relative:?}"))?;
            paths.push(if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                format!("{path}/")
            } else {
                path.to_owned()
            });
        }
    };
    paths.sort();
    Ok(PickerOutput::Files { paths, limited })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_directory_listing_is_shallow_and_reports_missing_roots() {
        let root = std::env::temp_dir().join(format!("ion-directory-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/nested.rs"), "fixture").unwrap();
        std::fs::write(root.join("visible.rs"), "fixture").unwrap();
        std::fs::write(root.join("ignored.rs"), "fixture").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.rs\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("sub"), root.join("linked")).unwrap();
        let PickerOutput::Files { paths, limited } = discover(
            &FileScope::Directory(root.clone()),
            &CancellationToken::new(),
        )
        .unwrap() else {
            panic!("unexpected cancellation")
        };
        assert!(!limited);
        for name in [".gitignore", "visible.rs", "ignored.rs", "sub/"] {
            assert!(paths.iter().any(|path| path == name));
        }
        assert!(!paths.iter().any(|path| path.contains("nested")));
        #[cfg(unix)]
        assert!(paths.iter().any(|path| path == "linked/"));
        assert!(
            discover(
                &FileScope::Directory(root.join("missing")),
                &CancellationToken::new()
            )
            .is_err()
        );
        assert!(discover(&FileScope::HomeUnavailable, &CancellationToken::new()).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
