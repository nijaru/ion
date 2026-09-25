//! Passive Session ownership and the first replacement runtime commands.
//!
//! Creating/opening a Session starts only the bounded database command service.
//! Provider/tool reconciliation and drive are explicit later operations; open is
//! semantically passive.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use thiserror::Error;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::effect_gate::{EffectGate, EffectGates, signal};
use crate::observation::{ObservationHub, Subscription};
use crate::store::{SessionStore, StoreError};
use crate::{
    CommitReceipt, CommitSeq, ConfigError, Conversation, ConversationConfig, ConversationId,
    DriveExit, DrivePolicy, Entry, EntryPage, Input, InputBody, InputMode, InputSender,
    InstalledConfig, ModelBoundaries, ObservationError, RequestKey, SessionId, SessionSnapshot,
    SnapshotRequest, SnapshotWatch, Turn, TurnId, WatchRequest,
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

#[derive(Debug)]
pub(crate) struct SessionInner {
    session_id: SessionId,
    primary_conversation: ConversationId,
    store: SessionStore,
    observations: ObservationHub,
    health: AtomicU8,
    effects: EffectGates,
    drives: Mutex<HashMap<TurnId, watch::Receiver<Option<DriveExit>>>>,
    joins: Mutex<Vec<JoinHandle<()>>>,
    #[cfg(test)]
    registration_pause: Mutex<Option<RegistrationPause>>,
}

#[cfg(test)]
#[derive(Debug)]
struct RegistrationPause {
    reached: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
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
    Committed { turn: Turn, receipt: CommitReceipt },
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
                effects: EffectGates::default(),
                drives: Mutex::new(HashMap::new()),
                joins: Mutex::new(Vec::new()),
                #[cfg(test)]
                registration_pause: Mutex::new(None),
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
                effects: EffectGates::default(),
                drives: Mutex::new(HashMap::new()),
                joins: Mutex::new(Vec::new()),
                #[cfg(test)]
                registration_pause: Mutex::new(None),
            }),
            closed: false,
        })
    }

    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.inner.session_id
    }

    #[must_use]
    pub fn primary_conversation(&self) -> ConversationId {
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
        signal(self.inner.effects.seal_all());
        let joins = {
            // Registration holds drives through join-list insertion. Once health
            // is Closing, this lock drains any registrar that already won admission.
            let _drives = self.inner.drives.lock().expect("drive map poisoned");
            let mut joins = self.inner.joins.lock().expect("drive join list poisoned");
            std::mem::take(&mut *joins)
        };
        for join in joins {
            let _ = join.await;
        }

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
            signal(self.inner.effects.seal_all());
        }
    }
}

