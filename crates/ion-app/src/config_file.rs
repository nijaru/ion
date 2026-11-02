//! Shared file mechanics for Host configuration; validation and trust stay with callers.

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy)]
pub(crate) enum Visibility {
    Private,
    Shared,
}

impl Visibility {
    fn directory_mode(self) -> u32 {
        match self {
            Self::Private => 0o700,
            Self::Shared => 0o755,
        }
    }

    fn file_mode(self) -> u32 {
        match self {
            Self::Private => 0o600,
            Self::Shared => 0o644,
        }
    }
}

/// Hold across the whole read/modify/publish operation. The separate lock inode
/// survives atomic replacement of the configuration file.
pub(crate) fn lock(path: &Path, visibility: Visibility) -> Result<File> {
    let parent = path.parent().context("config lock has no parent")?;
    DirBuilder::new()
        .recursive(true)
        .mode(visibility.directory_mode())
        .create(parent)?;
    let mode = match visibility {
        Visibility::Private => 0o600,
        Visibility::Shared => 0o666,
    };
    let lock = File::from(rustix::fs::open(
        path,
        rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::from_raw_mode(mode),
    )?);
    ensure!(
        lock.metadata()?.is_file(),
        "config lock is not a regular file"
    );
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
    Ok(lock)
}

pub(crate) fn write_json<T: Serialize>(
    path: &Path,
    value: &T,
    visibility: Visibility,
) -> Result<()> {
    write(path, &serde_json::to_vec_pretty(value)?, visibility)
}

pub(crate) fn write(path: &Path, bytes: &[u8], visibility: Visibility) -> Result<()> {
    let parent = path.parent().context("config path has no parent")?;
    DirBuilder::new()
        .recursive(true)
        .mode(visibility.directory_mode())
        .create(parent)?;
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow::anyhow!("cannot generate config temp name: {error}"))?;
    let temp = path.with_extension(format!(
        "{}.{:016x}.tmp",
        std::process::id(),
        u64::from_ne_bytes(nonce)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(visibility.file_mode())
        .open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| {
                format!(
                    "published {}, but syncing its directory failed",
                    path.display()
                )
            })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub(crate) fn project_root(cwd: &Path) -> Result<PathBuf> {
    let cwd = cwd
        .canonicalize()
        .with_context(|| format!("cannot resolve working directory {}", cwd.display()))?;
    for ancestor in cwd.ancestors() {
        if ancestor.join(".git").exists() {
            return Ok(ancestor.to_owned());
        }
    }
    Ok(cwd)
}

pub(crate) fn content_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
