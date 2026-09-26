//! Immutable out-of-line content references and a bounded Session-scoped blob store.
//!
//! The caller owns the host namespace and must keep it outside agent-writable workspace
//! state. A store is one Session namespace; references are meaningful only with that store.
//! Publication is synchronous and serialized per handle. One owner lock excludes other
//! BlobStore handles; the trusted host must still protect namespace ancestry from external
//! same-user programs.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::ContentDigest;

const IO_BUFFER_BYTES: usize = 64 * 1024;
const MAX_METADATA_BYTES: usize = 256;
const MAX_TEMP_NAME_ATTEMPTS: usize = 8;
const MAX_SCAN_ENTRIES: usize = 100_000;
const OBJECTS_DIRECTORY: &str = "objects";
const STAGING_DIRECTORY: &str = "staging";
const OWNER_LOCK: &str = ".blob-owner.lock";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    pub digest: ContentDigest,
    pub length: u64,
    pub media_type: Option<String>,
    pub encoding: Option<String>,
}

impl BlobRef {
    #[must_use]
    pub const fn new(digest: ContentDigest, length: u64) -> Self {
        Self {
            digest,
            length,
            media_type: None,
            encoding: None,
        }
    }
}

/// Hard limits for a single Session's blob namespace.
///
/// `max_spool_bytes` bounds one in-progress publication. Publications through one store
/// handle are serialized, so at most one staging file is actively written. The content
/// quota counts the logical lengths of immutable objects; the object-count quota also
/// bounds zero-length objects and namespace metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobStoreLimits {
    pub max_blob_bytes: u64,
    pub max_content_bytes: u64,
    pub max_spool_bytes: u64,
    pub max_page_bytes: u64,
    pub max_object_count: u64,
}

impl Default for BlobStoreLimits {
    fn default() -> Self {
        Self::new(
            64 * 1024 * 1024,
            1024 * 1024 * 1024,
            64 * 1024 * 1024,
            64 * 1024,
            100_000,
        )
    }
}

impl BlobStoreLimits {
    #[must_use]
    pub const fn new(
        max_blob_bytes: u64,
        max_content_bytes: u64,
        max_spool_bytes: u64,
        max_page_bytes: u64,
        max_object_count: u64,
    ) -> Self {
        Self {
            max_blob_bytes,
            max_content_bytes,
            max_spool_bytes,
            max_page_bytes,
            max_object_count,
        }
    }
}

/// A configured quota that prevented an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobQuota {
    BlobBytes,
    ContentBytes,
    SpoolBytes,
    PageBytes,
    ObjectCount,
    MetadataBytes,
}

impl std::fmt::Display for BlobQuota {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::BlobBytes => "per-blob bytes",
            Self::ContentBytes => "content bytes",
            Self::SpoolBytes => "spool bytes",
            Self::PageBytes => "read-page bytes",
            Self::ObjectCount => "object count",
            Self::MetadataBytes => "reference metadata bytes",
        };
        formatter.write_str(label)
    }
}

/// Current committed logical usage for a Session namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobStoreUsage {
    pub content_bytes: u64,
    pub object_count: u64,
}