impl SessionHandle {
    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.inner.session_id
    }

    #[must_use]
    pub fn primary_conversation(&self) -> ConversationId {
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
        let tokens = self.inner.effects.begin_abort(turn);
        let result = self.observe(self.inner.store.cancel_turn(turn).await);
        signal(tokens);
        if matches!(result, Ok(CancellationResult::Terminal(_))) {
            self.inner.effects.retire(turn);
        }
        result
    }

    pub async fn resume(
        &self,
        turn: TurnId,
        boundaries: ModelBoundaries,
    ) -> Result<DriveExit, SessionError> {
        self.resume_with_policy(turn, boundaries, DrivePolicy::default())
            .await
    }

    pub async fn resume_with_policy(
        &self,
        turn: TurnId,
        boundaries: ModelBoundaries,
        policy: DrivePolicy,
    ) -> Result<DriveExit, SessionError> {
        self.resume_with_tools(turn, boundaries, crate::ToolBoundaries::default(), policy)
            .await
    }

    pub async fn tool_records(
        &self,
        step: crate::StepId,
    ) -> Result<crate::ToolRecords, SessionError> {
        self.ensure_readable()?;
        self.observe(self.inner.store.tool_records(step).await)
    }

    /// Recover execution evidence even after exchange/Turn settlement. Never starts work.
    pub async fn reconcile_tools(
        &self,
        step: crate::StepId,
        tools: crate::ToolBoundaries,
    ) -> Result<DriveExit, SessionError> {
        self.ensure_mutable()?;
        let turn = self.tool_records(step).await?.turn.id;
        let inner = Arc::clone(&self.inner);
        self.drive_owned(turn, false, async move {
            match crate::tool_drive::reconcile(&inner, step, &tools).await {
                Ok(()) => DriveExit::Stopped { turn },
                Err(error) => DriveExit::Faulted {
                    turn,
                    message: error.to_string(),
                },
            }
        })
        .await
    }

    /// Record an explicit, targeted approval decision. The application must
    /// authenticate the user/host caller; ordinary text is never an approval.
    /// The digest identifies the exact saved action shown for inspection.
    pub async fn decide_tool_approval(
        &self,
        step: crate::StepId,
        invocation: crate::InvocationId,
        action_digest: crate::ContentDigest,
        decision: crate::ApprovalDecision,
        executor: crate::SemanticCompatibilityId,
    ) -> Result<Option<CommitReceipt>, SessionError> {
        self.ensure_mutable()?;
        let result = self.observe(
            self.inner
                .store
                .tool_mutate(crate::store::ToolMutation::DecideApproval {
                    step,
                    invocation,
                    action_digest,
                    decision,
                    executor,
                    now_unix_ms: crate::tool_drive::now_unix_ms()?,
                })
                .await,
        )?;
        Ok(result.receipt)
    }

    /// Explicitly settle the exchange as unknown without changing execution truth.
    pub async fn accept_tool_unknown(
        &self,
        step: crate::StepId,
        invocation: crate::InvocationId,
    ) -> Result<CommitReceipt, SessionError> {
        self.ensure_mutable()?;
        let result = self.observe(
            self.inner
                .store
                .tool_mutate(crate::store::ToolMutation::Stage {
                    step,
                    invocation,
                    source: crate::OutcomeSource::AcceptedUnknown,
                })
                .await,
        )?;
        result
            .receipt
            .ok_or_else(|| SessionError::InvalidState("tool settlement returned no commit".into()))
    }

    pub async fn resume_with_tools(
        &self,
        turn: TurnId,
        boundaries: ModelBoundaries,
        tools: crate::ToolBoundaries,
        policy: DrivePolicy,
    ) -> Result<DriveExit, SessionError> {
        let inner = Arc::clone(&self.inner);
        self.drive_owned(
            turn,
            true,
            crate::drive::run(inner, turn, boundaries, tools, policy),
        )
        .await
    }

    async fn drive_owned(
        &self,
        turn: TurnId,
        join_existing: bool,
        future: impl std::future::Future<Output = DriveExit> + Send + 'static,
    ) -> Result<DriveExit, SessionError> {
        self.ensure_mutable()?;
        #[cfg(test)]
        {
            let pause = self
                .inner
                .registration_pause
                .lock()
                .expect("test pause lock")
                .take();
            if let Some(pause) = pause {
                let _ = pause.reached.send(());
                let _ = pause.release.await;
            }
        }

        let mut receiver = {
            let mut drives = self.inner.drives.lock().expect("drive map poisoned");
            // Linearize registration with close's collection of owned joins.
            self.ensure_mutable()?;
            if let Some(existing) = drives.get(&turn) {
                if !join_existing {
                    return Err(SessionError::InvalidState(
                        "turn drive is already active".into(),
                    ));
                }
                existing.clone()
            } else {
                let (sender, receiver) = watch::channel(None);
                drives.insert(turn, receiver.clone());

                let inner = Arc::clone(&self.inner);
                let task_inner = Arc::clone(&inner);
                let join = tokio::spawn(async move {
                    let exit = std::panic::AssertUnwindSafe(future)
                        .catch_unwind()
                        .await
                        .unwrap_or_else(|_| DriveExit::Faulted {
                            turn,
                            message: "drive task panicked".to_owned(),
                        });
                    if matches!(exit, DriveExit::Settled(_)) {
                        task_inner.effects.retire(turn);
                    }
                    let _ = sender.send(Some(exit));
                    task_inner
                        .drives
                        .lock()
                        .expect("drive map poisoned")
                        .remove(&turn);
                });

                let mut joins = inner.joins.lock().expect("drive join list poisoned");
                joins.retain(|join| !join.is_finished());
                joins.push(join);
                receiver
            }
        };

        loop {
            if let Some(exit) = receiver.borrow().clone() {
                return Ok(exit);
            }
            receiver.changed().await.map_err(|_| SessionError::Closed)?;
        }
    }

    pub async fn abandon_turn(&self, turn: TurnId) -> Result<AbandonResult, SessionError> {
        self.ensure_mutable()?;
        let result = self.observe(self.inner.store.abandon_turn(turn).await);
        if result.is_ok() {
            self.inner.effects.retire(turn);
        }
        result
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
        self.inner.observe_store(result).map_err(Into::into)
    }
}

