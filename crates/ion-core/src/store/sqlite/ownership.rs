//! Cross-process writable ownership of one session database.
//!
//! One loaded session has one authoritative writer. The commit-cursor
//! compare-and-set is not that: it fences a stale canonical *write*, but two
//! processes can still both reconstruct state, both reserve recovery and both
//! perform an external action before either discovers its cursor is stale.
//!
//! The owner therefore holds an OS advisory lock on a lock file beside the
//! database for the whole time it can write: acquired before any SQLite open,
//! schema check or reconstruction, and released after local invocations have
//! joined and writes are fenced. The lock is kernel-held, so process death
//! releases it; a PID/heartbeat/timestamp file could not distinguish a live
//! owner from a dead one and is deliberately not used.
//!
//! A future read-only inspection mode would take a *shared* lock on the same
//! file instead of a second exclusive one; today every open is a writable
//! owner, so the exclusive lock is the whole contract.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use super::StoreError;

/// Held for as long as this process may write to the session database.
#[derive(Debug)]
pub(crate) struct Ownership {
    /// The locked lock file. The lock belongs to this open file description, so
    /// dropping the handle releases it even without an explicit unlock.
    _file: File,
}

/// How long an acquisition waits for a lock that may be mid-handover.
///
/// The kernel releases the lock when the owning process exits or execs, and a
/// process that has just forked shares its open file descriptions with the child
/// until that child execs. A short bounded wait therefore turns "the previous
/// owner is exiting" and "a fork is in flight" into a successful handover,
/// while a genuinely live owner is still refused: it holds the lock for the
/// session's lifetime, not for a few milliseconds.
const HANDOVER_ATTEMPTS: u32 = 20;
const HANDOVER_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);

impl Ownership {
    /// Take exclusive ownership of `database`, or fail because another live
    /// process owns it.
    pub(crate) fn acquire(database: &Path) -> Result<Self, StoreError> {
        let path = lock_path(database);
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| {
                StoreError::other(format!(
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
                    return Err(StoreError::other(format!(
                        "cannot lock session ownership file {}: {error}",
                        path.display()
                    )));
                }
            }
        }
        Err(StoreError::ownership_conflict(database.to_path_buf()))
    }
}

/// The ownership lock lives beside the database, so it never shares an inode
/// with the file SQLite locks itself.
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
        let dir = std::env::temp_dir().join(format!("ion-ownership-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let database = dir.join("session.sqlite");

        let first = Ownership::acquire(&database).expect("first owner");
        let refused = Ownership::acquire(&database).expect_err("second owner must be refused");
        assert!(
            refused
                .to_string()
                .contains("owned by another live process"),
            "unexpected error: {refused}"
        );
        drop(first);

        Ownership::acquire(&database).expect("released ownership is available again");
        std::fs::remove_dir_all(&dir).ok();
    }
}