/// Blob namespace, quota, integrity, or I/O failure.
#[derive(Debug, Error)]
pub enum BlobStoreError {
    #[error("blob store I/O during {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("blob {quota} quota exceeded (limit {limit})")]
    QuotaExceeded { quota: BlobQuota, limit: u64 },
    #[error("blob length overflow")]
    LengthOverflow,
    #[error("blob namespace is not a real directory: {0}")]
    InvalidNamespace(PathBuf),
    #[error("invalid blob reference: declared length {length} exceeds limit {limit}")]
    InvalidReferenceLength { length: u64, limit: u64 },
    #[error("invalid blob read range: offset {offset} exceeds length {length}")]
    InvalidRange { offset: u64, length: u64 },
    #[error("blob {digest} is missing")]
    Missing { digest: ContentDigest },
    #[error("blob {digest} is corrupt (expected {expected_length} bytes, found {actual_length:?})")]
    Corrupt {
        digest: ContentDigest,
        expected_length: u64,
        actual_length: Option<u64>,
    },
    #[error("blob namespace is already owned: {0}")]
    InUse(PathBuf),
    #[error("blob store state lock was poisoned")]
    StatePoisoned,
    #[error("could not reserve memory for bounded blob page: {0}")]
    Allocation(#[source] std::collections::TryReserveError),
}

#[derive(Debug)]
struct StoreState {
    objects: HashMap<String, u64>,
    content_bytes: u64,
}

/// Immutable content storage owned by one Session.
///
/// `open` hashes no existing object eagerly. Reads verify the complete object before
/// returning a requested page. `publish` returns only after file data and the object
/// directory entry have been synced; callers may then durably commit the returned ref.
/// Directory syncing is required, so platforms/filesystems that do not support it fail
/// closed. Durability remains conditional on the filesystem and host.
pub struct BlobStore {
    objects_dir: PathBuf,
    staging_dir: PathBuf,
    limits: BlobStoreLimits,
    state: Mutex<StoreState>,
    publish_lock: Mutex<()>,
    _owner: File,
}

impl BlobStore {
    /// Opens one existing host-owned Session namespace, creating its private store dirs.
    ///
    /// The namespace directory must already exist. An owner lock excludes another store
    /// handle until drop. Its parent/provisioning durability is the host's responsibility.
    pub fn open(
        session_namespace: impl AsRef<Path>,
        limits: BlobStoreLimits,
    ) -> Result<Self, BlobStoreError> {
        let requested_root = session_namespace.as_ref();
        let requested_metadata = fs::symlink_metadata(requested_root)
            .map_err(|source| io_error("inspect Session namespace", source))?;
        if requested_metadata.file_type().is_symlink() || !requested_metadata.is_dir() {
            return Err(BlobStoreError::InvalidNamespace(
                requested_root.to_path_buf(),
            ));
        }
        let root = fs::canonicalize(requested_root)
            .map_err(|source| io_error("resolve Session namespace", source))?;
        let owner = acquire_owner(&root)?;
        let objects_dir = root.join(OBJECTS_DIRECTORY);
        let staging_dir = root.join(STAGING_DIRECTORY);
        ensure_child_directory(&root, &objects_dir)?;
        ensure_child_directory(&root, &staging_dir)?;
        sync_directory(&root, "sync Session namespace directories")?;

        // Opening is inspection-only once the namespace exists. Crash-leftover
        // staging is reclaimed at the next explicit publication, not passive open.
        let state = scan_objects(&objects_dir)?;
        Ok(Self {
            objects_dir,
            staging_dir,
            limits,
            state: Mutex::new(state),
            publish_lock: Mutex::new(()),
            _owner: owner,
        })
    }

    /// Streams content into the store and returns a reference only after durable publish.
    pub fn publish<R: Read>(&self, source: R) -> Result<BlobRef, BlobStoreError> {
        self.publish_with_metadata(source, None, None)
    }

    /// Streams content into the store with optional descriptive metadata.
    pub fn publish_with_metadata<R: Read>(
        &self,
        mut source: R,
        media_type: Option<String>,
        encoding: Option<String>,
    ) -> Result<BlobRef, BlobStoreError> {
        if media_type
            .as_ref()
            .is_some_and(|s| s.len() > MAX_METADATA_BYTES)
            || encoding
                .as_ref()
                .is_some_and(|s| s.len() > MAX_METADATA_BYTES)
        {
            return Err(BlobStoreError::QuotaExceeded {
                quota: BlobQuota::MetadataBytes,
                limit: MAX_METADATA_BYTES as u64,
            });
        }
        let _publish_guard = self
            .publish_lock
            .lock()
            .map_err(|_| BlobStoreError::StatePoisoned)?;
        clean_staging(&self.staging_dir)?;
        self.publish_inner(&mut source, media_type, encoding, None)
    }

