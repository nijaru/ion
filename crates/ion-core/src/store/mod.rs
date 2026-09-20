//! Session database ownership for the replacement runtime.
//!
//! R1A owns only file ownership, schema creation/verification, and session identity.
//! Semantic transactions are added in R1B against this schema; there is no old-store adapter.

mod sqlite;

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::SessionId;

pub struct SessionStore {
    path: PathBuf,
    inner: sqlite::SqliteStore,
}

impl std::fmt::Debug for SessionStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionStore")
            .field("path", &self.path)
            .field("session_id", &self.inner.session_id)
            .finish()
    }
}

impl SessionStore {
    pub fn create(path: impl AsRef<Path>, session_id: SessionId) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let inner = sqlite::SqliteStore::create(&path, session_id)?;
        Ok(Self { path, inner })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref().to_path_buf();
        let inner = sqlite::SqliteStore::open(&path)?;
        Ok(Self { path, inner })
    }

    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.inner.session_id
    }

    #[must_use]
    pub const fn schema_version(&self) -> i64 {
        sqlite::schema::SCHEMA_VERSION
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("session database already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("session database does not exist: {0}")]
    Unknown(PathBuf),
    #[error("session database is owned by another process: {0}")]
    InUse(PathBuf),
    #[error("unsupported session schema {found}; expected {expected}")]
    UnsupportedSchema { found: i64, expected: i64 },
    #[error("corrupt session database: {0}")]
    Corrupt(String),
    #[error("sqlite failure: {0}")]
    Sqlite(String),
    #[error("filesystem failure: {0}")]
    Io(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ion-r1a-{name}-{}-{}.sqlite",
            std::process::id(),
            SessionId::new()
        ))
    }

    #[test]
    fn create_and_open_preserve_session_identity() {
        let path = path("roundtrip");
        let id = SessionId::new();
        {
            let store = SessionStore::create(&path, id).expect("create");
            assert_eq!(store.session_id(), id);
            assert_eq!(store.schema_version(), sqlite::schema::SCHEMA_VERSION);
        }
        let reopened = SessionStore::open(&path).expect("open");
        assert_eq!(reopened.session_id(), id);
        drop(reopened);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(path.with_extension("sqlite.lock")).ok();
    }
}
