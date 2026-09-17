//! The session owner.
//!
//! Creating or opening a session establishes durable ownership and starts the
//! command service. It starts no model or tool work: opening is passive, and a
//! turn advances only when a client submits or resumes one.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::{broadcast, mpsc, oneshot};

use super::command::Request;
use super::handle::SessionHandle;
use super::supervisor::{CloseOutcome, Services, Shared, Supervisor};
use crate::config::ConversationConfig;
use crate::error::{Error, Result};
use crate::limits::SessionLimits;
use crate::store::sqlite::SqliteStore;
use crate::store::sqlite::conversation::{CreateSession, ReadSessionInfo, ValidateAncestry};
use crate::store::{Db, SessionInfo, StoreError};
use crate::{ConversationId, SessionId};

/// How far behind an observer may fall before it is told to resnapshot.
const EVENT_CAPACITY: usize = 1_024;

/// The parameters a new session is created with.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    pub limits: SessionLimits,
    /// The primary conversation's initial configuration.
    pub config: ConversationConfig,
}

impl SessionSpec {
    pub fn validate(&self) -> Result<()> {
        self.limits
            .validate()
            .map_err(|error| Error::Invalid(error.to_string()))?;
        self.config.validate()?;
        Ok(())
    }
}

/// One durable session and its supervisor.
pub struct Session {
    shared: Arc<Shared>,
    requests: mpsc::Sender<Request>,
    supervisor: Option<tokio::task::JoinHandle<()>>,
    info: SessionInfo,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("session_id", &self.info.session_id)
            .field("root", &self.info.root)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Create a new session database and its primary conversation.
    pub async fn create(path: &Path, spec: SessionSpec, services: Services) -> Result<Self> {
        spec.validate()?;
        let store = SqliteStore::create(path).map_err(StoreError::into_error)?;
        let db = Db::start(store, spec.limits).map_err(|error| error.into_error())?;
        let info = db
            .run(CreateSession {
                session_id: SessionId::new(),
                config: spec.config,
            })
            .await
            .map_err(StoreError::into_error)?;
        db.run(ValidateAncestry)
            .await
            .map_err(StoreError::into_error)?;
        Ok(Self::start(db, info, spec.limits, services))
    }

    /// Open an existing session. Nothing is resumed and nothing is started.
    pub async fn open(path: &Path, limits: SessionLimits, services: Services) -> Result<Self> {
        limits
            .validate()
            .map_err(|error| Error::Invalid(error.to_string()))?;
        let store = SqliteStore::open(path).map_err(StoreError::into_error)?;
        let db = Db::start(store, limits).map_err(|error| error.into_error())?;
        let info = db
            .run(ReadSessionInfo)
            .await
            .map_err(StoreError::into_error)?;
        // A store with a cycle, a missing parent or an invisible cutoff is
        // refused before any client can traverse it.
        db.run(ValidateAncestry)
            .await
            .map_err(StoreError::into_error)?;
        Ok(Self::start(db, info, limits, services))
    }

    fn start(db: Db, info: SessionInfo, limits: SessionLimits, services: Services) -> Self {
        let (requests, receive) = mpsc::channel(limits.command_capacity);
        // Actions are handed to the supervisor rather than awaited by the turn
        // that asked for them, so a cancelled turn cannot drop one it started.
        let (spawns, spawned) = mpsc::channel(limits.command_capacity);
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let handle = SessionHandle::new(requests.clone(), events.clone(), info.root);
        let shared = Arc::new(Shared {
            // A client clone: drives and reads may use the queue, never close it.
            db: db.clone(),
            services,
            limits,
            events,
            requests: requests.clone(),
            spawns,
        });
        let supervisor =
            tokio::spawn(Supervisor::new(Arc::clone(&shared), db, receive, spawned, handle).run());
        Self {
            shared,
            requests,
            supervisor: Some(supervisor),
            info,
        }
    }

    /// The session's durable identity.
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.info.session_id
    }

    /// The session's primary conversation.
    #[must_use]
    pub const fn root(&self) -> ConversationId {
        self.info.root
    }

    /// A client handle. Cloning or dropping it never changes session state.
    #[must_use]
    pub fn handle(&self) -> SessionHandle {
        SessionHandle::new(
            self.requests.clone(),
            self.shared.events.clone(),
            self.info.root,
        )
    }

    /// Stop accepting work, join running turns and actions, and release storage
    /// ownership.
    ///
    /// Ownership is released only once nothing this session started is still
    /// running. Work that outlasts the join grace keeps the session open: the
    /// outcome says so, and calling this again retries the join.
    pub async fn close(&mut self) -> Result<CloseOutcome> {
        let (reply, receive) = oneshot::channel();
        let outcome = if self.requests.send(Request::Close { reply }).await.is_ok() {
            receive.await.ok()
        } else {
            None
        };
        if outcome.is_none_or(|outcome| outcome.is_closed()) {
            if let Some(supervisor) = self.supervisor.as_mut() {
                supervisor.await.map_err(|error| {
                    Error::Invalid(format!("session supervisor failed during close: {error}"))
                })?;
                self.supervisor.take();
            }
            return Ok(CloseOutcome::Closed);
        }
        Ok(outcome.expect("a still-closing outcome was received"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Best effort: a dropped owner must not leave the session running
        // unattended, but closing is asynchronous and callers that need the
        // guarantee await `close`.
        let (reply, _) = oneshot::channel();
        let _ = self.requests.try_send(Request::Close { reply });
    }
}