    /// Reads one bounded page after verifying the complete referenced object.
    ///
    /// A missing object and any length/digest mismatch are explicit errors. Bytes are not
    /// returned until verification succeeds, so corrupt partial content cannot masquerade
    /// as a valid page.
    pub fn read_range(
        &self,
        reference: &BlobRef,
        offset: u64,
        max_length: usize,
    ) -> Result<Vec<u8>, BlobStoreError> {
        if reference.length > self.limits.max_blob_bytes {
            return Err(BlobStoreError::InvalidReferenceLength {
                length: reference.length,
                limit: self.limits.max_blob_bytes,
            });
        }
        if offset > reference.length {
            return Err(BlobStoreError::InvalidRange {
                offset,
                length: reference.length,
            });
        }
        let requested = u64::try_from(max_length).map_err(|_| BlobStoreError::LengthOverflow)?;
        if requested > self.limits.max_page_bytes {
            return Err(BlobStoreError::QuotaExceeded {
                quota: BlobQuota::PageBytes,
                limit: self.limits.max_page_bytes,
            });
        }
        let available = reference.length - offset;
        let page_capacity = usize::try_from(available.min(requested))
            .map_err(|_| BlobStoreError::LengthOverflow)?;
        let mut page = Vec::new();
        page.try_reserve_exact(page_capacity)
            .map_err(BlobStoreError::Allocation)?;

        let path = self.object_path(reference.digest);
        let mut file = open_object(&path, reference.digest, reference.length)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; IO_BUFFER_BYTES];
        let mut actual_length = 0_u64;
        let page_end = offset.saturating_add(requested).min(reference.length);

        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|source| io_error("read blob", source))?;
            if read == 0 {
                break;
            }
            let next_length = checked_next_length(actual_length, read)?;
            if next_length > self.limits.max_blob_bytes || next_length > reference.length {
                return Err(BlobStoreError::Corrupt {
                    digest: reference.digest,
                    expected_length: reference.length,
                    actual_length: Some(next_length),
                });
            }
            let chunk_start = actual_length;
            let chunk_end = next_length;
            hasher.update(&buffer[..read]);

