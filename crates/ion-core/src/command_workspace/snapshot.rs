//! Bounded private workspace snapshot for a single command attempt.
//!
//! The caller supplies a pinned workspace root and a host-private destination
//! parent outside that workspace. No command may see the destination until this
//! function succeeds. Source traversal uses no-follow descriptors; included
//! symlinks, hard-linked files, and mount crossings are unsupported. Ignored
//! paths and special files are omitted with explicit path evidence. These
//! checks resist path replacement but cannot make a concurrently writable source
//! namespace atomic. The host must hold its workspace claim and later compare
//! the manifest against the live base before importing any command changes.

use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
};

use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, fchmod, fstat, mkdirat, openat, statat, unlinkat,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const COPY_CHUNK: usize = 64 * 1024;

/// Per-attempt limits supplied by the host. Every field must be nonzero.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SnapshotLimits {
    pub max_entries: usize,
    pub max_bytes: u64,
    pub max_depth: usize,
    pub max_path_bytes: usize,
    pub max_ignored_paths: usize,
    pub max_ignored_bytes: usize,
}

/// A source fact captured before the command can see its private workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BaseEntry {
    pub path: String,
    pub kind: BaseKind,
    pub mode: u32,
    pub size: u64,
    pub sha256: Option<String>,
    pub device: u64,
    pub inode: u64,
    pub modified_seconds: i64,
    pub modified_nanoseconds: i64,
    pub changed_seconds: i64,
    pub changed_nanoseconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum BaseKind {
    Directory,
    RegularFile,
}

/// A path omitted from the private view. An ignored directory record covers
/// its entire subtree; omitted special entries have no copied descendants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OmittedEntry {
    pub path: String,
    pub reason: OmissionReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum OmissionReason {
    Ignored,
    Fifo,
    Socket,
    CharacterDevice,
    BlockDevice,
}

/// The completed tree remains under the caller's private parent at `leaf`.
/// The caller owns its eventual cleanup and must not expose it before success.
#[derive(Debug)]
pub(crate) struct Snapshot {
    pub root: File,
    pub manifest: Vec<BaseEntry>,
    pub omitted: Vec<OmittedEntry>,
    pub total_bytes: u64,
}

#[derive(Debug, Error)]
pub(crate) enum SnapshotError {
    #[error("invalid snapshot limits or destination name")]
    InvalidInput,
    #[error("snapshot limit exceeded at {0}")]
    Limit(String),
    #[error("unsupported workspace entry at {0}")]
    Unsupported(String),
    #[error("workspace changed while snapshotting {0}")]
    Changed(String),
    #[error("snapshot filesystem operation failed at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("snapshot failed and private destination cleanup also failed: {build}; {cleanup}")]
    Cleanup {
        build: Box<Self>,
        cleanup: std::io::Error,
    },
}

fn io_error(path: &str, source: impl Into<std::io::Error>) -> SnapshotError {
    SnapshotError::Io {
        path: path.to_owned(),
        source: source.into(),
    }
}

/// Build a fresh private tree. `private_parent` must be outside the workspace,
/// inaccessible to the command and protected from other writers. `leaf` must
/// be an unused single path component; existing destinations are never reused.
/// `ignored_paths` is an exact, bounded set of relative paths supplied by the
/// host's Git ignore enumeration, rather than patterns interpreted here.
pub(crate) fn build_snapshot(
    source_root: &File,
    private_parent: &File,
    leaf: &str,
    limits: SnapshotLimits,
    ignored_paths: &[String],
) -> Result<Snapshot, SnapshotError> {
    build_with_hook(
        source_root,
        private_parent,
        leaf,
        limits,
        ignored_paths,
        |_, _| {},
    )
}

#[derive(Clone, Copy)]
enum HookPoint {
    AfterStat,
    AfterRead,
}

