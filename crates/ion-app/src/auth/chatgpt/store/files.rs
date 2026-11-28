//! File authority for a local, owner-only profile directory. All children are
//! opened relative to its verified descriptor, never via an account path.
use std::{
    fs::{File, TryLockError},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    time::Duration,
};

use rustix::{
    fs::{AtFlags, Mode, OFlags, mkdirat, open, openat, renameat, unlinkat},
    io::Errno,
    process::geteuid,
};
use serde::{Serialize, de::DeserializeOwned};
use tokio_util::sync::CancellationToken;

use super::{StoreError, check_stop};

const LIMIT: usize = 128 * 1024;
const SAFE_OPEN: OFlags = OFlags::NOFOLLOW
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
const PRIVATE_FILE: Mode = Mode::RUSR.union(Mode::WUSR);
const PRIVATE_DIR: Mode = PRIVATE_FILE.union(Mode::XUSR);

pub(super) struct RecordFile<'a> {
    directory: &'a File,
    name: String,
    scratch: String,
    // Never replace/unlink the inode used as the lock. Dropping the owned
    // descriptor releases it, including on cancellation and process exit.
    _lease: File,
}

impl<'a> RecordFile<'a> {
    pub(super) async fn lock(
        directory: &'a File,
        name: &str,
        stop: &CancellationToken,
    ) -> Result<Self, StoreError> {
        check_stop(stop)?;
        let lease = File::from(
            openat(
                directory,
                format!("{name}.lock"),
                SAFE_OPEN | OFlags::RDWR | OFlags::CREATE,
                PRIVATE_FILE,
            )
            .map_err(|_| StoreError::Storage)?,
        );
        check_file(&lease)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        loop {
            check_stop(stop)?;
            match lease.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                    tokio::select! {
                        biased;
                        () = stop.cancelled() => return Err(StoreError::Cancelled),
                        () = tokio::time::sleep(Duration::from_millis(20)) => {}
                    }
                }
                Err(TryLockError::WouldBlock) => return Err(StoreError::Busy),
                Err(TryLockError::Error(_)) => return Err(StoreError::Storage),
            }
        }
        check_stop(stop)?;
        let scratch = format!("{name}.next");
        // This name is reserved under the same lease. If a previous process
        // died before rename, its unpublished snapshot must never be read.
        match unlinkat(directory, &scratch, AtFlags::empty()) {
            Ok(()) | Err(Errno::NOENT) => {}
            Err(_) => return Err(StoreError::Storage),
        }
        // Also closes the crash window after a previous rename whose parent
        // sync was not acknowledged. Do not use visible state before this.
        directory.sync_all().map_err(|_| StoreError::Storage)?;
        Ok(Self {
            directory,
            name: name.to_owned(),
            scratch,
            _lease: lease,
        })
    }

    pub(super) fn read<T: DeserializeOwned>(&self) -> Result<Option<T>, StoreError> {
        let file = match openat(
            self.directory,
            &self.name,
            SAFE_OPEN | OFlags::RDONLY,
            Mode::empty(),
        ) {
            Ok(fd) => File::from(fd),
            Err(Errno::NOENT) => return Ok(None),
            Err(_) => return Err(StoreError::Storage),
        };
        check_file(&file)?;
        let mut content = Vec::new();
        file.take(LIMIT as u64 + 1)
            .read_to_end(&mut content)
            .map_err(|_| StoreError::Storage)?;
        if content.len() > LIMIT {
            return Err(StoreError::Storage);
        }
        serde_json::from_slice(&content)
            .map(Some)
            .map_err(|_| StoreError::Storage)
    }

    pub(super) fn write(&self, record: &impl Serialize) -> Result<(), StoreError> {
        let content = serde_json::to_vec(record).map_err(|_| StoreError::Storage)?;
        if content.len() > LIMIT {
            return Err(StoreError::Storage);
        }
        let mut file = File::from(
            openat(
                self.directory,
                &self.scratch,
                SAFE_OPEN | OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
                PRIVATE_FILE,
            )
            .map_err(|_| StoreError::Storage)?,
        );
        let result = (|| {
            file.write_all(&content).map_err(|_| StoreError::Storage)?;
            file.sync_all().map_err(|_| StoreError::Storage)?;
            renameat(self.directory, &self.scratch, self.directory, &self.name)
                .map_err(|_| StoreError::Storage)?;
            self.directory
                .sync_all()
                .map_err(|_| StoreError::PublicationUnknown)
        })();
        if result.is_err() {
            let _ = unlinkat(self.directory, &self.scratch, AtFlags::empty());
        }
        result
    }
}

pub(super) fn open_root(path: &Path) -> Result<File, StoreError> {
    if !path.is_absolute() {
        return Err(StoreError::Storage);
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(StoreError::Storage)?;
    let leaf = path.file_name().ok_or(StoreError::Storage)?;
    // The host chooses a trusted existing parent, which may include host
    // symlinks (e.g. macOS temp dirs). The vault leaf itself is never followed.
    let parent = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| StoreError::Storage)?;
    let flags = SAFE_OPEN | OFlags::RDONLY | OFlags::DIRECTORY;
    let directory = match openat(&parent, leaf, flags, Mode::empty()) {
        Ok(directory) => directory,
        Err(Errno::NOENT) => {
            match mkdirat(&parent, leaf, PRIVATE_DIR) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(_) => return Err(StoreError::Storage),
            }
            openat(&parent, leaf, flags, Mode::empty()).map_err(|_| StoreError::Storage)?
        }
        Err(_) => return Err(StoreError::Storage),
    };
    let directory = File::from(directory);
    let metadata = directory.metadata().map_err(|_| StoreError::Storage)?;
    if !metadata.is_dir() || metadata.uid() != geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(StoreError::Storage);
    }
    // The containing directory entry must be durable too, including after a
    // concurrent first opener made the directory but did not sync it yet.
    File::from(parent)
        .sync_all()
        .map_err(|_| StoreError::Storage)?;
    Ok(directory)
}

fn check_file(file: &File) -> Result<(), StoreError> {
    let metadata = file.metadata().map_err(|_| StoreError::Storage)?;
    if !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(StoreError::Storage);
    }
    Ok(())
}
