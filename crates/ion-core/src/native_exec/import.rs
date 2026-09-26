//! Plan a bounded import from a stopped command's private workspace.
//!
//! The host must authenticate scope stop and hold the command claim before
//! publishing any change. An uncertain publication retains that claim.

use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
};

use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, RenameFlags, fchmod, fstat, mkdirat, openat, renameat,
    renameat_with, statat, unlinkat,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::snapshot::{BaseEntry, BaseKind};

pub(super) const MAX_IMPORT_CHANGES: usize = 32;
pub(super) const MAX_IMPORT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_IMPORT_EFFECT_BYTES: usize = 8 * 1024;
const MAX_IMPORT_PLAN_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) enum ImportChange {
    CreateDirectory {
        path: String,
        mode: u32,
    },
    PutFile {
        path: String,
        base: Option<BaseEntry>,
        output: BaseEntry,
    },
    RemoveFile {
        path: String,
        base: BaseEntry,
    },
    RemoveDirectory {
        path: String,
        base: BaseEntry,
    },
}

impl ImportChange {
    pub(super) fn path(&self) -> &str {
        match self {
            Self::CreateDirectory { path, .. }
            | Self::PutFile { path, .. }
            | Self::RemoveFile { path, .. }
            | Self::RemoveDirectory { path, .. } => path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct ImportPlan {
    /// Ordered so parents exist before file publication and empty directories
    /// are removed after their contents.
    pub changes: Vec<ImportChange>,
    pub copied_bytes: u64,
    /// A command may run Git against its private repository copy, but those
    /// administrative changes are never published to the live checkout.
    pub git_metadata_changed: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(super) enum ImportPlanError {
    #[error("private workspace manifest contains a duplicate or invalid path")]
    InvalidManifest,
    #[error("command changed a file into a directory or vice versa at {0}")]
    TypeChange(String),
    #[error("command changed directory permissions at {0}")]
    DirectoryMode(String),
    #[error("command import exceeds the bounded change or byte allowance")]
    Limit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ImportFailure {
    /// No visible change was attempted; the command's private changes remain
    /// unpublished.
    Preflight(String),
    /// These paths passed their publication and durability barriers. A later
    /// operation failed before it could mutate the live workspace.
    Partial {
        applied: Vec<String>,
        reason: String,
    },
    /// An effect or durability barrier could not be observed. Keep the command
    /// claim quarantined and do not retry the import automatically.
    Unknown {
        applied: Vec<String>,
        reason: String,
    },
}

/// Record the exact bounded publication intent before the first live change.
/// The file remains in host custody until the registry has durable terminal
/// evidence, so an owner crash retains both the plan and the private output.
pub(super) fn persist_import_plan(
    stage: &File,
    name: &str,
    plan: &ImportPlan,
) -> std::io::Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return Err(std::io::Error::other("invalid import plan name"));
    }
    let encoded = serde_json::to_vec(plan).map_err(std::io::Error::other)?;
    if encoded.len() > MAX_IMPORT_PLAN_BYTES {
        return Err(std::io::Error::other(
            "import plan exceeds durable record limit",
        ));
    }
    let mut file = File::from(openat(
        stage,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?);
    file.write_all(&encoded)?;
    file.sync_all()?;
    stage.sync_all()
}

pub(super) fn remove_import_plan(stage: &File, name: &str) -> std::io::Result<()> {
    unlinkat(stage, name, AtFlags::empty())?;
    stage.sync_all()
}

/// Publish a prevalidated plan after the command scope is positively stopped.
/// The caller holds the registry claim and permanent custody of both private
/// directories. `stage` must be protected, on the live root's filesystem, and
/// outside the command view. No automatic recovery or cleanup of unknown stages
/// is attempted after owner loss.
pub(super) fn apply_import(
    plan: &ImportPlan,
    base_manifest: &[BaseEntry],
    live_root: &File,
    output_root: &File,
    stage: &File,
) -> Result<Vec<String>, ImportFailure> {
    let live_meta = live_root
        .metadata()
        .map_err(|e| ImportFailure::Preflight(e.to_string()))?;
    let stage_meta = stage
        .metadata()
        .map_err(|e| ImportFailure::Preflight(e.to_string()))?;
    if !live_meta.is_dir() || !stage_meta.is_dir() || live_meta.dev() != stage_meta.dev() {
        return Err(ImportFailure::Preflight(
            "private stage is not a directory on the workspace filesystem".into(),
        ));
    }
    let bases = index(base_manifest).map_err(|e| ImportFailure::Preflight(e.to_string()))?;
    let created = plan
        .changes
        .iter()
        .filter_map(|change| match change {
            ImportChange::CreateDirectory { path, .. } => Some(path.as_str()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    for change in &plan.changes {
        preflight_change(change, &bases, &created, live_root)
            .map_err(|reason| ImportFailure::Preflight(format!("{}: {reason}", change.path())))?;
    }
    let mut applied = Vec::new();
    for change in &plan.changes {
        let result = match change {
            ImportChange::CreateDirectory { path, .. } => create_directory(live_root, path),
            ImportChange::PutFile { path, base, output } => {
                put_file(live_root, output_root, stage, path, base.as_ref(), output)
            }
            ImportChange::RemoveFile { path, base } => remove_entry(live_root, path, base, false),
            ImportChange::RemoveDirectory { path, base } => {
                remove_entry(live_root, path, base, true)
            }
        };
        match result {
            Ok(()) => applied.push(change.path().to_owned()),
            Err((reason, uncertain)) => {
                return Err(if uncertain {
                    ImportFailure::Unknown { applied, reason }
                } else {
                    ImportFailure::Partial { applied, reason }
                });
            }
        }
    }
    for change in plan.changes.iter().rev() {
        if let ImportChange::CreateDirectory { path, mode } = change
            && let Err(reason) = finalize_directory(live_root, path, *mode)
        {
            return Err(ImportFailure::Unknown { applied, reason });
        }
    }
    Ok(applied)
}

type PublicationResult = Result<(), (String, bool)>;

fn preflight_change(
    change: &ImportChange,
    bases: &BTreeMap<String, &BaseEntry>,
    created: &std::collections::BTreeSet<&str>,
    live_root: &File,
) -> Result<(), String> {
    let path = change.path();
    // Authenticate every existing ancestor, including directories that are not
    // themselves being changed. Newly created parents are absent at preflight.
    let mut ancestor = path.rsplit_once('/').map_or("", |(parent, _)| parent);
    while !ancestor.is_empty() {
        if let Some(base) = bases.get(ancestor) {
            verify_live(live_root, ancestor, base).map_err(|e| e.to_string())?;
        } else if !created.contains(ancestor) {
            return Err("parent was not in the captured workspace".into());
        }
        ancestor = ancestor.rsplit_once('/').map_or("", |(parent, _)| parent);
    }
    match change {
        ImportChange::CreateDirectory { .. } | ImportChange::PutFile { base: None, .. } => {
            let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            if !created.contains(parent)
                && path_exists(live_root, path).map_err(|e| e.to_string())?
            {
                return Err("new path already exists".into());
            }
        }
        ImportChange::PutFile {
            base: Some(base), ..
        }
        | ImportChange::RemoveFile { base, .. }
        | ImportChange::RemoveDirectory { base, .. } => {
            verify_live(live_root, path, base).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn verify_live(root: &File, path: &str, base: &BaseEntry) -> std::io::Result<()> {
    let entry = open_entry(root, path, base.kind == BaseKind::Directory)?;
    let meta = entry.metadata()?;
    if meta.dev() != base.device
        || meta.ino() != base.inode
        || meta.mode() & 0o777 != base.mode
        || meta.len() != base.size && base.kind == BaseKind::RegularFile
        || meta.mtime() != base.modified_seconds
        || meta.mtime_nsec() != base.modified_nanoseconds
        || meta.ctime() != base.changed_seconds
        || meta.ctime_nsec() != base.changed_nanoseconds
    {
        return Err(std::io::Error::other("captured base changed"));
    }
    if base.kind == BaseKind::RegularFile {
        let mut reader = &entry;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
        }
        if base.sha256.as_deref() != Some(format!("{:x}", digest.finalize()).as_str()) {
            return Err(std::io::Error::other("captured content changed"));
        }
        let after = entry.metadata()?;
        if after.len() != meta.len()
            || after.mtime() != meta.mtime()
            || after.mtime_nsec() != meta.mtime_nsec()
            || after.ctime() != meta.ctime()
            || after.ctime_nsec() != meta.ctime_nsec()
        {
            return Err(std::io::Error::other(
                "captured content changed while reading",
            ));
        }
    }
    Ok(())
}

fn path_exists(root: &File, path: &str) -> std::io::Result<bool> {
    let (parent, leaf) = parent_leaf(root, path)?;
    match statat(&parent, leaf, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(_) => Ok(true),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn create_directory(root: &File, path: &str) -> PublicationResult {
    let (parent, leaf) = parent_leaf(root, path).map_err(|e| (e.to_string(), false))?;
    if path_exists(root, path).map_err(|e| (e.to_string(), false))? {
        return Err((format!("{path}: new directory already exists"), false));
    }
    mkdirat(&parent, leaf, Mode::from_raw_mode(0o700))
        .map_err(|e| (format!("{path}: {e}"), false))?;
    parent
        .sync_all()
        .map_err(|e| (format!("{path}: {e}"), true))
}

fn finalize_directory(root: &File, path: &str, mode: u32) -> Result<(), String> {
    let directory = open_entry(root, path, true).map_err(|e| format!("{path}: {e}"))?;
    fchmod(&directory, Mode::from_raw_mode(mode as _)).map_err(|e| format!("{path}: {e}"))?;
    directory.sync_all().map_err(|e| format!("{path}: {e}"))
}

fn put_file(
    live_root: &File,
    output_root: &File,
    stage: &File,
    path: &str,
    base: Option<&BaseEntry>,
    output: &BaseEntry,
) -> PublicationResult {
    let mut source = open_entry(output_root, path, false)
        .map_err(|e| (format!("{path}: private output: {e}"), false))?;
    let stage_name = format!("ion-import-{}", uuid::Uuid::now_v7());
    let mut staged = File::from(
        openat(
            stage,
            stage_name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .map_err(|e| (format!("{path}: stage allocation: {e}"), false))?,
    );
    let copy = (|| -> std::io::Result<()> {
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut copied = 0_u64;
        loop {
            let n = source.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            copied = copied
                .checked_add(n as u64)
                .ok_or_else(|| std::io::Error::other("output size overflow"))?;
            if copied > output.size {
                return Err(std::io::Error::other("private output changed"));
            }
            digest.update(&buffer[..n]);
            staged.write_all(&buffer[..n])?;
        }
        if copied != output.size
            || output.sha256.as_deref() != Some(format!("{:x}", digest.finalize()).as_str())
        {
            return Err(std::io::Error::other("private output digest changed"));
        }
        fchmod(&staged, Mode::from_raw_mode(output.mode as _))?;
        staged.sync_all()?;
        stage.sync_all()?;
        Ok(())
    })();
    if let Err(error) = copy {
        return Err(clean_stage(stage, &stage_name, format!("{path}: {error}")));
    }
    let (parent, leaf) = match parent_leaf(live_root, path) {
        Ok(value) => value,
        Err(error) => return Err(clean_stage(stage, &stage_name, format!("{path}: {error}"))),
    };
    let still_valid = match base {
        Some(base) => verify_live(live_root, path, base),
        None => path_exists(live_root, path).and_then(|exists| {
            if exists {
                Err(std::io::Error::other("new path already exists"))
            } else {
                Ok(())
            }
        }),
    };
    if let Err(error) = still_valid {
        return Err(clean_stage(stage, &stage_name, format!("{path}: {error}")));
    }
    let rename = if base.is_none() {
        renameat_with(
            stage,
            stage_name.as_str(),
            &parent,
            leaf,
            RenameFlags::NOREPLACE,
        )
    } else {
        renameat(stage, stage_name.as_str(), &parent, leaf)
    };
    if let Err(error) = rename {
        return Err(clean_stage(
            stage,
            &stage_name,
            format!("{path}: rename: {error}"),
        ));
    }
    parent
        .sync_all()
        .map_err(|e| (format!("{path}: destination sync: {e}"), true))?;
    stage
        .sync_all()
        .map_err(|e| (format!("{path}: stage sync: {e}"), true))
}

fn clean_stage(stage: &File, name: &str, reason: String) -> (String, bool) {
    if unlinkat(stage, name, AtFlags::empty()).is_ok() && stage.sync_all().is_ok() {
        (reason, false)
    } else {
        (
            format!("{reason}; private stage cleanup is uncertain"),
            true,
        )
    }
}

fn remove_entry(root: &File, path: &str, base: &BaseEntry, directory: bool) -> PublicationResult {
    if directory {
        verify_directory_identity(root, path, base).map_err(|e| (format!("{path}: {e}"), false))?;
    } else {
        verify_live(root, path, base).map_err(|e| (format!("{path}: {e}"), false))?;
    }
    let (parent, leaf) = parent_leaf(root, path).map_err(|e| (format!("{path}: {e}"), false))?;
    unlinkat(
        &parent,
        leaf,
        if directory {
            AtFlags::REMOVEDIR
        } else {
            AtFlags::empty()
        },
    )
    .map_err(|e| (format!("{path}: unlink: {e}"), false))?;
    parent
        .sync_all()
        .map_err(|e| (format!("{path}: parent sync: {e}"), true))
}

fn verify_directory_identity(root: &File, path: &str, base: &BaseEntry) -> std::io::Result<()> {
    let directory = open_entry(root, path, true)?;
    let meta = directory.metadata()?;
    if meta.dev() != base.device || meta.ino() != base.inode || meta.mode() & 0o777 != base.mode {
        return Err(std::io::Error::other("captured directory identity changed"));
    }
    Ok(())
}

fn open_entry(root: &File, path: &str, directory: bool) -> std::io::Result<File> {
    let (parent, leaf) = parent_leaf(root, path)?;
    let flags = OFlags::RDONLY
        | OFlags::NOFOLLOW
        | OFlags::CLOEXEC
        | if directory {
            OFlags::DIRECTORY
        } else {
            OFlags::NONBLOCK
        };
    let file = File::from(openat(&parent, leaf, flags, Mode::empty())?);
    let observed = FileType::from_raw_mode(fstat(&file)?.st_mode);
    if observed
        != if directory {
            FileType::Directory
        } else {
            FileType::RegularFile
        }
    {
        return Err(std::io::Error::other("unexpected filesystem entry type"));
    }
    Ok(file)
}

fn parent_leaf<'a>(root: &File, path: &'a str) -> std::io::Result<(File, &'a str)> {
    let (parent, leaf) = path.rsplit_once('/').unwrap_or(("", path));
    if leaf.is_empty() || leaf == "." || leaf == ".." {
        return Err(std::io::Error::other("invalid import path"));
    }
    Ok((open_directory(root, parent)?, leaf))
}

fn open_directory(root: &File, path: &str) -> std::io::Result<File> {
    let mut current = root.try_clone()?;
    if path.is_empty() {
        return Ok(current);
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(std::io::Error::other("invalid import path"));
        }
        current = File::from(openat(
            &current,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
    }
    Ok(current)
}

pub(super) fn plan_import(
    before: &[BaseEntry],
    after: &[BaseEntry],
) -> Result<ImportPlan, ImportPlanError> {
    let base = index(before)?;
    let result = index(after)?;
    let mut new_directories = Vec::new();
    let mut put_files = Vec::new();
    let mut removed_files = Vec::new();
    let mut removed_directories = Vec::new();
    let mut copied_bytes = 0_u64;
    let mut verified_bytes = 0_u64;
    let mut git_metadata_changed = false;

    for (path, output) in &result {
        if protected(path) {
            git_metadata_changed |= base.get(path).is_none_or(|original| {
                original.kind != output.kind
                    || original.mode != output.mode
                    || original.sha256 != output.sha256
            });
            continue;
        }
        if path.is_empty() {
            continue;
        }
        match base.get(path) {
            None if output.kind == BaseKind::Directory => {
                new_directories.push(ImportChange::CreateDirectory {
                    path: path.clone(),
                    mode: output.mode,
                });
            }
            None => {
                copied_bytes = copied_bytes
                    .checked_add(output.size)
                    .ok_or(ImportPlanError::Limit)?;
                put_files.push(ImportChange::PutFile {
                    path: path.clone(),
                    base: None,
                    output: (*output).clone(),
                });
            }
            Some(original) if original.kind != output.kind => {
                return Err(ImportPlanError::TypeChange(path.clone()));
            }
            Some(original) if output.kind == BaseKind::Directory => {
                if original.mode != output.mode {
                    return Err(ImportPlanError::DirectoryMode(path.clone()));
                }
            }
            Some(original) if original.sha256 != output.sha256 || original.mode != output.mode => {
                verified_bytes = verified_bytes
                    .checked_add(original.size)
                    .ok_or(ImportPlanError::Limit)?;
                copied_bytes = copied_bytes
                    .checked_add(output.size)
                    .ok_or(ImportPlanError::Limit)?;
                put_files.push(ImportChange::PutFile {
                    path: path.clone(),
                    base: Some((*original).clone()),
                    output: (*output).clone(),
                });
            }
            Some(_) => {}
        }
    }
    for (path, original) in &base {
        if protected(path) {
            git_metadata_changed |= !result.contains_key(path);
            continue;
        }
        if path.is_empty() || result.contains_key(path) {
            continue;
        }
        if original.kind == BaseKind::Directory {
            removed_directories.push(ImportChange::RemoveDirectory {
                path: path.clone(),
                base: (*original).clone(),
            });
        } else {
            verified_bytes = verified_bytes
                .checked_add(original.size)
                .ok_or(ImportPlanError::Limit)?;
            removed_files.push(ImportChange::RemoveFile {
                path: path.clone(),
                base: (*original).clone(),
            });
        }
    }
    let total =
        new_directories.len() + put_files.len() + removed_files.len() + removed_directories.len();
    if total > MAX_IMPORT_CHANGES
        || copied_bytes > MAX_IMPORT_BYTES
        || verified_bytes > MAX_IMPORT_BYTES
    {
        return Err(ImportPlanError::Limit);
    }
    new_directories.sort_by(|a, b| {
        depth(a.path())
            .cmp(&depth(b.path()))
            .then_with(|| a.path().cmp(b.path()))
    });
    put_files.sort_by(|a, b| a.path().cmp(b.path()));
    removed_files.sort_by(|a, b| {
        depth(b.path())
            .cmp(&depth(a.path()))
            .then_with(|| a.path().cmp(b.path()))
    });
    removed_directories.sort_by(|a, b| {
        depth(b.path())
            .cmp(&depth(a.path()))
            .then_with(|| a.path().cmp(b.path()))
    });
    new_directories.extend(put_files);
    new_directories.extend(removed_files);
    new_directories.extend(removed_directories);
    if !serde_json::to_vec(
        &new_directories
            .iter()
            .map(ImportChange::path)
            .collect::<Vec<_>>(),
    )
    .is_ok_and(|encoded| encoded.len() <= MAX_IMPORT_EFFECT_BYTES)
    {
        return Err(ImportPlanError::Limit);
    }
    Ok(ImportPlan {
        changes: new_directories,
        copied_bytes,
        git_metadata_changed,
    })
}

fn index(entries: &[BaseEntry]) -> Result<BTreeMap<String, &BaseEntry>, ImportPlanError> {
    let mut map = BTreeMap::new();
    for entry in entries {
        if (entry.path.is_empty() && entry.kind != BaseKind::Directory)
            || entry.path.split('/').any(|part| {
                (!entry.path.is_empty() && part.is_empty()) || part == "." || part == ".."
            })
            || entry.path.contains('\0')
            || map.insert(entry.path.clone(), entry).is_some()
        {
            return Err(ImportPlanError::InvalidManifest);
        }
    }
    if !map.contains_key("") {
        return Err(ImportPlanError::InvalidManifest);
    }
    Ok(map)
}

fn protected(path: &str) -> bool {
    path.split('/').any(|component| component == ".git")
}

fn depth(path: &str) -> usize {
    path.bytes().filter(|byte| *byte == b'/').count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_exec::snapshot::{SnapshotLimits, build_snapshot};
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };

    struct Fixture {
        root: PathBuf,
        live: PathBuf,
        private: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("ion-import-{}", uuid::Uuid::now_v7()));
            let live = root.join("live");
            let private = root.join("private");
            fs::create_dir(&root).unwrap();
            fs::create_dir(&live).unwrap();
            fs::create_dir(&private).unwrap();
            Self {
                root,
                live,
                private,
            }
        }

        fn limits() -> SnapshotLimits {
            SnapshotLimits {
                max_entries: 32,
                max_bytes: 4096,
                max_depth: 5,
                max_path_bytes: 512,
                max_ignored_paths: 8,
                max_ignored_bytes: 512,
            }
        }

        fn snapshot(&self, source: &Path, leaf: &str) -> super::super::snapshot::Snapshot {
            build_snapshot(
                &File::open(source).unwrap(),
                &File::open(&self.private).unwrap(),
                leaf,
                Self::limits(),
                &[],
            )
            .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for dir in [
                self.live.join("created"),
                self.private.join("command/created"),
                self.private.join("output/created"),
            ] {
                if dir.is_dir() {
                    let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
                }
            }
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    fn entry(path: &str, kind: BaseKind, mode: u32, bytes: &str) -> BaseEntry {
        BaseEntry {
            path: path.to_owned(),
            kind,
            mode,
            size: bytes.len() as u64,
            sha256: (kind == BaseKind::RegularFile).then(|| bytes.to_owned()),
            device: 1,
            inode: 1,
            modified_seconds: 0,
            modified_nanoseconds: 0,
            changed_seconds: 0,
            changed_nanoseconds: 0,
        }
    }

    fn directory(path: &str) -> BaseEntry {
        entry(path, BaseKind::Directory, 0o755, "")
    }

    fn file(path: &str, bytes: &str) -> BaseEntry {
        entry(path, BaseKind::RegularFile, 0o644, bytes)
    }

    #[test]
    fn plans_create_replace_remove_and_directory_order() {
        let before = [
            directory(""),
            directory("old"),
            file("old/gone", "x"),
            file("edit", "a"),
        ];
        let after = [
            directory(""),
            directory("new"),
            file("new/add", "xy"),
            file("edit", "b"),
        ];
        let plan = plan_import(&before, &after).unwrap();
        assert_eq!(plan.copied_bytes, 3);
        assert_eq!(
            plan.changes
                .iter()
                .map(ImportChange::path)
                .collect::<Vec<_>>(),
            ["new", "edit", "new/add", "old/gone", "old"]
        );
        assert!(matches!(
            plan.changes[0],
            ImportChange::CreateDirectory { .. }
        ));
        assert!(matches!(
            plan.changes[4],
            ImportChange::RemoveDirectory { .. }
        ));
    }

    #[test]
    fn never_imports_git_metadata() {
        let before = [
            directory(""),
            directory("project"),
            directory("project/.git"),
            file("project/.git/HEAD", "old"),
        ];
        let after = [
            directory(""),
            directory("project"),
            directory("project/.git"),
            file("project/.git/HEAD", "new"),
        ];
        let plan = plan_import(&before, &after).unwrap();
        assert!(plan.changes.is_empty());
        assert!(plan.git_metadata_changed);
    }

    #[test]
    fn publishes_private_file_changes_and_directory_lifecycle() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.live.join("old")).unwrap();
        fs::write(fixture.live.join("old/gone"), b"remove").unwrap();
        fs::write(fixture.live.join("edit"), b"old").unwrap();
        let base = fixture.snapshot(&fixture.live, "command");
        let command = fixture.private.join("command");
        fs::remove_file(command.join("old/gone")).unwrap();
        fs::remove_dir(command.join("old")).unwrap();
        fs::write(command.join("edit"), b"new").unwrap();
        fs::create_dir(command.join("created")).unwrap();
        fs::write(command.join("created/add"), b"content").unwrap();
        fs::set_permissions(command.join("created"), fs::Permissions::from_mode(0o555)).unwrap();
        let output = fixture.snapshot(&command, "output");
        let plan = plan_import(&base.manifest, &output.manifest).unwrap();
        let applied = apply_import(
            &plan,
            &base.manifest,
            &File::open(&fixture.live).unwrap(),
            &output.root,
            &File::open(&fixture.private).unwrap(),
        )
        .unwrap();
        assert_eq!(
            applied,
            ["created", "created/add", "edit", "old/gone", "old"]
        );
        assert_eq!(fs::read(fixture.live.join("edit")).unwrap(), b"new");
        assert_eq!(
            fs::read(fixture.live.join("created/add")).unwrap(),
            b"content"
        );
        assert_eq!(
            fs::metadata(fixture.live.join("created")).unwrap().mode() & 0o777,
            0o555
        );
        assert!(!fixture.live.join("old").exists());
    }

    #[test]
    fn stale_live_base_refuses_entire_import() {
        let fixture = Fixture::new();
        fs::write(fixture.live.join("edit"), b"old").unwrap();
        let base = fixture.snapshot(&fixture.live, "command");
        let command = fixture.private.join("command");
        fs::write(command.join("edit"), b"new").unwrap();
        fs::write(command.join("add"), b"added").unwrap();
        let output = fixture.snapshot(&command, "output");
        let plan = plan_import(&base.manifest, &output.manifest).unwrap();
        fs::write(fixture.live.join("edit"), b"external").unwrap();
        let result = apply_import(
            &plan,
            &base.manifest,
            &File::open(&fixture.live).unwrap(),
            &output.root,
            &File::open(&fixture.private).unwrap(),
        );
        assert!(matches!(result, Err(ImportFailure::Preflight(_))));
        assert!(!fixture.live.join("add").exists());
        assert_eq!(fs::read(fixture.live.join("edit")).unwrap(), b"external");
    }

    #[test]
    fn rejects_type_change_before_any_import() {
        let before = [directory(""), file("item", "old")];
        let after = [directory(""), directory("item")];
        assert_eq!(
            plan_import(&before, &after),
            Err(ImportPlanError::TypeChange("item".into()))
        );
    }

    #[test]
    fn rejects_duplicate_manifest_paths() {
        let before = [directory(""), file("x", "1"), file("x", "2")];
        assert_eq!(
            plan_import(&before, &[directory("")]),
            Err(ImportPlanError::InvalidManifest)
        );
    }
}