            let copy_start = offset.max(chunk_start);
            let copy_end = page_end.min(chunk_end);
            if copy_start < copy_end {
                let start = usize::try_from(copy_start - chunk_start)
                    .map_err(|_| BlobStoreError::LengthOverflow)?;
                let end = usize::try_from(copy_end - chunk_start)
                    .map_err(|_| BlobStoreError::LengthOverflow)?;
                page.extend_from_slice(&buffer[start..end]);
            }
            actual_length = next_length;
        }

        if actual_length != reference.length || !hash_matches(hasher, reference.digest) {
            return Err(BlobStoreError::Corrupt {
                digest: reference.digest,
                expected_length: reference.length,
                actual_length: Some(actual_length),
            });
        }
        Ok(page)
    }

    /// Returns the current committed logical usage.
    pub fn usage(&self) -> Result<BlobStoreUsage, BlobStoreError> {
        let state = self
            .state
            .lock()
            .map_err(|_| BlobStoreError::StatePoisoned)?;
        Ok(BlobStoreUsage {
            content_bytes: state.content_bytes,
            object_count: u64::try_from(state.objects.len())
                .map_err(|_| BlobStoreError::LengthOverflow)?,
        })
    }

    /// Only the Session owner calls this, under its exclusive publication/GC gate.
    /// Reachability is queried from committed indexed links, without hydrating history.
    pub(crate) fn collect_garbage<E: From<BlobStoreError>>(
        &self,
        mut reachable: impl FnMut(&str) -> Result<bool, E>,
    ) -> Result<BlobStoreUsage, E> {
        let _publish = self
            .publish_lock
            .lock()
            .map_err(|_| BlobStoreError::StatePoisoned)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| BlobStoreError::StatePoisoned)?;
        let mut removed = BlobStoreUsage {
            content_bytes: 0,
            object_count: 0,
        };
        // Bounded by the namespace object-count limit; do not scan all Session history.
        let names: Vec<_> = state.objects.keys().cloned().collect();
        for name in names {
            if reachable(&name)? {
                continue;
            }
            match fs::remove_file(self.objects_dir.join(&name)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(io_error("remove unreferenced blob", source).into()),
            }
            let length = state.objects.remove(&name).expect("scanned object");
            state.content_bytes -= length;
            removed.content_bytes += length;
            removed.object_count += 1;
            sync_directory(&self.objects_dir, "sync blob collection")?;
        }
        clean_staging(&self.staging_dir)?;
        Ok(removed)
    }

    fn publish_inner<R: Read>(
        &self,
        source: &mut R,
        media_type: Option<String>,
        encoding: Option<String>,
        #[cfg(test)] fail_after_bytes: Option<u64>,
        #[cfg(not(test))] _no_fault_injection: Option<()>,
    ) -> Result<BlobRef, BlobStoreError> {
        let mut staged = TempBlob::create(&self.staging_dir)?;
        let mut hasher = Sha256::new();
        let mut length = 0_u64;
        let mut buffer = [0_u8; IO_BUFFER_BYTES];

        loop {
            let read = source
                .read(&mut buffer)
                .map_err(|source| io_error("read publication source", source))?;
            if read == 0 {
                break;
            }

            #[cfg(test)]
            let write_length = match fail_after_bytes {
                Some(fail_after) if length >= fail_after => 0,
                Some(fail_after) => usize::try_from(
                    u64::try_from(read)
                        .map_err(|_| BlobStoreError::LengthOverflow)?
                        .min(fail_after - length),
                )
                .map_err(|_| BlobStoreError::LengthOverflow)?,
                None => read,
            };
            #[cfg(not(test))]
            let write_length = read;

            let next_length = checked_next_length(length, write_length)?;
            if next_length > self.limits.max_blob_bytes {
                return Err(BlobStoreError::QuotaExceeded {
                    quota: BlobQuota::BlobBytes,
                    limit: self.limits.max_blob_bytes,
                });
            }
            if next_length > self.limits.max_spool_bytes {
                return Err(BlobStoreError::QuotaExceeded {
                    quota: BlobQuota::SpoolBytes,
                    limit: self.limits.max_spool_bytes,
                });
            }
            staged
                .file_mut()?
                .write_all(&buffer[..write_length])
                .map_err(|source| io_error("write staging blob", source))?;
            hasher.update(&buffer[..write_length]);
            length = next_length;

            #[cfg(test)]
            if write_length < read {
                return Err(io_error(
                    "write staging blob",
                    io::Error::other("injected partial publication failure"),
                ));
            }
        }

        let mut file = staged
            .file
            .take()
            .ok_or_else(|| io_error("close staging blob", io::Error::other("file closed")))?;
        file.flush()
            .map_err(|source| io_error("flush staging blob", source))?;
        file.sync_all()
            .map_err(|source| io_error("sync staging blob", source))?;
        drop(file);

        let digest = digest_from_sha256(hasher.finalize().into());
        verify_object_file(&staged.path, digest, length, self.limits.max_blob_bytes)?;

        let reference = self.publish_staged(&staged.path, digest, length)?;
        staged.cleanup()?;
        Ok(BlobRef {
            digest: reference,
            length,
            media_type,
            encoding,
        })
    }

    fn publish_staged(
        &self,
        staged_path: &Path,
        digest: ContentDigest,
        length: u64,
    ) -> Result<ContentDigest, BlobStoreError> {
        let object_name = digest.to_string();
        let target = self.objects_dir.join(&object_name);
        let mut state = self
            .state
            .lock()
            .map_err(|_| BlobStoreError::StatePoisoned)?;

        match fs::symlink_metadata(&target) {
            Ok(_) => {
                verify_object_file(&target, digest, length, self.limits.max_blob_bytes)?;
                if !state.objects.contains_key(&object_name) {
                    self.account_existing(&mut state, object_name, length)?;
                }
                // Retry the sync even for an object already in memory: an earlier
                // publication may have linked it before its directory sync failed.
                sync_directory(&self.objects_dir, "sync existing blob object")?;
                return Ok(digest);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if state.objects.contains_key(&object_name) {
                    return Err(BlobStoreError::Missing { digest });
                }
            }
            Err(source) => return Err(io_error("inspect blob object", source)),
        }

        self.ensure_content_capacity(&state, length)?;
        self.ensure_object_capacity(&state)?;

        match fs::hard_link(staged_path, &target) {
            Ok(()) => {
                self.account_existing(&mut state, object_name, length)?;
                // Do not return a BlobRef unless the new object name is durable.
                sync_directory(&self.objects_dir, "sync published blob object")?;
                Ok(digest)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // Protect idempotency if an external writer races despite the exclusive
                // namespace contract. Verify before recognizing the existing artifact.
                verify_object_file(&target, digest, length, self.limits.max_blob_bytes)?;
                self.account_existing(&mut state, object_name, length)?;
                sync_directory(&self.objects_dir, "sync raced blob object")?;
                Ok(digest)
            }
            Err(source) => Err(io_error("publish blob object", source)),
        }
    }

    fn ensure_content_capacity(
        &self,
        state: &StoreState,
        incoming: u64,
    ) -> Result<(), BlobStoreError> {
        let total = state
            .content_bytes
            .checked_add(incoming)
            .ok_or(BlobStoreError::LengthOverflow)?;
        if total > self.limits.max_content_bytes {
            return Err(BlobStoreError::QuotaExceeded {
                quota: BlobQuota::ContentBytes,
                limit: self.limits.max_content_bytes,
            });
        }
        Ok(())
    }

    fn ensure_object_capacity(&self, state: &StoreState) -> Result<(), BlobStoreError> {
        let count =
            u64::try_from(state.objects.len()).map_err(|_| BlobStoreError::LengthOverflow)?;
        let limit = self.limits.max_object_count.min(MAX_SCAN_ENTRIES as u64);
        if count >= limit {
            return Err(BlobStoreError::QuotaExceeded {
                quota: BlobQuota::ObjectCount,
                limit,
            });
        }
        Ok(())
    }

    fn account_existing(
        &self,
        state: &mut StoreState,
        object_name: String,
        length: u64,
    ) -> Result<(), BlobStoreError> {
        if state.objects.contains_key(&object_name) {
            return Ok(());
        }
        self.ensure_content_capacity(state, length)?;
        self.ensure_object_capacity(state)?;
        state.content_bytes = state
            .content_bytes
            .checked_add(length)
            .ok_or(BlobStoreError::LengthOverflow)?;
        state.objects.insert(object_name, length);
        Ok(())
    }

    fn object_path(&self, digest: ContentDigest) -> PathBuf {
        self.objects_dir.join(digest.to_string())
    }
}

