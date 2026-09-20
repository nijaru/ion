//! Dedicated SQLite database-thread ownership for one Session.

mod sqlite;

use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

use crate::observation::ObservationHub;
use crate::session::{
    AbandonResult, Admission, AdmitInputRequest, CancellationResult, ConfiguredConversation,
    CreatedConversation, StartTurnRequest, StartedTurn,
};
use crate::{
    CommitReceipt, CommitSeq, ConversationConfig, ConversationId, EntryId, EntryPage,
    InstalledConfig, SessionId, SessionSnapshot, SnapshotRequest, TurnId,
};

const COMMAND_CAPACITY: usize = 64;

#[derive(Debug, Clone, Copy)]
pub(crate) struct StoreMetadata {
    pub(crate) session_id: SessionId,
    pub(crate) primary_conversation: ConversationId,
}

#[derive(Clone)]
pub(crate) struct SessionStore {
    tx: mpsc::Sender<Command>,
}

impl std::fmt::Debug for SessionStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionStore")
            .finish_non_exhaustive()
    }
}

impl SessionStore {
    pub(crate) async fn create(
        path: &Path,
        session_id: SessionId,
        config: ConversationConfig,
        observations: ObservationHub,
    ) -> Result<(Self, StoreMetadata, CommitReceipt), StoreError> {
        let path = path.to_path_buf();
        let (tx, rx) = mpsc::channel(COMMAND_CAPACITY);
        let (startup_tx, startup_rx) = oneshot::channel();

        std::thread::Builder::new()
            .name(format!("ion-db-{session_id}"))
            .spawn(move || {
                let startup =
                    sqlite::SqliteDatabase::create(&path, session_id, config, observations);
                match startup {
                    Ok((database, metadata, receipt)) => {
                        let _ = startup_tx.send(Ok((metadata, receipt)));
                        run(database, rx);
                    }
                    Err(error) => {
                        let _ = startup_tx.send(Err(error));
                    }
                }
            })
            .map_err(|error| StoreError::Io(format!("cannot start database thread: {error}")))?;

        let (metadata, receipt) = startup_rx.await.map_err(|_| StoreError::Closed)??;
        Ok((Self { tx }, metadata, receipt))
    }

    pub(crate) async fn open(
        path: &Path,
        observations: ObservationHub,
    ) -> Result<(Self, StoreMetadata), StoreError> {
        let path = path.to_path_buf();
        let (tx, rx) = mpsc::channel(COMMAND_CAPACITY);
        let (startup_tx, startup_rx) = oneshot::channel();

        std::thread::Builder::new()
            .name("ion-db-open".to_owned())
            .spawn(move || {
                let startup = sqlite::SqliteDatabase::open(&path, observations);
                match startup {
                    Ok((database, metadata)) => {
                        let _ = startup_tx.send(Ok(metadata));
                        run(database, rx);
                    }
                    Err(error) => {
                        let _ = startup_tx.send(Err(error));
                    }
                }
            })
            .map_err(|error| StoreError::Io(format!("cannot start database thread: {error}")))?;

        let metadata = startup_rx.await.map_err(|_| StoreError::Closed)??;
        Ok((Self { tx }, metadata))
    }

    pub(crate) async fn create_conversation(
        &self,
        config: ConversationConfig,
    ) -> Result<CreatedConversation, StoreError> {
        self.call(|reply| Command::CreateConversation { config, reply })
            .await
    }

    pub(crate) async fn current_config(
        &self,
        conversation: ConversationId,
    ) -> Result<InstalledConfig, StoreError> {
        self.call(|reply| Command::CurrentConfig {
            conversation,
            reply,
        })
        .await
    }

    pub(crate) async fn config_as_of(
        &self,
        conversation: ConversationId,
        revision: CommitSeq,
    ) -> Result<InstalledConfig, StoreError> {
        self.call(|reply| Command::ConfigAsOf {
            conversation,
            revision,
            reply,
        })
        .await
    }

    pub(crate) async fn configure(
        &self,
        conversation: ConversationId,
        expected_revision: CommitSeq,
        config: ConversationConfig,
    ) -> Result<ConfiguredConversation, StoreError> {
        self.call(|reply| Command::Configure {
            conversation,
            expected_revision,
            config,
            reply,
        })
        .await
    }

    pub(crate) async fn admit_input(
        &self,
        conversation: ConversationId,
        request: AdmitInputRequest,
    ) -> Result<Admission, StoreError> {
        self.call(|reply| Command::AdmitInput {
            conversation,
            request,
            reply,
        })
        .await
    }

    pub(crate) async fn start_turn(
        &self,
        request: StartTurnRequest,
    ) -> Result<StartedTurn, StoreError> {
        self.call(|reply| Command::StartTurn { request, reply })
            .await
    }

    pub(crate) async fn cancel_turn(&self, turn: TurnId) -> Result<CancellationResult, StoreError> {
        self.call(|reply| Command::CancelTurn { turn, reply }).await
    }

    pub(crate) async fn abandon_turn(&self, turn: TurnId) -> Result<AbandonResult, StoreError> {
        self.call(|reply| Command::AbandonTurn { turn, reply })
            .await
    }

    pub(crate) async fn snapshot(
        &self,
        request: SnapshotRequest,
    ) -> Result<SessionSnapshot, StoreError> {
        self.call(|reply| Command::Snapshot { request, reply })
            .await
    }

    pub(crate) async fn page_entries(
        &self,
        conversation: ConversationId,
        before: Option<EntryId>,
        limit: usize,
    ) -> Result<EntryPage, StoreError> {
        self.call(|reply| Command::PageEntries {
            conversation,
            before,
            limit,
            reply,
        })
        .await
    }

