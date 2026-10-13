//! Verified regular-file reads and coordinated native file replacement.
use std::{
    fs::{self, File, OpenOptions, Permissions, TryLockError},
    io::{self, Read, Write},
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::Duration,
};

use rustix::fs::{Mode, OFlags, open};
use tokio_util::sync::CancellationToken;

/// Serializes cooperating Ion mutations, not external editors or `exec` writes.
/// A fresh parent-directory descriptor survives target-inode replacement and
/// coordinates separate instances/processes without leaving lock files behind.
pub(crate) struct FileMutation {
    target: PathBuf,
    parent: File,
}

impl FileMutation {
    pub(crate) fn open(
        path: &Path,
        create_parents: bool,
        stop: &CancellationToken,
    ) -> io::Result<Self> {
        let cancelled =
            || io::Error::new(io::ErrorKind::Interrupted, "cancelled before file mutation");
        if stop.is_cancelled() {
            return Err(cancelled());
        }
        let target = match fs::symlink_metadata(path) {
            Ok(_) => fs::canonicalize(path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A missing leaf is creatable; a dangling symlink above takes
                // the canonicalization error instead of replacing the link.
                if path.as_os_str().as_bytes().ends_with(b"/") {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "file path ends in a directory separator",
                    ));
                }
                let parent = path.parent().ok_or_else(|| io::Error::other("no parent"))?;
                let leaf = path
                    .file_name()
                    .ok_or_else(|| io::Error::other("no file name"))?;
                if create_parents {
                    fs::create_dir_all(parent)?;
                }
                fs::canonicalize(parent)?.join(leaf)
            }
            Err(error) => return Err(error),
        };
        let parent = File::open(
            target
                .parent()
                .ok_or_else(|| io::Error::other("no parent"))?,
        )?;
        if !parent.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file parent is not a directory",
            ));
        }
        loop {
            // Parent creation above can itself be an observed effect. Stop
            // prevents target mutation, not a claim of filesystem rollback.
            if stop.is_cancelled() {
                return Err(cancelled());
            }
            match parent.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
        if stop.is_cancelled() {
            return Err(cancelled());
        }
        Ok(Self { target, parent })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.target
    }

    pub(crate) fn ensure_writable(&self) -> io::Result<()> {
        // Atomic rename must not bypass the target's effective write access.
        let file = File::from(open(
            &self.target,
            OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("not a regular file"));
        }
        Ok(())
    }

    pub(crate) fn replace(
        &self,
        content: &[u8],
        permissions: Option<Permissions>,
    ) -> io::Result<()> {
        let temporary = self
            .target
            .parent()
            .expect("validated parent")
            .join(format!(
                ".ion-{}-{}.tmp",
                std::process::id(),
                uuid::Uuid::now_v7()
            ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            if let Some(permissions) = permissions {
                file.set_permissions(permissions)?;
            }
            file.write_all(content)?;
            file.sync_all()?;
            fs::rename(&temporary, &self.target)?;
            // Once renamed, finish publication even if stop arrives: report
            // the actual effect rather than implying it was cancelled away.
            self.parent.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }
}

pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    // Checking the path before a blocking open races with replacement. Open
    // nonblocking, then validate the same descriptor we will actually read.
    let file = File::from(open(
        path,
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// Read at most `limit` bytes from a verified regular file or its symlink.
/// Special files are rejected without a blocking open; oversized data is an error.
pub fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let bound = u64::try_from(limit)
        .ok()
        .and_then(|limit| limit.checked_add(1))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "read byte bound overflow"))?;
    let file = open_regular(path)?;
    let too_large = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("file exceeds {limit}-byte read bound"),
        )
    };
    if file.metadata()?.len() >= bound {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(bound).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(too_large());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn bounded_reads_follow_regular_symlinks_and_reject_special_files() {
        let root = std::env::temp_dir().join(format!("ion-file-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("text"), "abc").unwrap();
        symlink("text", root.join("link")).unwrap();
        assert_eq!(read_bounded(&root.join("link"), 3).unwrap(), b"abc");
        assert!(read_bounded(&root.join("text"), 2).is_err());
        assert!(open_regular(&root).is_err());
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.join("pipe"))
                .status()
                .unwrap()
                .success()
        );
        assert!(open_regular(&root.join("pipe")).is_err());
        symlink("pipe", root.join("pipe-link")).unwrap();
        assert!(read_bounded(&root.join("pipe-link"), 1024).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