struct TempBlob {
    path: PathBuf,
    file: Option<File>,
    removed: bool,
}

impl TempBlob {
    fn create(staging_dir: &Path) -> Result<Self, BlobStoreError> {
        for _ in 0..MAX_TEMP_NAME_ATTEMPTS {
            let path = staging_dir.join(Uuid::now_v7().to_string());
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                        removed: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(io_error("create staging blob", source)),
            }
        }
        Err(io_error(
            "create staging blob",
            io::Error::new(
                io::ErrorKind::AlreadyExists,
                "staging name collision limit reached",
            ),
        ))
    }

    fn file_mut(&mut self) -> Result<&mut File, BlobStoreError> {
        self.file
            .as_mut()
            .ok_or_else(|| io_error("write staging blob", io::Error::other("file closed")))
    }

    fn cleanup(&mut self) -> Result<(), BlobStoreError> {
        self.file.take();
        match fs::remove_file(&self.path) {
            Ok(()) => {
                self.removed = true;
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.removed = true;
                Ok(())
            }
            Err(source) => Err(io_error("remove staging blob", source)),
        }
    }
}

impl Drop for TempBlob {
    fn drop(&mut self) {
        self.file.take();
        if !self.removed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn acquire_owner(root: &Path) -> Result<File, BlobStoreError> {
    let lock = root.join(OWNER_LOCK);
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock)
        .map_err(|source| io_error("open blob namespace owner lock", source))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(BlobStoreError::InUse(root.to_path_buf())),
        Err(TryLockError::Error(source)) => Err(io_error("lock blob namespace", source)),
    }
}

pub(crate) fn ensure_session_namespace(path: &Path) -> Result<(), BlobStoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| BlobStoreError::InvalidNamespace(path.to_path_buf()))?;
    ensure_child_directory(parent, path)?;
    // Retry the parent sync even after a previous creation/sync failure.
    sync_directory(parent, "sync blob namespace creation")
}

fn ensure_child_directory(root: &Path, child: &Path) -> Result<(), BlobStoreError> {
    match fs::symlink_metadata(child) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => Ok(()),
        Ok(_) => Err(BlobStoreError::InvalidNamespace(child.to_path_buf())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(child)
                .map_err(|source| io_error("create blob store directory", source))?;
            sync_directory(root, "sync blob store directory creation")
        }
        Err(source) => Err(io_error("inspect blob store directory", source)),
    }
}

fn clean_staging(staging_dir: &Path) -> Result<(), BlobStoreError> {
    let entries =
        fs::read_dir(staging_dir).map_err(|source| io_error("list staging blobs", source))?;
    let mut changed = false;
    for entry in entries {
        let entry = entry.map_err(|source| io_error("read staging directory entry", source))?;
        fs::remove_file(entry.path())
            .map_err(|source| io_error("remove stale staging blob", source))?;
        changed = true;
    }
    if changed {
        sync_directory(staging_dir, "sync staging cleanup")?;
    }
    Ok(())
}