fn build_with_hook(
    source_root: &File,
    private_parent: &File,
    leaf: &str,
    limits: SnapshotLimits,
    ignored_paths: &[String],
    mut hook: impl FnMut(HookPoint, &str),
) -> Result<Snapshot, SnapshotError> {
    if limits.max_entries == 0
        || limits.max_bytes == 0
        || limits.max_depth == 0
        || limits.max_path_bytes == 0
        || !valid_component(leaf)
        || leaf.len() > limits.max_path_bytes
    {
        return Err(SnapshotError::InvalidInput);
    }
    if ignored_paths.len() > limits.max_ignored_paths {
        return Err(SnapshotError::Limit("ignore paths".to_owned()));
    }
    let mut ignored_bytes = 0_usize;
    let mut ignored = BTreeSet::new();
    for path in ignored_paths {
        if path.len() > limits.max_path_bytes
            || !valid_relative_path(path)
            || !ignored.insert(path.as_str())
        {
            return Err(SnapshotError::InvalidInput);
        }
        ignored_bytes = ignored_bytes
            .checked_add(path.len())
            .ok_or_else(|| SnapshotError::Limit("ignore paths".to_owned()))?;
        if ignored_bytes > limits.max_ignored_bytes {
            return Err(SnapshotError::Limit("ignore paths".to_owned()));
        }
    }
    let source_meta = source_root.metadata().map_err(|e| io_error(".", e))?;
    if !source_meta.is_dir() || source_meta.mode() & 0o7000 != 0 {
        return Err(SnapshotError::Unsupported(".".to_owned()));
    }
    let parent_meta = private_parent.metadata().map_err(|e| io_error(leaf, e))?;
    if !parent_meta.is_dir() || parent_is_inside_source(&source_meta, private_parent)? {
        return Err(SnapshotError::InvalidInput);
    }
    mkdirat(private_parent, leaf, Mode::from_raw_mode(0o700)).map_err(|e| io_error(leaf, e))?;
    let result = (|| {
        let destination = File::from(
            openat(
                private_parent,
                leaf,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| io_error(leaf, e))?,
        );
        let mut builder = Builder {
            limits,
            source_device: source_meta.dev(),
            manifest: Vec::new(),
            omitted: Vec::new(),
            total_bytes: 0,
            ignored,
            hook: &mut hook,
        };
        builder.push_entry("", BaseKind::Directory, &source_meta, None)?;
        builder.copy_directory(source_root, &destination, "", 0)?;
        if fingerprint(&source_root.metadata().map_err(|e| io_error(".", e))?)
            != fingerprint(&source_meta)
        {
            return Err(SnapshotError::Changed(".".to_owned()));
        }
        destination.sync_all().map_err(|e| io_error(leaf, e))?;
        Ok(Snapshot {
            root: destination,
            manifest: builder.manifest,
            omitted: builder.omitted,
            total_bytes: builder.total_bytes,
        })
    })();
    if let Err(build) = result {
        if let Err(cleanup) = cleanup_private_tree(private_parent, leaf) {
            return Err(SnapshotError::Cleanup {
                build: Box::new(build),
                cleanup,
            });
        }
        return Err(build);
    }
    result
}

struct Builder<'a, F> {
    limits: SnapshotLimits,
    source_device: u64,
    manifest: Vec<BaseEntry>,
    omitted: Vec<OmittedEntry>,
    total_bytes: u64,
    ignored: BTreeSet<&'a str>,
    hook: &'a mut F,
}

