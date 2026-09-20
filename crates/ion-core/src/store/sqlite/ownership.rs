//! Cross-process writable ownership of one Session database.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use super::super::StoreError;

#[derive(Debug)]
pub(super) struct Ownership {
    _file: File,
}

const HANDOVER_ATTEMPTS: u32 = 20;
const HANDOVER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

impl Ownership {
    pub(super) fn acquire(database: &Path) -> Result<Self, StoreError> {
        let path = lock_path(database);
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
