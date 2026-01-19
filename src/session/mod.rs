mod store;

pub use store::{SessionStore, SessionStoreError, SessionSummary};

use crate::provider::Message;
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

impl Session {
    pub fn new(working_dir: PathBuf, model: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            working_dir,
            model,
            messages: Vec::new(),
            abort_token: CancellationToken::new(),
            no_sandbox: false,
        }
    }

    /// Create a new session with sandbox disabled.
    pub fn with_no_sandbox(mut self) -> Self {
        self.no_sandbox = true;
        self
    }
}
