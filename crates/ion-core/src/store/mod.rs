//! Persistence ownership.
//!
//! One session owns one database file. A dedicated thread owns the SQLite
//! connection and every transaction; async callers submit typed commands over a
//! bounded channel and receive owned results. No borrowed statement, row or
//! transaction crosses that boundary, and a dropped reply receiver never
//! cancels the mutation it asked for.
//!
//! SQLite is the only authority. There is no resident mirror, no undo journal
//! and no second in-memory copy of semantic state to keep consistent.

pub(crate) mod sqlite;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};

use crate::error::Error;
use crate::limits::SessionLimits;

pub(crate) use sqlite::SqliteStore;

/// A persistence operation failed, or was refused by a semantic rule.
#[derive(Debug)]
pub(crate) enum StoreError {
    /// The store is healthy; the operation violated a rule the caller can act
    /// on (busy conversation, conflicting request key, quota, stale revision).
    Rejected(Error),
    /// Persistence itself failed. The session is fenced: an unclear storage
    /// outcome is not a state the engine may keep writing through.
    Failed(String),
}

impl StoreError {
    pub(crate) fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }

    pub(crate) fn is_fenced(&self) -> bool {
        matches!(self, Self::Failed(_))
    }

    pub(crate) fn into_error(self) -> Error {
        match self {
            Self::Rejected(error) => error,
            Self::Failed(message) => Error::Persistence(message),
        }
    }

    /// Whether this was a semantic refusal rather than a storage failure.
    pub(crate) fn is_rejected(&self) -> bool {
        matches!(self, Self::Rejected(_))
    }
}

impl From<Error> for StoreError {
    fn from(error: Error) -> Self {
        Self::Rejected(error)
    }
}

/// A typed semantic operation.
///
/// Implementing this rather than matching a global command enum keeps each
/// transaction's arguments and result in one place, and keeps the transport
/// that carries them from becoming a second schema.
pub(crate) trait Command: Send + 'static {
    type Output: Send + 'static;

    fn apply(self, store: &mut SqliteStore) -> Result<Self::Output, StoreError>;
}

type Job = Box<dyn FnOnce(&mut SqliteStore) -> Result<(), StoreError> + Send>;

struct Inner {
    /// `None` once the owning session has closed the queue.
    commands: Mutex<Option<mpsc::Sender<Job>>>,
    fenced: AtomicBool,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// The database thread and the bounded command queue in front of it.
#[derive(Clone)]
pub(crate) struct Db {
    inner: Arc<Inner>,
    /// Only the handle that started the thread may close it.
    owner: bool,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Db")
            .field("fenced", &self.inner.fenced.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Db {
    /// Start the database thread over an already opened store.
    ///
    /// Starting does not read semantic state: a freshly created database has
    /// none yet, and an existing one is read by the caller's first command.
    pub(crate) fn start(store: SqliteStore, limits: SessionLimits) -> Result<Self, StoreError> {
        let mut store = store;
        let (commands, mut queue) = mpsc::channel::<Job>(limits.command_capacity);
        let inner = Arc::new(Inner {
            commands: Mutex::new(Some(commands)),
            fenced: AtomicBool::new(false),
            thread: Mutex::new(None),
        });
        let thread_inner = Arc::clone(&inner);
        let thread = std::thread::Builder::new()
            .name("ion-db".to_owned())
            .spawn(move || {
                while let Some(job) = queue.blocking_recv() {
                    if thread_inner.fenced.load(Ordering::Relaxed) {
                        continue;
                    }
                    if (job)(&mut store).is_err() {
                        thread_inner.fenced.store(true, Ordering::SeqCst);
                    }
                }
            })
            .map_err(|error| {
                StoreError::failed(format!("cannot start database thread: {error}"))
            })?;
        *inner.thread.lock().expect("thread mutex") = Some(thread);
        Ok(Self { inner, owner: true })
    }

    /// Whether a persistence failure has fenced this session.
    pub(crate) fn is_fenced(&self) -> bool {
        self.inner.fenced.load(Ordering::Relaxed)
    }

    pub(crate) async fn run<C: Command>(&self, command: C) -> Result<C::Output, StoreError> {
        if self.is_fenced() {
            return Err(StoreError::failed(
                "the session fenced after a persistence failure",
            ));
        }
        let (reply, receive) = oneshot::channel();
        let job: Job = Box::new(move |store| {
            let result = command.apply(store);
            let fail = result.as_ref().err().is_some_and(StoreError::is_fenced);
            let _ = reply.send(result);
            if fail {
                Err(StoreError::failed("fenced"))
            } else {
                Ok(())
            }
        });
        let sender = self
            .inner
            .commands
            .lock()
            .expect("command mutex")
            .clone()
            .ok_or_else(|| StoreError::failed("the session is closed"))?;
        sender
            .send(job)
            .await
            .map_err(|_| StoreError::failed("the database thread is gone"))?;
        receive
            .await
            .map_err(|_| StoreError::failed("the database thread dropped the reply"))?
    }

    /// Stop accepting commands, drain the queue and release ownership.
    ///
    /// Only the creating handle may close the session; clones are clients and
    /// may not end another holder's session.
    pub(crate) async fn close(&self) {
        if !self.owner {
            return;
        }
        // Dropping the last sender lets the thread finish queued jobs and exit
        // its loop; joining proves the connection is gone and the OS-owned
        // session lock released.
        self.inner.commands.lock().expect("command mutex").take();
        let thread = self.inner.thread.lock().expect("thread mutex").take();
        if let Some(thread) = thread {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

/// Readable session identity, available as soon as the store is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionInfo {
    pub(crate) session_id: crate::SessionId,
    pub(crate) root: crate::ConversationId,
    pub(crate) last_commit: Option<crate::CommitSeq>,
}