fn scan_objects(objects_dir: &Path) -> Result<StoreState, BlobStoreError> {
    let entries =
        fs::read_dir(objects_dir).map_err(|source| io_error("list blob objects", source))?;
    let mut objects = HashMap::new();
    let mut content_bytes = 0_u64;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_SCAN_ENTRIES {
            return Err(BlobStoreError::QuotaExceeded {
                quota: BlobQuota::ObjectCount,
                limit: MAX_SCAN_ENTRIES as u64,
            });
        }
        let entry = entry.map_err(|source| io_error("read blob object entry", source))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|source| io_error("inspect blob object", source))?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        // Corrupt or foreign entries must not hide the Session's committed
        // transcript. A referenced malformed object fails explicitly on read.
        if !metadata.is_file() || metadata.file_type().is_symlink() || !is_digest_name(&name) {
            continue;
        }
        let length = metadata.len();
        // Existing objects remain inspectable if limits were reduced; new growth
        // is refused by the publication checks below.
        content_bytes = content_bytes
            .checked_add(length)
            .ok_or(BlobStoreError::LengthOverflow)?;
        objects.insert(name, length);
    }
    Ok(StoreState {
        objects,
        content_bytes,
    })
}

fn is_digest_name(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn open_object(
    path: &Path,
    digest: ContentDigest,
    expected_length: u64,
) -> Result<File, BlobStoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {}
        Ok(_) => {
            return Err(BlobStoreError::Corrupt {
                digest,
                expected_length,
                actual_length: None,
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(BlobStoreError::Missing { digest });
        }
        Err(source) => return Err(io_error("inspect blob", source)),
    }
    match File::open(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(BlobStoreError::Missing { digest })
        }
        Err(source) => Err(io_error("open blob", source)),
    }
}

fn verify_object_file(
    path: &Path,
    digest: ContentDigest,
    expected_length: u64,
    max_blob_bytes: u64,
) -> Result<(), BlobStoreError> {
    let mut file = open_object(path, digest, expected_length)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; IO_BUFFER_BYTES];
    let mut actual_length = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| io_error("verify blob", source))?;
        if read == 0 {
            break;
        }
        actual_length =
            checked_next_length(actual_length, read).map_err(|_| BlobStoreError::Corrupt {
                digest,
                expected_length,
                actual_length: None,
            })?;
        if actual_length > max_blob_bytes || actual_length > expected_length {
            return Err(BlobStoreError::Corrupt {
                digest,
                expected_length,
                actual_length: Some(actual_length),
            });
        }
        hasher.update(&buffer[..read]);
    }
    if actual_length != expected_length || !hash_matches(hasher, digest) {
        return Err(BlobStoreError::Corrupt {
            digest,
            expected_length,
            actual_length: Some(actual_length),
        });
    }
    Ok(())
}

fn checked_next_length(current: u64, bytes: usize) -> Result<u64, BlobStoreError> {
    let bytes = u64::try_from(bytes).map_err(|_| BlobStoreError::LengthOverflow)?;
    current
        .checked_add(bytes)
        .ok_or(BlobStoreError::LengthOverflow)
}

fn hash_matches(hasher: Sha256, expected: ContentDigest) -> bool {
    let actual: [u8; 32] = hasher.finalize().into();
    actual == expected.bytes()
}

fn digest_from_sha256(bytes: [u8; 32]) -> ContentDigest {
    ContentDigest::from_bytes(bytes)
}

fn sync_directory(path: &Path, operation: &'static str) -> Result<(), BlobStoreError> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error(operation, source))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io_error(
            operation,
            io::Error::new(
                io::ErrorKind::Unsupported,
                "directory durability sync is unsupported on this platform",
            ),
        ))
    }
}

