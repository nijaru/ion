//! Concrete application-lifetime custody, separate from Session control.
use std::sync::Arc;

use anyhow::Result;

use crate::{BackgroundOwner, ProcessOwner};

/// Captured by the outer application, not serialized or inferred from history.
/// Both owners outlive individual Turns and binding replacements. Holding this
/// owner does not itself install either capability in an application runtime.
pub struct ApplicationOwner {
    background: Arc<BackgroundOwner>,
    processes: Arc<ProcessOwner>,
}

impl ApplicationOwner {
    pub fn new(background: Arc<BackgroundOwner>, processes: Arc<ProcessOwner>) -> Self {
        Self {
            background,
            processes,
        }
    }

    pub fn background(&self) -> &Arc<BackgroundOwner> {
        &self.background
    }

    pub fn processes(&self) -> &Arc<ProcessOwner> {
        &self.processes
    }

    /// Close both admission paths before awaiting either resource family.
    pub fn request_shutdown(&self) {
        self.background.request_shutdown();
        self.processes.request_shutdown();
    }

    /// Cancelling a borrowed join does not cancel either retained owner.
    pub async fn join_shutdown(&self) -> Result<()> {
        self.request_shutdown();
        let (children, processes) = tokio::join!(
            self.background.join_shutdown(),
            self.processes.join_shutdown(),
        );
        match (children, processes) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error.into()),
            (Ok(()), Err(error)) => Err(error),
            (Err(children), Err(processes)) => Err(anyhow::anyhow!(
                "child shutdown failed: {children}; process shutdown also failed: {processes}"
            )),
        }
    }
}

impl Drop for ApplicationOwner {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}
