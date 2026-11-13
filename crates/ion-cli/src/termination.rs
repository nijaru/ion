//! Application termination is independent of the client's input/publication pump.
use std::{io, sync::Arc};

use ion_app::ApplicationOwner;
use tokio::{
    signal::unix::{Signal, SignalKind, signal},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub(crate) struct Termination {
    requested: CancellationToken,
    closing: CancellationToken,
    listener: JoinHandle<()>,
}

impl Termination {
    pub(crate) fn listen(
        startup: CancellationToken,
        owner: Option<Arc<ApplicationOwner>>,
    ) -> io::Result<Self> {
        let mut terminate = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;
        let requested = CancellationToken::new();
        let notification = requested.clone();
        let closing = CancellationToken::new();
        let close_listener = closing.clone();
        // One-shot shutdown, not forced-exit escalation. Tokio keeps installed
        // handlers for the process lifetime; resource owners still finish cleanup.
        let listener = tokio::spawn(async move {
            tokio::select! {
                biased;
                _ = terminate.recv() => {},
                _ = hangup.recv() => {},
                _ = close_listener.cancelled() => return,
            }
            startup.cancel();
            if let Some(owner) = owner {
                owner.request_shutdown();
            }
            notification.cancel();
        });
        Ok(Self {
            requested,
            closing,
            listener,
        })
    }

    pub(crate) fn token(&self) -> CancellationToken {
        self.requested.clone()
    }

    pub(crate) async fn finish(mut self) -> Result<(), tokio::task::JoinError> {
        // The listener owns only notification/shutdown capabilities, never a
        // child, terminal lease or durable write. Client/resource joins stay put.
        // Give an already-ready signal priority over normal listener closure.
        self.closing.cancel();
        (&mut self.listener).await
    }
}

impl Drop for Termination {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

// Keep SIGINT separate: during an editor handoff it belongs to the editor,
// whereas TERM/HUP close the application even when its input pump is blocked.
pub(crate) async fn requested(interrupt: &mut Signal, termination: &CancellationToken) {
    tokio::select! {
        biased;
        _ = termination.cancelled() => {},
        _ = interrupt.recv() => {},
    }
}
