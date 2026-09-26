//! Cross-process writable ownership of one Session database.

use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};

use super::super::StoreError;

#[derive(Debug)]
pub(crate) struct Ownership {
    _file: File,
}

const HANDOVER_ATTEMPTS: u32 = 20;
const HANDOVER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

impl Ownership {
    pub(super) fn acquire(database: &Path) -> Result<Self, StoreError> {
        let path = lock_path(&canonical_database(database)?);
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| {
                StoreError::Io(format!(
                    "cannot open session ownership lock {}: {error}",
                    path.display()
                ))
            })?;

        for attempt in 0..HANDOVER_ATTEMPTS {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(TryLockError::WouldBlock) => {
                    if attempt + 1 < HANDOVER_ATTEMPTS {
                        std::thread::sleep(HANDOVER_INTERVAL);
                    }
                }
                Err(TryLockError::Error(error)) => {
                    return Err(StoreError::Io(format!(
                        "cannot lock session ownership file {}: {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(StoreError::InUse(database.to_path_buf()))
    }
}

fn canonical_database(database: &Path) -> Result<PathBuf, StoreError> {
    match fs::symlink_metadata(database) {
        Ok(_) => {
            let canonical = fs::canonicalize(database).map_err(|error| {
                StoreError::Io(format!("cannot resolve Session database: {error}"))
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if fs::metadata(&canonical)
                    .map_err(|error| StoreError::Io(error.to_string()))?
                    .nlink()
                    != 1
                {
                    return Err(StoreError::InvalidRequest(
                        "hard-linked Session database aliases are unsupported".into(),
                    ));
                }
            }
            Ok(canonical)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = database
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let name = database.file_name().ok_or_else(|| {
                StoreError::InvalidRequest("Session database path needs a filename".into())
            })?;
            let parent = fs::canonicalize(parent).map_err(|error| {
                StoreError::Io(format!("cannot resolve Session directory: {error}"))
            })?;
            Ok(parent.join(name))
        }
        Err(error) => Err(StoreError::Io(format!(
            "cannot inspect Session database: {error}"
        ))),
    }
}

fn lock_path(database: &Path) -> PathBuf {
    let mut name = database
        .file_name()
        .map_or_else(|| "session.sqlite".into(), std::ffi::OsString::from);
    name.push(".lock");
    database.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn aliases_cannot_acquire_a_second_session_owner() {
        use std::os::unix::fs::symlink;

        let dir =
            std::env::temp_dir().join(format!("ion-ownership-alias-{}", crate::SessionId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let database = dir.join("session.sqlite");
        std::fs::write(&database, b"test").unwrap();
        let alias = dir.join("alias.sqlite");
        symlink(&database, &alias).unwrap();
        let first = Ownership::acquire(&database).unwrap();
        assert!(matches!(
            Ownership::acquire(&alias),
            Err(StoreError::InUse(_))
        ));
        let hardlink = dir.join("hardlink.sqlite");
        std::fs::hard_link(&database, &hardlink).unwrap();
        assert!(matches!(
            Ownership::acquire(&hardlink),
            Err(StoreError::InvalidRequest(_))
        ));
        drop(first);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_second_owner_is_refused_and_release_frees_it() {
        let dir =
            std::env::temp_dir().join(format!("ion-r1a-ownership-{}", crate::SessionId::new()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let database = dir.join("session.sqlite");

        let first = Ownership::acquire(&database).expect("first owner");
        let refused = Ownership::acquire(&database).expect_err("second owner must be refused");
        assert!(matches!(refused, StoreError::InUse(_)));
        drop(first);

        Ownership::acquire(&database).expect("released ownership is available again");
        std::fs::remove_dir_all(&dir).ok();
    }
}