impl<F: FnMut(HookPoint, &str)> Builder<'_, F> {
    fn push_entry(
        &mut self,
        path: &str,
        kind: BaseKind,
        meta: &std::fs::Metadata,
        sha256: Option<String>,
    ) -> Result<(), SnapshotError> {
        if self.manifest.len() + self.omitted.len() >= self.limits.max_entries {
            return Err(SnapshotError::Limit(path.to_owned()));
        }
        self.manifest.push(BaseEntry {
            path: path.to_owned(),
            kind,
            mode: meta.mode() & 0o777,
            size: if kind == BaseKind::RegularFile {
                meta.len()
            } else {
                0
            },
            sha256,
            device: meta.dev(),
            inode: meta.ino(),
            modified_seconds: meta.mtime(),
            modified_nanoseconds: meta.mtime_nsec(),
            changed_seconds: meta.ctime(),
            changed_nanoseconds: meta.ctime_nsec(),
        });
        Ok(())
    }

    fn record_omission(&mut self, path: &str, reason: OmissionReason) -> Result<(), SnapshotError> {
        if self.manifest.len() + self.omitted.len() >= self.limits.max_entries {
            return Err(SnapshotError::Limit(path.to_owned()));
        }
        self.omitted.push(OmittedEntry {
            path: path.to_owned(),
            reason,
        });
        Ok(())
    }

    fn copy_directory(
        &mut self,
        source: &File,
        destination: &File,
        prefix: &str,
        depth: usize,
    ) -> Result<(), SnapshotError> {
        let start = source.metadata().map_err(|e| io_error(prefix, e))?;
        let mut names = Vec::new();
        for entry in Dir::read_from(source).map_err(|e| io_error(prefix, e))? {
            let entry = entry.map_err(|e| io_error(prefix, e))?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| SnapshotError::Unsupported(prefix.to_owned()))?;
            if !valid_component(name) {
                return Err(SnapshotError::Unsupported(prefix.to_owned()));
            }
            if names.len() >= self.limits.max_entries - self.manifest.len() - self.omitted.len() {
                return Err(SnapshotError::Limit(prefix.to_owned()));
            }
            names.push(name.to_owned());
        }
        names.sort_unstable();
        for name in names {
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if path.len() > self.limits.max_path_bytes {
                return Err(SnapshotError::Limit(path));
            }
            if self.ignored.contains(path.as_str()) {
                self.record_omission(&path, OmissionReason::Ignored)?;
                continue;
            }
            let before = statat(source, name.as_str(), AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|e| io_error(&path, e))?;
            (self.hook)(HookPoint::AfterStat, &path);
            match FileType::from_raw_mode(before.st_mode) {
                FileType::RegularFile => {
                    self.copy_file(source, destination, &name, &path, &before)?;
                }
                FileType::Directory => {
                    if depth >= self.limits.max_depth {
                        return Err(SnapshotError::Limit(path));
                    }
                    self.copy_child_directory(
                        source,
                        destination,
                        &name,
                        &path,
                        &before,
                        depth + 1,
                    )?;
                }
                FileType::Symlink | FileType::Unknown => {
                    return Err(SnapshotError::Unsupported(path));
                }
                FileType::Fifo => self.record_omission(&path, OmissionReason::Fifo)?,
                FileType::Socket => self.record_omission(&path, OmissionReason::Socket)?,
                FileType::CharacterDevice => {
                    self.record_omission(&path, OmissionReason::CharacterDevice)?;
                }
                FileType::BlockDevice => {
                    self.record_omission(&path, OmissionReason::BlockDevice)?;
                }
            }
        }
        if fingerprint(&source.metadata().map_err(|e| io_error(prefix, e))?) != fingerprint(&start)
        {
            return Err(SnapshotError::Changed(prefix.to_owned()));
        }
        Ok(())
    }

    fn copy_child_directory(
        &mut self,
        source: &File,
        destination: &File,
        name: &str,
        path: &str,
        before: &rustix::fs::Stat,
        depth: usize,
    ) -> Result<(), SnapshotError> {
        let child = File::from(
            openat(
                source,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| io_error(path, e))?,
        );
        let meta = child.metadata().map_err(|e| io_error(path, e))?;
        check_identity(before, &meta, self.source_device, path)?;
        if meta.mode() & 0o7000 != 0 {
            return Err(SnapshotError::Unsupported(path.to_owned()));
        }
        mkdirat(destination, name, Mode::from_raw_mode(0o700)).map_err(|e| io_error(path, e))?;
        let copied = File::from(
            openat(
                destination,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| io_error(path, e))?,
        );
        self.push_entry(path, BaseKind::Directory, &meta, None)?;
        self.copy_directory(&child, &copied, path, depth)?;
        check_path_unchanged(source, name, before, &child, path)?;
        fchmod(&copied, Mode::from_raw_mode((meta.mode() & 0o777) as _))
            .map_err(|e| io_error(path, e))?;
        copied.sync_all().map_err(|e| io_error(path, e))?;
        Ok(())
    }

    fn copy_file(
        &mut self,
        source: &File,
        destination: &File,
        name: &str,
        path: &str,
        before: &rustix::fs::Stat,
    ) -> Result<(), SnapshotError> {
        let file = File::from(
            openat(
                source,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| io_error(path, e))?,
        );
        if FileType::from_raw_mode(fstat(&file).map_err(|e| io_error(path, e))?.st_mode)
            != FileType::RegularFile
        {
            return Err(SnapshotError::Changed(path.to_owned()));
        }
        let meta = file.metadata().map_err(|e| io_error(path, e))?;
        check_identity(before, &meta, self.source_device, path)?;
        if meta.nlink() != 1 || meta.mode() & 0o7000 != 0 {
            return Err(SnapshotError::Unsupported(path.to_owned()));
        }
        let remaining = self.limits.max_bytes - self.total_bytes;
        if meta.len() > remaining {
            return Err(SnapshotError::Limit(path.to_owned()));
        }
        let mut copied = File::from(
            openat(
                destination,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|e| io_error(path, e))?,
        );
        let mut reader = &file;
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; COPY_CHUNK];
        let mut copied_bytes = 0_u64;
        loop {
            let count = reader.read(&mut buffer).map_err(|e| io_error(path, e))?;
            if count == 0 {
                break;
            }
            copied_bytes = copied_bytes
                .checked_add(count as u64)
                .ok_or_else(|| SnapshotError::Limit(path.to_owned()))?;
            if copied_bytes > meta.len() || copied_bytes > remaining {
                return Err(SnapshotError::Changed(path.to_owned()));
            }
            digest.update(&buffer[..count]);
            copied
                .write_all(&buffer[..count])
                .map_err(|e| io_error(path, e))?;
        }
        (self.hook)(HookPoint::AfterRead, path);
        if copied_bytes != meta.len()
            || fingerprint(&file.metadata().map_err(|e| io_error(path, e))?) != fingerprint(&meta)
        {
            return Err(SnapshotError::Changed(path.to_owned()));
        }
        check_path_unchanged(source, name, before, &file, path)?;
        fchmod(&copied, Mode::from_raw_mode((meta.mode() & 0o777) as _))
            .map_err(|e| io_error(path, e))?;
        copied.sync_all().map_err(|e| io_error(path, e))?;
        self.total_bytes += copied_bytes;
        self.push_entry(
            path,
            BaseKind::RegularFile,
            &meta,
            Some(format!("{:x}", digest.finalize())),
        )?;
        Ok(())
    }
}

