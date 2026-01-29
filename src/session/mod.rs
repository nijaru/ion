mod store;

pub use store::{SessionStore, SessionStoreError, SessionSummary};

use crate::provider::Message;
use chrono::Local;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub working_dir: PathBuf,
    pub model: String,
    pub messages: Vec<Message>,
    pub abort_token: CancellationToken,
    /// Allow operations outside CWD (sandbox disabled)
    pub no_sandbox: bool,
}

/// Generate a session ID: YYYYMMDD-HHMMSS-xxxx (timestamp + 4-char random suffix)
fn generate_session_id() -> String {
    let timestamp = Local::now().format("%Y%m%d-%H%M%S");
    // Use first 4 chars of UUID for random suffix (avoids adding rand dependency)
    let suffix = &uuid::Uuid::new_v4().to_string()[..4];
    format!("{timestamp}-{suffix}")
}

impl Session {
    #[must_use] 
    pub fn new(working_dir: PathBuf, model: String) -> Self {
        Self {
            id: generate_session_id(),
            working_dir,
            model,
            messages: Vec::new(),
            abort_token: CancellationToken::new(),
            no_sandbox: false,
        }
    }

    /// Create a new session with sandbox disabled.
    #[must_use] 
    pub fn with_no_sandbox(mut self) -> Self {
        self.no_sandbox = true;
        self
    }
}
