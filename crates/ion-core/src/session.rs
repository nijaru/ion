//! Passive Session ownership and the first replacement runtime commands.
//!
//! Creating/opening a Session starts only the bounded database command service.
//! Provider/tool reconciliation and drive are explicit later operations; open is
//! semantically passive.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use thiserror::Error;

use crate::observation::{ObservationHub, Subscription};
use crate::store::{SessionStore, StoreError};
use crate::{
    CommitReceipt, CommitSeq, ConfigError, Conversation, ConversationConfig, ConversationId, Entry,
    EntryPage, Input, InputBody, InputMode, InputSender, InstalledConfig, ObservationError,
    RequestKey, SessionId, SessionSnapshot, SnapshotRequest, SnapshotWatch, Turn, TurnId,
    WatchRequest,
};

const HEALTH_OPEN: u8 = 0;
const HEALTH_FENCED: u8 = 1;
const HEALTH_CLOSING: u8 = 2;
const HEALTH_CLOSED: u8 = 3;

pub struct Session {
    inner: Arc<SessionInner>,
    closed: bool,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("session_id", &self.inner.session_id)
            .field("primary_conversation", &self.inner.primary_conversation)
            .field("health", &self.health())
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct SessionHandle {
    inner: Arc<SessionInner>,
}

struct SessionInner {
    session_id: SessionId,
    primary_conversation: ConversationId,
    store: SessionStore,
    observations: ObservationHub,
    health: AtomicU8,
}

#[derive(Debug)]
pub struct CreatedSession {
    pub session: Session,
    pub receipt: CommitReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionHealth {
    Open,
    Fenced,
    Closing,
    Closed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdmitInputRequest {
    pub sender: InputSender,
    pub mode: InputMode,
    pub request_key: Option<RequestKey>,
    pub body: InputBody,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Admission {
    Created {
        input: Input,
        admitted_at: CommitSeq,
        receipt: CommitReceipt,
    },
    Replayed {
        input: Input,
        admitted_at: CommitSeq,
    },
}

impl Admission {
    #[must_use]
    pub const fn input(&self) -> &Input {
        match self {
            Self::Created { input, .. } | Self::Replayed { input, .. } => input,
        }
    }

    #[must_use]
    pub const fn admitted_at(&self) -> CommitSeq {
        match self {
            Self::Created { admitted_at, .. } | Self::Replayed { admitted_at, .. } => *admitted_at,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartTurnRequest {
    pub conversation: ConversationId,
    pub input: crate::InputId,
    pub admitted_at_unix_ms: i64,
    pub wall_deadline_unix_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StartedTurn {
    pub turn: Turn,
    pub input: Input,
    pub entry: Entry,
    pub receipt: CommitReceipt,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CreatedConversation {
    pub conversation: Conversation,
    pub config: InstalledConfig,
    pub receipt: CommitReceipt,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredConversation {
    pub conversation: Conversation,
    pub config: InstalledConfig,
    pub receipt: CommitReceipt,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CancellationResult {
    Committed { turn: Turn, receipt: CommitReceipt },
    AlreadyRequested(Turn),
    Terminal(Turn),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AbandonResult {
    Committed {
        turn: Turn,
        receipt: CommitReceipt,
    },
    Terminal(Turn),
}

impl Session {
    pub async fn create(
        path: impl AsRef<Path>,
        config: ConversationConfig,
    ) -> Result<CreatedSession, SessionError> {
        config.validate()?;
        let observations = ObservationHub::new();
        let session_id = SessionId::new();
        let (store, metadata, receipt) =
            SessionStore::create(path.as_ref(), session_id, config, observations.clone()).await?;
        let session = Self {
            inner: Arc::new(SessionInner {
                session_id: metadata.session_id,
                primary_conversation: metadata.primary_conversation,
                store,
                observations,
                health: AtomicU8::new(HEALTH_OPEN),
            }),
            closed: false,
        };
        Ok(CreatedSession { session, receipt })
    }

    pub async fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        let observations = ObservationHub::new();
        let (store, metadata) = SessionStore::open(path.as_ref(), observations.clone()).await?;
        Ok(Self {
            inner: Arc::new(SessionInner {
                session_id: metadata.session_id,
                primary_conversation: metadata.primary_conversation,
                store,
                observations,
                health: AtomicU8::new(HEALTH_OPEN),
            }),
            closed: false,
        })
    }

    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.inner.session_id
    }

    #[must_use]
    pub const fn primary_conversation(&self) -> ConversationId {
        self.inner.primary_conversation
    }

    #[must_use]
    pub fn handle(&self) -> SessionHandle {
        SessionHandle {
            inner: Arc::clone(&self.inner),
        }
    }

    #[must_use]
    pub fn health(&self) -> SessionHealth {
        self.inner.health()
    }

    pub async fn close(mut self) -> Result<(), SessionError> {
        self.inner
            .health
            .compare_exchange(
                HEALTH_OPEN,
                HEALTH_CLOSING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .or_else(|current| {
                if current == HEALTH_FENCED {
                    self.inner.health.store(HEALTH_CLOSING, Ordering::Release);
                    Ok(HEALTH_FENCED)
                } else {
                    Err(current)
                }
            })
            .map_err(|_| SessionError::Closed)?;
        self.inner.store.shutdown().await?;
        self.inner.health.store(HEALTH_CLOSED, Ordering::Release);
        self.closed = true;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let current = self.inner.health.load(Ordering::Acquire);
        if current == HEALTH_OPEN || current == HEALTH_FENCED {
            self.inner.health.store(HEALTH_CLOSING, Ordering::Release);
            self.inner.store.try_shutdown();
        }
    }
}

impl SessionHandle {
    #[must_use]
    pub const fn session_id(&self) -> SessionId {
        self.inner.session_id
    }

    #[must_use]
    pub const fn primary_conversation(&self) -> ConversationId {
        self.inner.primary_conversation
    }

    #[must_use]
    pub fn health(&self) -> SessionHealth {
        self.inner.health()
    }

    pub async fn create_conversation(
        &self,
        config: ConversationConfig,
    ) -> Result<CreatedConversation, SessionError> {
        config.validate()?;
        self.ensure_mutable()?;
        self.observe(self.inner.store.create_conversation(config).await)
    }

    pub async fn current_config(
        &self,
        conversation: ConversationId,
    ) -> Result<InstalledConfig, SessionError> {
        self.ensure_readable()?;
        self.observe(self.inner.store.current_config(conversation).await)
    }

    pub async fn config_as_of(
        &self,
        conversation: ConversationId,
        revision: CommitSeq,
    ) -> Result<InstalledConfig, SessionError> {
        self.ensure_readable()?;
        self.observe(self.inner.store.config_as_of(conversation, revision).await)
    }

    pub async fn configure(
        &self,
        conversation: ConversationId,
        expected_revision: CommitSeq,
        config: ConversationConfig,
    ) -> Result<ConfiguredConversation, SessionError> {
        config.validate()?;
        self.ensure_mutable()?;
        self.observe(
            self.inner
                .store
                .configure(conversation, expected_revision, config)
                .await,
        )
    }

    pub async fn admit_input(
        &self,
        conversation: ConversationId,
        request: AdmitInputRequest,
    ) -> Result<Admission, SessionError> {
        self.ensure_mutable()?;
        self.observe(self.inner.store.admit_input(conversation, request).await)
    }

    pub async fn start_turn(&self, request: StartTurnRequest) -> Result<StartedTurn, SessionError> {
        self.ensure_mutable()?;
        self.observe(self.inner.store.start_turn(request).await)
    }

    pub async fn cancel_turn(&self, turn: TurnId) -> Result<CancellationResult, SessionError> {
        self.ensure_mutable()?;
        self.observe(self.inner.store.cancel_turn(turn).await)
    }

    pub async fn abandon_turn(&self, turn: TurnId) -> Result<AbandonResult, SessionError> {
        self.ensure_mutable()?;
        self.observe(self.inner.store.abandon_turn(turn).await)
    }

    pub async fn snapshot(
        &self,
        request: SnapshotRequest,
    ) -> Result<SessionSnapshot, SessionError> {
        request.validate()?;
        self.ensure_readable()?;
        self.observe(self.inner.store.snapshot(request).await)
    }

    pub async fn snapshot_and_watch(
        &self,
        request: WatchRequest,
    ) -> Result<SnapshotWatch, SessionError> {
        let request = request.validate()?;
        self.ensure_readable()?;

        // Subscription is established before the snapshot command enters the
        // database queue. Commits during snapshot acquisition are therefore
        // either covered by the snapshot or already queued on this subscriber.
        let subscription: Subscription = self.inner.observations.subscribe(request.queue)?;
        let snapshot = self.observe(self.inner.store.snapshot(request.snapshot).await)?;
        let watch = subscription.handoff(snapshot.coverage)?;
        Ok(SnapshotWatch { snapshot, watch })
    }

    pub async fn page_entries(
        &self,
        conversation: ConversationId,
        before: Option<crate::EntryId>,
        limit: usize,
    ) -> Result<EntryPage, SessionError> {
        self.ensure_readable()?;
        self.observe(
            self.inner
                .store
                .page_entries(conversation, before, limit)
                .await,
        )
    }

    fn ensure_readable(&self) -> Result<(), SessionError> {
        match self.health() {
            SessionHealth::Open | SessionHealth::Fenced => Ok(()),
            SessionHealth::Closing | SessionHealth::Closed => Err(SessionError::Closed),
        }
    }

    fn ensure_mutable(&self) -> Result<(), SessionError> {
        match self.health() {
            SessionHealth::Open => Ok(()),
            SessionHealth::Fenced => Err(SessionError::Fenced(
                "session mutation is fenced after a storage ambiguity".to_owned(),
            )),
            SessionHealth::Closing | SessionHealth::Closed => Err(SessionError::Closed),
        }
    }

    fn observe<T>(&self, result: Result<T, StoreError>) -> Result<T, SessionError> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                if error.requires_fence() {
                    self.inner.health.store(HEALTH_FENCED, Ordering::Release);
                }
                Err(error.into())
            }
        }
    }
}

impl SessionInner {
    fn health(&self) -> SessionHealth {
        match self.health.load(Ordering::Acquire) {
            HEALTH_OPEN => SessionHealth::Open,
            HEALTH_FENCED => SessionHealth::Fenced,
            HEALTH_CLOSING => SessionHealth::Closing,
            HEALTH_CLOSED => SessionHealth::Closed,
            _ => SessionHealth::Fenced,
        }
    }
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Observation(#[from] ObservationError),
    #[error("request key {request_key:?} conflicts in conversation {conversation}")]
    RequestKeyConflict {
        conversation: ConversationId,
        request_key: String,
    },
    #[error("conversation {0} already has an unfinished turn")]
    ConversationBusy(ConversationId),
    #[error("{kind} {id} was not found")]
    NotFound { kind: &'static str, id: i64 },
    #[error(
        "conversation {conversation} configuration revision changed: expected {expected}, found {actual}"
    )]
    RevisionConflict {
        conversation: ConversationId,
        expected: CommitSeq,
        actual: CommitSeq,
    },
    #[error("invalid session operation: {0}")]
    InvalidState(String),
    #[error("session mutation is fenced: {0}")]
    Fenced(String),
    #[error("session is closing or closed")]
    Closed,
    #[error("session storage error: {0}")]
    Storage(String),
}

impl From<StoreError> for SessionError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::RequestKeyConflict {
                conversation,
                request_key,
            } => Self::RequestKeyConflict {
                conversation,
                request_key,
            },
            StoreError::ConversationBusy(conversation) => Self::ConversationBusy(conversation),
            StoreError::NotFound { kind, id } => Self::NotFound { kind, id },
            StoreError::RevisionConflict {
                conversation,
                expected,
                actual,
            } => Self::RevisionConflict {
                conversation,
                expected,
                actual,
            },
            StoreError::InvalidState(message) | StoreError::InvalidRequest(message) => {
                Self::InvalidState(message)
            }
            StoreError::SnapshotTooLarge { maximum } => {
                Self::Observation(ObservationError::SnapshotTooLarge { maximum })
            }
            StoreError::Fenced { cause } => Self::Fenced(cause),
            StoreError::Closed => Self::Closed,
            other => Self::Storage(other.to_string()),
        }
    }
}