fn check_identity(
    before: &rustix::fs::Stat,
    meta: &std::fs::Metadata,
    source_device: u64,
    path: &str,
) -> Result<(), SnapshotError> {
    #[cfg(target_os = "linux")]
    let same_device = before.st_dev == meta.dev();
    #[cfg(not(target_os = "linux"))]
    let same_device = u64::try_from(before.st_dev).ok() == Some(meta.dev());
    if !same_device || before.st_ino != meta.ino() || meta.dev() != source_device {
        return Err(SnapshotError::Changed(path.to_owned()));
    }
    Ok(())
}

fn check_path_unchanged(
    parent: &File,
    name: &str,
    before: &rustix::fs::Stat,
    opened: &File,
    path: &str,
) -> Result<(), SnapshotError> {
    let after = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|_| SnapshotError::Changed(path.to_owned()))?;
    let opened = fstat(opened).map_err(|e| io_error(path, e))?;
    if after.st_dev != before.st_dev
        || after.st_ino != before.st_ino
        || opened.st_dev != before.st_dev
        || opened.st_ino != before.st_ino
    {
        return Err(SnapshotError::Changed(path.to_owned()));
    }
    Ok(())
}

fn fingerprint(meta: &std::fs::Metadata) -> (u64, u64, u64, u64, u32, i64, i64, i64, i64) {
    (
        meta.dev(),
        meta.ino(),
        meta.nlink(),
        meta.len(),
        meta.mode(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}

fn same_identity(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

fn parent_is_inside_source(
    source: &std::fs::Metadata,
    private_parent: &File,
) -> Result<bool, SnapshotError> {
    let mut current = private_parent
        .try_clone()
        .map_err(|e| io_error("private parent", e))?;
    // A directory cannot contain itself except through a mount or namespace
    // anomaly. Bound ancestry inspection rather than risking an endless walk.
    for _ in 0..4096 {
        let current_meta = current
            .metadata()
            .map_err(|e| io_error("private parent", e))?;
        if same_identity(source, &current_meta) {
            return Ok(true);
        }
        let parent = File::from(
            openat(
                &current,
                "..",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|e| io_error("private parent", e))?,
        );
        if same_identity(
            &current_meta,
            &parent
                .metadata()
                .map_err(|e| io_error("private parent", e))?,
        ) {
            return Ok(false);
        }
        current = parent;
    }
    Err(SnapshotError::InvalidInput)
}

fn valid_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty() && path.split('/').all(valid_component)
}

pub(crate) fn cleanup_private_tree(parent: &File, name: &str) -> std::io::Result<()> {
    let child = File::from(openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    fchmod(&child, Mode::from_raw_mode(0o700))?;
    for entry in Dir::read_from(&child)? {
        let entry = entry?;
        let component = entry.file_name();
        if component.to_bytes() == b"." || component.to_bytes() == b".." {
            continue;
        }
        let kind =
            FileType::from_raw_mode(statat(&child, component, AtFlags::SYMLINK_NOFOLLOW)?.st_mode);
        if kind == FileType::Directory {
            let name = component.to_str().map_err(std::io::Error::other)?;
            cleanup_private_tree(&child, name)?;
        } else {
            unlinkat(&child, component, AtFlags::empty())?;
        }
    }
    unlinkat(parent, name, AtFlags::REMOVEDIR)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::{
            fs::{PermissionsExt, symlink},
            net::UnixListener,
        },
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        base: PathBuf,
        source: PathBuf,
        private: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!(
                "ion-snapshot-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&base).unwrap();
            let source = base.join("source");
            let private = base.join("private");
            fs::create_dir(&source).unwrap();
            fs::create_dir(&private).unwrap();
            Self {
                base,
                source,
                private,
            }
        }

        fn build(&self, limits: SnapshotLimits) -> Result<Snapshot, SnapshotError> {
            self.build_ignoring(limits, &[])
        }

        fn build_ignoring(
            &self,
            limits: SnapshotLimits,
            ignored_paths: &[String],
        ) -> Result<Snapshot, SnapshotError> {
            build_snapshot(
                &File::open(&self.source).unwrap(),
                &File::open(&self.private).unwrap(),
                "attempt",
                limits,
                ignored_paths,
            )
        }

        fn limits() -> SnapshotLimits {
            SnapshotLimits {
                max_entries: 16,
                max_bytes: 128,
                max_depth: 3,
                max_path_bytes: 128,
                max_ignored_paths: 8,
                max_ignored_bytes: 256,
            }
        }

        fn destination(&self) -> PathBuf {
            self.private.join("attempt")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.base).unwrap();
        }
    }

    fn assert_absent(path: &Path) {
        assert!(
            !path.exists(),
            "partial destination was left behind: {}",
            path.display()
        );
    }

    #[test]
    fn copies_regular_files_and_executable_mode_with_base_digests() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.source.join("bin")).unwrap();
        fs::write(fixture.source.join("bin/run"), b"#!/bin/sh\necho ok\n").unwrap();
        fs::set_permissions(
            fixture.source.join("bin/run"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::write(fixture.source.join("empty"), b"").unwrap();
        let snapshot = fixture.build(Fixture::limits()).unwrap();
        assert_eq!(snapshot.total_bytes, 18);
        assert_eq!(
            fs::read(fixture.destination().join("bin/run")).unwrap(),
            b"#!/bin/sh\necho ok\n"
        );
        assert_eq!(
            fs::metadata(fixture.destination().join("bin/run"))
                .unwrap()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            snapshot
                .manifest
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["", "bin", "bin/run", "empty"]
        );
        let run = snapshot
            .manifest
            .iter()
            .find(|entry| entry.path == "bin/run")
            .unwrap();
        assert_eq!(
            run.sha256,
            Some(format!("{:x}", Sha256::digest(b"#!/bin/sh\necho ok\n")))
        );
        assert_eq!(run.mode, 0o755);
        assert!(snapshot.root.metadata().unwrap().is_dir());
    }

    #[test]
    fn rejects_included_symlinks_and_omits_special_entries() {
        let fixture = Fixture::new();
        let outside = fixture.base.join("outside");
        fs::write(&outside, b"secret").unwrap();
        symlink(&outside, fixture.source.join("link")).unwrap();
        assert!(
            matches!(fixture.build(Fixture::limits()), Err(SnapshotError::Unsupported(path)) if path == "link")
        );
        assert_absent(&fixture.destination());
        fs::remove_file(fixture.source.join("link")).unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(fixture.source.join("fifo"))
                .status()
                .unwrap()
                .success()
        );
        let _listener = UnixListener::bind(fixture.source.join("socket")).unwrap();
        let snapshot = fixture.build(Fixture::limits()).unwrap();
        assert_eq!(
            snapshot.omitted,
            [
                OmittedEntry {
                    path: "fifo".to_owned(),
                    reason: OmissionReason::Fifo
                },
                OmittedEntry {
                    path: "socket".to_owned(),
                    reason: OmissionReason::Socket
                },
            ]
        );
        assert_absent(&fixture.destination().join("fifo"));
        assert_absent(&fixture.destination().join("socket"));
        assert_eq!(fs::read(outside).unwrap(), b"secret");
    }

    #[test]
    fn rejects_hardlinks_and_bounded_resource_overruns() {
        let fixture = Fixture::new();
        fs::write(fixture.source.join("a"), b"abc").unwrap();
        fs::hard_link(fixture.source.join("a"), fixture.source.join("b")).unwrap();
        assert!(matches!(
            fixture.build(Fixture::limits()),
            Err(SnapshotError::Unsupported(_))
        ));
        assert_absent(&fixture.destination());
        fs::remove_file(fixture.source.join("b")).unwrap();
        let limits = SnapshotLimits {
            max_bytes: 2,
            ..Fixture::limits()
        };
        assert!(matches!(fixture.build(limits), Err(SnapshotError::Limit(path)) if path == "a"));
        assert_absent(&fixture.destination());
        let limits = SnapshotLimits {
            max_entries: 1,
            ..Fixture::limits()
        };
        assert!(matches!(
            fixture.build(limits),
            Err(SnapshotError::Limit(_))
        ));
        assert_absent(&fixture.destination());
    }

    #[test]
    fn exact_ignored_paths_omit_a_directory_subtree_and_a_symlink() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.source.join("generated")).unwrap();
        fs::write(fixture.source.join("generated/artifact"), b"generated").unwrap();
        symlink(
            fixture.base.join("outside"),
            fixture.source.join("generated/escape"),
        )
        .unwrap();
        symlink(
            fixture.base.join("outside"),
            fixture.source.join("ignored-link"),
        )
        .unwrap();
        fs::write(fixture.source.join("keep"), b"kept").unwrap();
        let ignored = ["generated".to_owned(), "ignored-link".to_owned()];
        let snapshot = fixture.build_ignoring(Fixture::limits(), &ignored).unwrap();
        assert_eq!(
            snapshot
                .manifest
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["", "keep"]
        );
        assert_eq!(
            snapshot.omitted,
            [
                OmittedEntry {
                    path: "generated".to_owned(),
                    reason: OmissionReason::Ignored
                },
                OmittedEntry {
                    path: "ignored-link".to_owned(),
                    reason: OmissionReason::Ignored
                },
            ]
        );
        assert_absent(&fixture.destination().join("generated"));
        assert_absent(&fixture.destination().join("ignored-link"));
        assert_eq!(
            fs::read(fixture.destination().join("keep")).unwrap(),
            b"kept"
        );
    }

    #[test]
    fn ignore_set_is_validated_and_bounded_before_destination_creation() {
        let fixture = Fixture::new();
        for invalid in ["../escape", "/absolute", "a//b", "a/./b"] {
            assert!(matches!(
                fixture.build_ignoring(Fixture::limits(), &[invalid.to_owned()]),
                Err(SnapshotError::InvalidInput)
            ));
            assert_absent(&fixture.destination());
        }
        assert!(matches!(
            fixture.build_ignoring(Fixture::limits(), &["same".to_owned(), "same".to_owned()]),
            Err(SnapshotError::InvalidInput)
        ));
        let limits = SnapshotLimits {
            max_ignored_paths: 1,
            ..Fixture::limits()
        };
        assert!(matches!(
            fixture.build_ignoring(limits, &["a".to_owned(), "b".to_owned()]),
            Err(SnapshotError::Limit(_))
        ));
        let limits = SnapshotLimits {
            max_ignored_bytes: 2,
            ..Fixture::limits()
        };
        assert!(matches!(
            fixture.build_ignoring(limits, &["long".to_owned()]),
            Err(SnapshotError::Limit(_))
        ));
        assert_absent(&fixture.destination());
    }

    #[test]
    fn special_entry_replacement_during_scan_invalidates_snapshot() {
        let fixture = Fixture::new();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(fixture.source.join("fifo"))
                .status()
                .unwrap()
                .success()
        );
        let result = build_with_hook(
            &File::open(&fixture.source).unwrap(),
            &File::open(&fixture.private).unwrap(),
            "attempt",
            Fixture::limits(),
            &[],
            |point, path| {
                if matches!(point, HookPoint::AfterStat) && path == "fifo" {
                    fs::remove_file(fixture.source.join("fifo")).unwrap();
                    fs::write(fixture.source.join("fifo"), b"replacement").unwrap();
                }
            },
        );
        assert!(matches!(result, Err(SnapshotError::Changed(path)) if path.is_empty()));
        assert_absent(&fixture.destination());
    }

    #[test]
    fn replacement_after_stat_cannot_redirect_the_read() {
        let fixture = Fixture::new();
        fs::write(fixture.source.join("file"), b"safe").unwrap();
        let outside = fixture.base.join("outside");
        fs::write(&outside, b"secret").unwrap();
        let mut swapped = false;
        let result = build_with_hook(
            &File::open(&fixture.source).unwrap(),
            &File::open(&fixture.private).unwrap(),
            "attempt",
            Fixture::limits(),
            &[],
            |point, path| {
                if matches!(point, HookPoint::AfterStat) && path == "file" {
                    fs::rename(fixture.source.join("file"), fixture.base.join("moved")).unwrap();
                    symlink(&outside, fixture.source.join("file")).unwrap();
                    swapped = true;
                }
            },
        );
        assert!(swapped);
        assert!(result.is_err());
        assert_absent(&fixture.destination());
        assert_eq!(fs::read(outside).unwrap(), b"secret");
    }

    #[test]
    fn content_changed_during_copy_is_rejected() {
        let fixture = Fixture::new();
        fs::write(fixture.source.join("file"), b"original").unwrap();
        let result = build_with_hook(
            &File::open(&fixture.source).unwrap(),
            &File::open(&fixture.private).unwrap(),
            "attempt",
            Fixture::limits(),
            &[],
            |point, path| {
                if matches!(point, HookPoint::AfterRead) && path == "file" {
                    fs::write(fixture.source.join("file"), b"changed!").unwrap();
                }
            },
        );
        assert!(matches!(result, Err(SnapshotError::Changed(path)) if path == "file"));
        assert_absent(&fixture.destination());
    }

    #[test]
    fn refuses_destination_inside_source_before_creating_it() {
        let fixture = Fixture::new();
        fs::create_dir(fixture.source.join("private")).unwrap();
        let result = build_snapshot(
            &File::open(&fixture.source).unwrap(),
            &File::open(fixture.source.join("private")).unwrap(),
            "attempt",
            Fixture::limits(),
            &[],
        );
        assert!(matches!(result, Err(SnapshotError::InvalidInput)));
        assert_absent(&fixture.source.join("private/attempt"));
    }
}