impl SessionInner {
    pub(crate) fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub(crate) fn store(&self) -> &SessionStore {
        &self.store
    }

    pub(crate) fn effect_gate(&self, turn: TurnId) -> Arc<EffectGate> {
        self.effects.gate(turn)
    }

    pub(crate) fn health(&self) -> SessionHealth {
        match self.health.load(Ordering::Acquire) {
            HEALTH_OPEN => SessionHealth::Open,
            HEALTH_FENCED => SessionHealth::Fenced,
            HEALTH_CLOSING => SessionHealth::Closing,
            HEALTH_CLOSED => SessionHealth::Closed,
            _ => SessionHealth::Fenced,
        }
    }

    pub(crate) fn observe_store<T>(&self, result: Result<T, StoreError>) -> Result<T, StoreError> {
        if let Err(error) = &result
            && error.requires_fence()
        {
            self.health.store(HEALTH_FENCED, Ordering::Release);
            signal(self.effects.seal_all());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn abandoned_turn_retires_its_gate_without_reopening_old_references() {
        let root = std::env::temp_dir().join(format!("ion-gate-retirement-{}", SessionId::new()));
        std::fs::create_dir_all(&root).unwrap();
        let session = Session::create(root.join("session.sqlite"), crate::config::tests::config())
            .await
            .unwrap()
            .session;
        let handle = session.handle();
        for _ in 0..32 {
            let input = handle
                .admit_input(
                    session.primary_conversation(),
                    AdmitInputRequest {
                        sender: crate::InputSender::User,
                        mode: crate::InputMode::Submit,
                        request_key: None,
                        body: crate::InputBody::Text("test".into()),
                    },
                )
                .await
                .unwrap();
            let turn = handle
                .start_turn(StartTurnRequest {
                    conversation: session.primary_conversation(),
                    input: input.input().id,
                    admitted_at_unix_ms: 0,
                    wall_deadline_unix_ms: None,
                })
                .await
                .unwrap()
                .turn
                .id;
            let old = session.inner.effect_gate(turn);
            handle.abandon_turn(turn).await.unwrap();
            assert!(old.admit().is_none());
        }
        assert_eq!(session.inner.effects.len(), 0);
        session.close().await.unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn close_rejects_a_drive_paused_before_registration() {
        let root =
            std::env::temp_dir().join(format!("ion-close-registration-{}", SessionId::new()));
        std::fs::create_dir_all(&root).unwrap();
        let session = Session::create(root.join("session.sqlite"), crate::config::tests::config())
            .await
            .unwrap()
            .session;
        let (reached, at_pause) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        *session.inner.registration_pause.lock().unwrap() = Some(RegistrationPause {
            reached,
            release: released,
        });
        let handle = session.handle();
        let executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = Arc::clone(&executed);
        let drive = tokio::spawn(async move {
            let turn = TurnId::new(1).unwrap();
            handle
                .drive_owned(turn, false, async move {
                    probe.store(true, Ordering::SeqCst);
                    DriveExit::Stopped { turn }
                })
                .await
        });
        at_pause.await.unwrap();
        session.close().await.unwrap();
        release.send(()).unwrap();
        assert!(matches!(drive.await.unwrap(), Err(SessionError::Closed)));
        assert!(
            !executed.load(Ordering::SeqCst),
            "no late backend reconciliation after ownership release"
        );
        std::fs::remove_dir_all(root).unwrap();
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
            StoreError::ApprovalRequired => Self::InvalidState("tool approval required".into()),
            StoreError::SnapshotTooLarge { maximum } => {
                Self::Observation(ObservationError::SnapshotTooLarge { maximum })
            }
            StoreError::Fenced { cause } => Self::Fenced(cause),
            StoreError::Closed => Self::Closed,
            other => Self::Storage(other.to_string()),
        }
    }
}
