//! The client handle and its observation stream.
//!
//! A handle is a client, not an owner: cloning it, dropping it or dropping a
//! wait never changes what the session is doing. Every command returns after the
//! transaction that produced it committed, and waiting is a separate,
//! cancellation-safe operation over a commit-addressed observation stream.

use tokio::sync::{broadcast, mpsc, oneshot};

use super::command::{
    AdmissionReceipt, CancelReceipt, ConfigureRequest, EntryQuery, ResolveRequest, SessionEvent,
    SubmitRequest,
};
use crate::session::Request;
use crate::store::sqlite::entry::MAX_ENTRY_PAGE;
use crate::turn::TurnOutcome;
use crate::view::{EntryPage, TurnView};
use crate::{CommitSeq, Conversation, ConversationId, Error, Input, InputId, Result, TurnId};

/// Why a watcher did not receive the next event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WatchError {
    /// The watcher fell behind. Events were dropped, so the caller must
    /// resnapshot through a read instead of assuming it still has coverage.
    #[error("the observation stream skipped {skipped} events; resnapshot required")]
    Lagged { skipped: u64 },
    /// The session closed.
    #[error("the session closed")]
    Closed,
}

/// A commit-addressed observation stream.
///
/// It is best-effort: terminal state is durable and readable whether or not an
/// observer was attached, and a lagging observer is told to resnapshot.
pub struct SessionWatch {
    events: broadcast::Receiver<SessionEvent>,
}

impl std::fmt::Debug for SessionWatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionWatch")
            .finish_non_exhaustive()
    }
}

impl SessionWatch {
    pub async fn recv(&mut self) -> std::result::Result<SessionEvent, WatchError> {
        match self.events.recv().await {
            Ok(event) => Ok(event),
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                Err(WatchError::Lagged { skipped })
            }
            Err(broadcast::error::RecvError::Closed) => Err(WatchError::Closed),
        }
    }
}

/// A cloneable client for one session.
#[derive(Clone)]
pub struct SessionHandle {
    requests: mpsc::Sender<Request>,
    events: broadcast::Sender<SessionEvent>,
    root: ConversationId,
}

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionHandle")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl SessionHandle {
    pub(crate) fn new(
        requests: mpsc::Sender<Request>,
        events: broadcast::Sender<SessionEvent>,
        root: ConversationId,
    ) -> Self {
        Self {
            requests,
            events,
            root,
        }
    }

    /// The session's primary conversation.
    #[must_use]
    pub const fn root(&self) -> ConversationId {
        self.root
    }

    /// Admit an input, returning its durable receipt.
    pub async fn submit(&self, request: SubmitRequest) -> Result<AdmissionReceipt> {
        let (reply, receive) = oneshot::channel();
        self.send(Request::Submit { request, reply }, receive).await
    }

    /// Mark cancellation for one turn.
    pub async fn cancel(&self, turn: TurnId) -> Result<CancelReceipt> {
        let (reply, receive) = oneshot::channel();
        self.send(Request::Cancel { turn, reply }, receive).await
    }

    /// Drive a conversation's unfinished turn, or start its queued input.
    ///
    /// Resuming an already-driven turn is idempotent; it never creates a second
    /// local invocation of the same step.
    pub async fn resume(&self, conversation: ConversationId) -> Result<Option<TurnId>> {
        let (reply, receive) = oneshot::channel();
        self.send(
            Request::Resume {
                conversation,
                reply,
            },
            receive,
        )
        .await
    }

    /// Decide what to do about an invocation whose outcome is unknown.
    pub async fn resolve(&self, request: ResolveRequest) -> Result<()> {
        let (reply, receive) = oneshot::channel();
        self.send(Request::Resolve { request, reply }, receive)
            .await
    }

    /// Replace a conversation's configuration, guarded by its revision.
    pub async fn configure(&self, request: ConfigureRequest) -> Result<CommitSeq> {
        let (reply, receive) = oneshot::channel();
        self.send(Request::Configure { request, reply }, receive)
            .await
    }

    /// Inspect one turn.
    pub async fn turn(&self, turn: TurnId) -> Result<Option<TurnView>> {
        let (reply, receive) = oneshot::channel();
        self.send(Request::Turn { turn, reply }, receive).await
    }

    /// Inspect one conversation.
    pub async fn conversation(&self, conversation: ConversationId) -> Result<Option<Conversation>> {
        let (reply, receive) = oneshot::channel();
        self.send(
            Request::Conversation {
                conversation,
                reply,
            },
            receive,
        )
        .await
    }

    /// Read a bounded transcript page. The limit is clamped, never overflowed.
    pub async fn entries(&self, query: EntryQuery) -> Result<EntryPage> {
        let query = EntryQuery {
            limit: query.limit.min(MAX_ENTRY_PAGE),
            ..query
        };
        let (reply, receive) = oneshot::channel();
        self.send(Request::Entries { query, reply }, receive).await
    }

    /// Inspect one accepted input.
    pub async fn input(&self, input: InputId) -> Result<Option<Input>> {
        let (reply, receive) = oneshot::channel();
        self.send(Request::ReadInput { input, reply }, receive)
            .await
    }

    /// Give up an input that has not been placed in the transcript.
    ///
    /// An input that was already placed keeps its entry: it is part of the
    /// history the model was shown, and withdrawing it would rewrite what
    /// happened.
    pub async fn withdraw(&self, input: InputId) -> Result<()> {
        let (reply, receive) = oneshot::channel();
        self.send(Request::Withdraw { input, reply }, receive).await
    }

    /// A stream of commit-addressed observations.
    #[must_use]
    pub fn watch(&self) -> SessionWatch {
        SessionWatch {
            events: self.events.subscribe(),
        }
    }

    /// Wait for a turn's terminal outcome.
    ///
    /// This is cancellation-safe: dropping the future does not cancel the turn,
    /// and a missed observation is repaired by re-reading durable state.
    pub async fn wait(&self, turn: TurnId) -> Result<TurnOutcome> {
        let mut watch = self.watch();
        loop {
            match self.turn(turn).await? {
                Some(view) => {
                    if let Some(outcome) = view.turn.outcome {
                        return Ok(outcome);
                    }
                }
                None => {
                    return Err(Error::Invalid(format!("turn {turn} does not exist")));
                }
            }
            match watch.recv().await {
                Ok(SessionEvent::TurnTerminal {
                    turn: settled,
                    outcome,
                    ..
                }) if settled == turn => return Ok(outcome),
                Ok(_) => {}
                // A skipped batch means the event we wanted may be gone: read
                // the durable outcome again rather than assume it never came.
                Err(WatchError::Lagged { .. }) => {}
                Err(WatchError::Closed) => return Err(Error::Closed),
            }
        }
    }

    /// Keeps the request/reply plumbing in one place.
    async fn send<T>(&self, request: Request, receive: oneshot::Receiver<Result<T>>) -> Result<T> {
        self.requests
            .send(request)
            .await
            .map_err(|_| Error::Closed)?;
        receive.await.map_err(|_| Error::Closed)?
    }
}
