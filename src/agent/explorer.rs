//! Explorer module - placeholder for future codebase exploration.

use std::path::PathBuf;

pub struct Explorer {
    #[allow(dead_code)]
    working_dir: PathBuf,
}

impl Explorer {
    #[must_use]
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }
}