fn io_error(operation: &'static str, source: io::Error) -> BlobStoreError {
    BlobStoreError::Io { operation, source }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct TestNamespace(PathBuf);

    impl TestNamespace {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("ion-blob-test-{}", Uuid::now_v7()));
            fs::create_dir(&path).expect("create test namespace");
            Self(path)
        }
    }

    impl Drop for TestNamespace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn limits() -> BlobStoreLimits {
        BlobStoreLimits::new(1024, 4096, 1024, 128, 16)
    }

    fn store(namespace: &TestNamespace, limits: BlobStoreLimits) -> BlobStore {
        BlobStore::open(&namespace.0, limits).expect("open test store")
    }

    #[test]
    fn streams_publishes_verifies_and_reads_pages() {
        let namespace = TestNamespace::new();
        let store = store(
            &namespace,
            BlobStoreLimits::new(256 * 1024, 512 * 1024, 256 * 1024, 128, 16),
        );
        let contents = vec![b'x'; IO_BUFFER_BYTES * 3 + 17];
        let reference = store
            .publish_with_metadata(
                Cursor::new(&contents),
                Some("application/octet-stream".into()),
                Some("identity".into()),
            )
            .expect("publish streamed content");

        assert_eq!(reference.length, contents.len() as u64);
        assert_eq!(reference.digest, ContentDigest::of_bytes(&contents));
        assert_eq!(
            reference.media_type.as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(
            store.read_range(&reference, 123, 101).expect("read page"),
            contents[123..224]
        );
        assert_eq!(
            store.usage().expect("usage"),
            BlobStoreUsage {
                content_bytes: contents.len() as u64,
                object_count: 1,
            }
        );
    }

    #[test]
    fn repeated_publication_is_idempotent_and_metadata_is_per_reference() {
        let namespace = TestNamespace::new();
        let store = store(&namespace, limits());
        let first = store
            .publish(Cursor::new(b"same bytes"))
            .expect("first publish");
        let second = store
            .publish_with_metadata(Cursor::new(b"same bytes"), Some("text/plain".into()), None)
            .expect("repeat publish");

        assert_eq!(first.digest, second.digest);
        assert_eq!(first.length, second.length);
        assert_eq!(second.media_type.as_deref(), Some("text/plain"));
        assert_eq!(store.usage().expect("usage").object_count, 1);
        assert_eq!(
            fs::read_dir(namespace.0.join(OBJECTS_DIRECTORY))
                .expect("objects")
                .count(),
            1
        );
    }

    #[test]
    fn oversized_reference_metadata_is_rejected_before_spooling() {
        let namespace = TestNamespace::new();
        let store = store(&namespace, limits());
        assert!(matches!(
            store.publish_with_metadata(
                Cursor::new(b"x"),
                Some("x".repeat(MAX_METADATA_BYTES + 1)),
                None
            ),
            Err(BlobStoreError::QuotaExceeded {
                quota: BlobQuota::MetadataBytes,
                ..
            })
        ));
        assert_eq!(store.usage().unwrap().object_count, 0);
    }

    #[test]
    fn partial_write_failure_leaves_no_object_or_stale_quota() {
        let namespace = TestNamespace::new();
        let store = store(&namespace, BlobStoreLimits::new(4, 4, 4, 4, 2));
        let error = store
            .publish_inner(&mut Cursor::new(b"four"), None, None, Some(2))
            .expect_err("injected partial write fails");
        assert!(matches!(error, BlobStoreError::Io { .. }));
        assert_eq!(
            store.usage().expect("usage"),
            BlobStoreUsage {
                content_bytes: 0,
                object_count: 0
            }
        );
        assert_eq!(
            fs::read_dir(namespace.0.join(STAGING_DIRECTORY))
                .expect("staging")
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(namespace.0.join(OBJECTS_DIRECTORY))
                .expect("objects")
                .count(),
            0
        );

        let reference = store
            .publish(Cursor::new(b"four"))
            .expect("quota was released");
        assert_eq!(reference.length, 4);
    }

    #[test]
    fn streaming_limits_reject_overflow_before_publication() {
        let namespace = TestNamespace::new();
        let store = store(&namespace, BlobStoreLimits::new(3, 8, 3, 3, 2));
        let error = store
            .publish(Cursor::new(b"four"))
            .expect_err("per-blob limit is enforced while streaming");
        assert!(matches!(
            error,
            BlobStoreError::QuotaExceeded {
                quota: BlobQuota::BlobBytes,
                limit: 3
            }
        ));
        assert_eq!(store.usage().expect("usage").object_count, 0);
        assert_eq!(
            fs::read_dir(namespace.0.join(OBJECTS_DIRECTORY))
                .expect("objects")
                .count(),
            0
        );
        let spool_namespace = TestNamespace::new();
        let spool_limited =
            BlobStore::open(&spool_namespace.0, BlobStoreLimits::new(8, 8, 2, 2, 2))
                .expect("open spool-limited store");
        assert!(matches!(
            spool_limited.publish(Cursor::new(b"abc")),
            Err(BlobStoreError::QuotaExceeded {
                quota: BlobQuota::SpoolBytes,
                limit: 2
            })
        ));

        assert!(matches!(
            checked_next_length(u64::MAX, 1),
            Err(BlobStoreError::LengthOverflow)
        ));
    }

    #[test]
    fn content_quota_and_object_count_include_empty_blobs() {
        let namespace = TestNamespace::new();
        let store = store(&namespace, BlobStoreLimits::new(8, 3, 8, 8, 1));
        store.publish(Cursor::new(b"abc")).expect("first object");
        assert!(matches!(
            store.publish(Cursor::new(b"")),
            Err(BlobStoreError::QuotaExceeded {
                quota: BlobQuota::ObjectCount,
                limit: 1
            })
        ));
        assert!(matches!(
            store.publish(Cursor::new(b"d")),
            Err(BlobStoreError::QuotaExceeded {
                quota: BlobQuota::ContentBytes,
                limit: 3
            })
        ));
    }

    #[test]
    fn missing_and_corrupt_artifacts_are_explicit() {
        let namespace = TestNamespace::new();
        let store = store(&namespace, limits());
        let reference = store.publish(Cursor::new(b"artifact")).expect("publish");
        fs::remove_file(store.object_path(reference.digest)).expect("remove object");
        assert!(matches!(
            store.read_range(&reference, 0, 8),
            Err(BlobStoreError::Missing { digest }) if digest == reference.digest
        ));

        let replacement = store
            .publish(Cursor::new(b"restore"))
            .expect("publish replacement");
        fs::write(store.object_path(replacement.digest), b"corrupt").expect("corrupt object");
        assert!(matches!(
            store.read_range(&replacement, 0, 8),
            Err(BlobStoreError::Corrupt { digest, .. }) if digest == replacement.digest
        ));
    }

    #[test]
    fn passive_open_preserves_staging_until_publication_and_defers_digest_checks() {
        let namespace = TestNamespace::new();
        let original_store = store(&namespace, limits());
        let reference = original_store
            .publish(Cursor::new(b"content"))
            .expect("publish");
        drop(original_store);

        let staging = namespace.0.join(STAGING_DIRECTORY);
        fs::write(staging.join("crash-leftover"), b"partial").expect("write orphan");
        let reopened = store(&namespace, limits());
        assert_eq!(fs::read_dir(&staging).expect("staging").count(), 1);
        let republished = reopened
            .publish(Cursor::new(b"content"))
            .expect("reuse durable orphan after reopen");
        assert_eq!(republished.digest, reference.digest);
        assert_eq!(fs::read_dir(staging).expect("staging").count(), 0);
        assert_eq!(reopened.usage().expect("usage").object_count, 1);
        // The digest is checked on dereference, not on open.
        fs::write(reopened.object_path(reference.digest), b"corrupt").expect("corrupt object");
        assert!(matches!(
            reopened.read_range(&reference, 0, 7),
            Err(BlobStoreError::Corrupt { .. })
        ));
    }

    #[test]
    fn concurrent_open_cannot_remove_an_active_writers_staging() {
        let namespace = TestNamespace::new();
        let first = store(&namespace, limits());
        let staging = namespace.0.join(STAGING_DIRECTORY).join("in-progress");
        fs::write(&staging, b"uncommitted").expect("stage test content");
        assert!(matches!(
            BlobStore::open(&namespace.0, limits()),
            Err(BlobStoreError::InUse(_))
        ));
        assert_eq!(fs::read(&staging).unwrap(), b"uncommitted");
        drop(first);
        let reopened = store(&namespace, limits());
        assert_eq!(fs::read(&staging).unwrap(), b"uncommitted");
        reopened.publish(Cursor::new(b"committed")).unwrap();
        assert!(!staging.exists());
    }

    #[test]
    fn corrupt_auxiliary_object_and_reduced_quota_do_not_block_open() {
        let namespace = TestNamespace::new();
        let original = store(&namespace, limits());
        let reference = original.publish(Cursor::new(b"unchanged")).unwrap();
        drop(original);
        let small = BlobStoreLimits::new(2, 2, 2, 2, 1);
        let reopened = store(&namespace, small);
        assert_eq!(reopened.usage().unwrap().content_bytes, reference.length);
        drop(reopened);
        let path = namespace
            .0
            .join(OBJECTS_DIRECTORY)
            .join(reference.digest.to_string());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let reopened = store(&namespace, limits());
        assert!(matches!(
            reopened.read_range(&reference, 0, 4),
            Err(BlobStoreError::Corrupt { .. })
        ));
    }

    #[test]
    fn sha256_digest_bridge_matches_existing_digest_api() {
        let contents = b"digest bridge";
        let streamed = digest_from_sha256(Sha256::digest(contents).into());
        assert_eq!(streamed, ContentDigest::of_bytes(contents));
    }
}
