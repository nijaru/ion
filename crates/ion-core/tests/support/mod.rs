//! Deterministic support shared by integration suites.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// A temporary session database that removes itself and its WAL sidecars.
pub struct TempDb {
    path: PathBuf,
}

impl TempDb {
    pub fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ion-{name}-{}.sqlite", ion_core::SessionId::new()));
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}
