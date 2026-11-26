//! Native Session file acquisition and physical writer-lease ownership.
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags};

use super::{
    SessionEntry, SessionError, artifacts,
    store::{FORMAT_VERSION, Header, Store, StoreSeed, configure_connection, read_header},
};

/// Native location and writer lease. Ownership moves with the live journal,
/// never with a passive read handle.
pub(super) struct FileBinding {
    pub path: PathBuf,
    _lock: SessionLock,
}

#[cfg(test)]
impl FileBinding {
    pub fn duplicate_lease_descriptor(&self) -> std::io::Result<File> {
        self._lock.0.try_clone()
    }
}

pub(super) struct OpenedStore {
    pub store: Store,
    pub header: Header,
    pub file: FileBinding,
}

impl OpenedStore {
    pub fn create(
        path: &Path,
        cwd: Option<&Path>,
        id: uuid::Uuid,
        entries: &[SessionEntry],
        artifact_source: Option<(&Path, uuid::Uuid)>,
    ) -> Result<Self, SessionError> {
        // Validate and encode the prefix before reserving a path or touching disk.
        let seed = StoreSeed::prepare(entries)?;
        let cwd = cwd.map(Path::canonicalize).transpose()?;
        if cwd.as_ref().is_some_and(|cwd| !cwd.is_dir()) {
            return Err(SessionError::InvalidWorkingDirectory);
        }
        let filename = path.file_name().ok_or(SessionError::InvalidPath)?;
        let parent = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let path = parent.canonicalize()?.join(filename);
        // Hold the canonical lease through creation and failed-initialization
        // cleanup; another opener must never own a database we are removing.
        let lock = lock(&path)?;
        let mut creation = create_database_file(&path)?;
        let path = database_path(&path)?;
        let connection = Connection::open(&path)?;
        configure_connection(&connection)?;
        prepare_writer(&connection)?;
        let header = Header {
            version: FORMAT_VERSION,
            // Random prefixes remain useful as catalog selectors and short UI labels.
            id,
            provider_session_id: uuid::Uuid::now_v7(),
            cwd,
            name: None,
        };
        let store = seed.initialize(connection, &header, |tx| {
            if let Some((path, id)) = artifact_source {
                let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                configure_connection(&source)?;
                let snapshot = source.unchecked_transaction()?;
                if read_header(&snapshot)?.provider_session_id != id {
                    return Err(SessionError::InvalidDatabase);
                }
                artifacts::copy(&snapshot, tx, entries)?;
            }
            Ok(())
        })?;
        creation.publish();
        Ok(Self::owned(path, lock, header, store))
    }

    pub fn open(path: &Path) -> Result<Self, SessionError> {
        let path = database_path(path)?;
        let lock = lock(&path)?;
        // A disappearing resume target must fail, not create a new empty file
        // between path validation and SQLite opening it.
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::default().difference(OpenFlags::SQLITE_OPEN_CREATE),
        )?;
        configure_connection(&connection)?;
        let (store, header) = Store::reopen(connection)?;
        prepare_writer(&store.connection)?;
        Ok(Self::owned(path, lock, header, store))
    }

    fn owned(path: PathBuf, lock: SessionLock, header: Header, store: Store) -> Self {
        Self {
            store,
            header,
            file: FileBinding { path, _lock: lock },
        }
    }
}

struct SessionLock(File);

struct CreatedDatabase {
    path: PathBuf,
    identity: (u64, u64),
    keep: bool,
}
impl CreatedDatabase {
    fn publish(&mut self) {
        self.keep = true;
    }
}
impl Drop for CreatedDatabase {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && (metadata.dev(), metadata.ino()) == self.identity
        {
            let _ = fs::remove_file(&self.path);
            for suffix in ["-wal", "-shm"] {
                let mut path = self.path.as_os_str().to_os_string();
                path.push(suffix);
                let _ = fs::remove_file(PathBuf::from(path));
            }
        }
        // The lock file's stable inode belongs to future attempts as well.
    }
}

fn create_database_file(path: &Path) -> Result<CreatedDatabase, SessionError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            let _ = fs::remove_file(path);
            return Err(error.into());
        }
    };
    Ok(CreatedDatabase {
        path: path.to_path_buf(),
        identity: (metadata.dev(), metadata.ino()),
        keep: false,
    })
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        // A child may briefly inherit a duplicate file description. Release
        // this writer's lease explicitly before closing our descriptor.
        let _ = self.0.unlock();
    }
}

fn prepare_writer(connection: &Connection) -> Result<(), SessionError> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

pub(super) fn database_path(path: &Path) -> Result<PathBuf, SessionError> {
    let path = path.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SessionError::NotFound
        } else {
            SessionError::Io(error)
        }
    })?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_file() {
        return Err(SessionError::NotFound);
    }
    // SQLite resolves symlinks, but cannot unify hard-linked journal/WAL names.
    // Reject those aliases rather than claiming lock normalization makes them safe.
    // https://www.sqlite.org/howtocorrupt.html#multiple_links_to_the_same_file
    if metadata.nlink() != 1 {
        return Err(SessionError::HardLinkedDatabase);
    }
    Ok(path)
}

fn lock(path: &Path) -> Result<SessionLock, SessionError> {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(".lock");
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(PathBuf::from(sidecar))?;
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => SessionError::AlreadyOpen,
        fs::TryLockError::Error(error) => SessionError::Io(error),
    })?;
    Ok(SessionLock(file))
}