    pub(crate) async fn shutdown(&self) -> Result<(), StoreError> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(Command::Shutdown { reply: Some(reply) })
            .await
            .map_err(|_| StoreError::Closed)?;
        receive.await.map_err(|_| StoreError::Closed)
    }

    pub(crate) fn try_shutdown(&self) {
        let _ = self.tx.try_send(Command::Shutdown { reply: None });
    }

    async fn call<T>(
        &self,
        build: impl FnOnce(oneshot::Sender<Result<T, StoreError>>) -> Command,
    ) -> Result<T, StoreError> {
        let (reply, receive) = oneshot::channel();
        self.tx
            .send(build(reply))
            .await
            .map_err(|_| StoreError::Closed)?;
        receive.await.map_err(|_| StoreError::Closed)?
    }
}

enum Command {
    CreateConversation {
        config: ConversationConfig,
        reply: oneshot::Sender<Result<CreatedConversation, StoreError>>,
    },
    CurrentConfig {
        conversation: ConversationId,
        reply: oneshot::Sender<Result<InstalledConfig, StoreError>>,
    },
    ConfigAsOf {
        conversation: ConversationId,
        revision: CommitSeq,
        reply: oneshot::Sender<Result<InstalledConfig, StoreError>>,
    },
    Configure {
        conversation: ConversationId,
        expected_revision: CommitSeq,
        config: ConversationConfig,
        reply: oneshot::Sender<Result<ConfiguredConversation, StoreError>>,
    },
    AdmitInput {
        conversation: ConversationId,
        request: AdmitInputRequest,
        reply: oneshot::Sender<Result<Admission, StoreError>>,
    },
    StartTurn {
        request: StartTurnRequest,
        reply: oneshot::Sender<Result<StartedTurn, StoreError>>,
    },
    CancelTurn {
        turn: TurnId,
        reply: oneshot::Sender<Result<CancellationResult, StoreError>>,
    },
    AbandonTurn {
        turn: TurnId,
        reply: oneshot::Sender<Result<AbandonResult, StoreError>>,
    },
    Snapshot {
        request: SnapshotRequest,
        reply: oneshot::Sender<Result<SessionSnapshot, StoreError>>,
    },
    PageEntries {
        conversation: ConversationId,
        before: Option<EntryId>,
        limit: usize,
        reply: oneshot::Sender<Result<EntryPage, StoreError>>,
    },
    Shutdown {
        reply: Option<oneshot::Sender<()>>,
    },
}

fn run(mut database: sqlite::SqliteDatabase, mut rx: mpsc::Receiver<Command>) {
    while let Some(command) = rx.blocking_recv() {
        match command {
            Command::CreateConversation { config, reply } => {
                let _ = reply.send(database.create_conversation(config));
            }
            Command::CurrentConfig {
                conversation,
                reply,
            } => {
                let _ = reply.send(database.current_config(conversation));
            }
            Command::ConfigAsOf {
                conversation,
                revision,
                reply,
            } => {
                let _ = reply.send(database.config_as_of(conversation, revision));
            }
            Command::Configure {
                conversation,
                expected_revision,
                config,
                reply,
            } => {
                let _ = reply.send(database.configure(conversation, expected_revision, config));
            }
            Command::AdmitInput {
                conversation,
                request,
                reply,
            } => {
                let _ = reply.send(database.admit_input(conversation, request));
            }
            Command::StartTurn { request, reply } => {
                let _ = reply.send(database.start_turn(request));
            }
            Command::CancelTurn { turn, reply } => {
                let _ = reply.send(database.cancel_turn(turn));
            }
            Command::AbandonTurn { turn, reply } => {
                let _ = reply.send(database.abandon_turn(turn));
            }
            Command::Snapshot { request, reply } => {
                let _ = reply.send(database.snapshot(request));
            }
            Command::PageEntries {
                conversation,
                before,
                limit,
                reply,
            } => {
                let _ = reply.send(database.page_entries(conversation, before, limit));
            }
            Command::Shutdown { reply } => {
                drop(database);
                if let Some(reply) = reply {
                    let _ = reply.send(());
                }
                return;
            }
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum StoreError {
    #[error("session database already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("session database does not exist: {0}")]
    Unknown(PathBuf),
    #[error("session database is owned by another process: {0}")]
    InUse(PathBuf),
    #[error("unsupported session schema {found}; expected {expected}")]
    UnsupportedSchema { found: i64, expected: i64 },
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
    #[error("invalid session state: {0}")]
    InvalidState(String),
    #[error("invalid session request: {0}")]
    InvalidRequest(String),
    #[error("snapshot mandatory state exceeds the requested {maximum}-byte bound")]
    SnapshotTooLarge { maximum: usize },
    #[error("session mutation is fenced after ambiguous persistence: {cause}")]
    Fenced { cause: String },
    #[error("session database command service is closed")]
    Closed,
    #[error("corrupt session database: {0}")]
    Corrupt(String),
    #[error("sqlite failure: {0}")]
    Sqlite(String),
    #[error("filesystem failure: {0}")]
    Io(String),
}

impl StoreError {
    pub(crate) fn requires_fence(&self) -> bool {
        matches!(self, Self::Fenced { .. } | Self::Corrupt(_))
    }

    pub(crate) fn is_semantic_rejection(&self) -> bool {
        matches!(
            self,
            Self::RequestKeyConflict { .. }
                | Self::ConversationBusy(_)
                | Self::NotFound { .. }
                | Self::RevisionConflict { .. }
                | Self::InvalidState(_)
                | Self::InvalidRequest(_)
                | Self::SnapshotTooLarge { .. }
        )
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}
