//! Host-owned session lifecycle for the interactive TUI (DESIGN.md §12).
//!
//! Pi-parity session surface: `/new`, `/resume`, `/clone`, `/name`, and
//! the session picker. The manager is the single owner of runtime
//! switching: closing the attached stack in order (session handle →
//! runtime join → hosted-agent teardown), opening the next session with
//! the durable model (never the launch default), and handing the TUI the
//! fresh handle. The TUI never writes the session store and never owns
//! runtime lifecycle.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use ion_core::{Runtime, RuntimeError, SessionId, SessionStore};
/// How the next attached session is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionStart {
    /// A brand-new durable session.
    New,
    /// Reopen an existing session by durable id.
    Resume(SessionId),
}

/// Type-erased teardown for the hosted-agent service bound to one
/// session's family. One async call; boxing keeps `AttachedSession`
/// concrete while the host composes its generic `AgentHost<P>`.
pub type HostedTeardown =
    Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send>;

/// The full per-session runtime stack as opened by the host.
pub struct OpenedRuntime {
    pub runtime: Runtime,
    pub hosted_teardown: Option<HostedTeardown>,
}

/// The runtime factory the host supplies per switch. Opening a resumed
/// session must compose the provider from the session's durable model
/// reference, not the launch default (§14.8). Boxed-async because the
/// host composes runtime + hosted-agent service before the switch
/// exposes either.
pub type OpenRuntime = Arc<
    dyn Fn(
            SessionStart,
        ) -> Pin<Box<dyn Future<Output = Result<OpenedRuntime, RuntimeError>> + Send>>
        + Send
        + Sync,
>;

/// One picker row for the TUI: durable identity plus presentation
/// metadata, already rendered by the store layer.
pub use ion_core::SessionSummary;

/// Everything the TUI needs while attached to one durable session. The
/// manager (and, at final exit, the process host) owns the stack.
pub struct AttachedSession {
    runtime: Runtime,
    session_id: SessionId,
    title: String,
    hosted_teardown: Option<HostedTeardown>,
}

impl AttachedSession {
    #[must_use]
    pub fn handle(&self) -> ion_core::SessionHandle {
        self.runtime.session()
    }

    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Close in the one valid order: session handle, runtime join, then
    /// hosted-agent teardown (matching process-exit teardown).
    pub async fn close(self) -> Result<(), RuntimeError> {
        let Self {
            runtime,
            session_id,
            hosted_teardown,
            ..
        } = self;
        let handle = runtime.session();
        if let Err(err) = handle.close().await
            && !err.to_string().contains("closed")
        {
            tracing::warn!(session = %session_id, %err, "session close during switch");
        }
        runtime.join().await?;
        if let Some(teardown) = hosted_teardown
            && let Err(err) = teardown().await
        {
            tracing::error!(session = %session_id, %err, "hosted-agent teardown failed");
        }
        Ok(())
    }
}

/// Errors surfaced to the user as notices, never silent failures.
#[derive(Debug, thiserror::Error)]
pub enum SessionManagerError {
    #[error("{0}")]
    Store(#[from] ion_core::StoreError),
    #[error("{0}")]
    Runtime(#[from] RuntimeError),
}

/// Owns switching between durable sessions for one interactive process.
pub struct SessionManager {
    store: SessionStore,
    open_runtime: OpenRuntime,
}

impl SessionManager {
    #[must_use]
    pub fn new(store: SessionStore, open_runtime: OpenRuntime) -> Self {
        Self {
            store,
            open_runtime,
        }
    }

    /// Attach to the first session of the process (already resolved by
    /// the CLI flags); assigns the durable title for resumed sessions.
    pub async fn adopt(
        &self,
        opened: OpenedRuntime,
    ) -> Result<AttachedSession, SessionManagerError> {
        let session_id = opened.runtime.session_id();
        let title = self
            .store
            .load(session_id)
            .await
            .map(|loaded| loaded.session.title)
            .unwrap_or_default();
        Ok(AttachedSession {
            runtime: opened.runtime,
            session_id,
            title,
            hosted_teardown: opened.hosted_teardown,
        })
    }

    /// Durable session roots for the picker, newest first.
    pub async fn list(&self) -> Result<Vec<SessionSummary>, SessionManagerError> {
        Ok(self.store.list_sessions().await?)
    }

    /// Set a session's display title.
    pub async fn rename(
        &self,
        session_id: SessionId,
        title: &str,
    ) -> Result<(), SessionManagerError> {
        Ok(self.store.rename_session(session_id, title).await?)
    }

    /// Delete a session root. The currently attached session cannot be
    /// deleted; switch away first.
    pub async fn delete(&self, session_id: SessionId) -> Result<(), SessionManagerError> {
        Ok(self.store.delete_session(session_id).await?)
    }

    /// Clone a session's semantic history into a new durable session.
    /// Returns the clone's id.
    pub async fn clone_session(
        &self,
        source: SessionId,
        title: &str,
    ) -> Result<SessionId, SessionManagerError> {
        Ok(self.store.clone_session(source, title).await?)
    }

    /// Close the attached stack and open the requested session. The
    /// close order is owned here so no caller can strand an in-flight
    /// hosted agent or a half-joined runtime.
    pub async fn switch(
        &self,
        current: AttachedSession,
        start: SessionStart,
    ) -> Result<AttachedSession, SessionManagerError> {
        current.close().await?;
        let opened = (self.open_runtime)(start).await?;
        self.adopt(opened).await
    }
}
