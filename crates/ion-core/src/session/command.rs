//! Public requests, receipts and the client-to-supervisor command surface.
//!
//! A receipt is returned only after the transaction that produced it committed,
//! and it names durable identities rather than a position in a queue. Waiting
//! is a separate operation, so a client that disconnects after admission does
//! not revoke the work it asked for.

use tokio::sync::oneshot;

use crate::config::ConversationConfig;
use crate::input::{InputMode, InputSender, RequestKey};
use crate::invocation::Resolution;
use crate::turn::{TurnOutcome, TurnPhase};
use crate::view::TurnView;
use crate::{
    CommitSeq, Conversation, ConversationId, EntryId, Error, InputId, InvocationId, TurnId,
};

/// One accepted-input request.
#[derive(Debug, Clone)]
pub struct SubmitRequest {
    /// The target conversation. `None` means the session's primary conversation.
    pub conversation: Option<ConversationId>,
    pub sender: InputSender,
    pub mode: InputMode,
    pub request_key: Option<RequestKey>,
    pub text: String,
}

impl SubmitRequest {
    /// A user submission to the session's primary conversation.
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            conversation: None,
            sender: InputSender::User,
            mode: InputMode::Submit,
            request_key: None,
            text: text.into(),
        }
    }

    /// A user request that waits for the running turn instead of rejecting.
    #[must_use]
    pub fn follow_up(text: impl Into<String>) -> Self {
        Self {
            mode: InputMode::FollowUp,
            ..Self::user(text)
        }
    }
}

/// The durable identity of an accepted input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionReceipt {
    pub input: InputId,
    /// The turn answering this input now, if admission started one.
    pub turn: Option<TurnId>,
    /// Whether this was an exact replay of an already accepted request.
    pub replay: bool,
    /// The commit that made the admission durable.
    pub commit: Option<CommitSeq>,
}

impl AdmissionReceipt {
    #[must_use]
    pub fn started(input: InputId, turn: TurnId, commit: CommitSeq) -> Self {
        Self {
            input,
            turn: Some(turn),
            replay: false,
            commit: Some(commit),
        }
    }

    #[must_use]
    pub fn queued(input: InputId, commit: CommitSeq) -> Self {
        Self {
            input,
            turn: None,
            replay: false,
            commit: Some(commit),
        }
    }

    #[must_use]
    pub const fn replay(input: InputId, turn: Option<TurnId>) -> Self {
        Self {
            input,
            turn,
            replay: true,
            commit: None,
        }
    }
}

/// The result of asking to cancel a turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelReceipt {
    pub turn: TurnId,
    /// The generation this cancellation marked, when the turn was not already
    /// terminal.
    pub generation: Option<u64>,
    /// Present when the turn had already settled. Its outcome is returned
    /// unchanged rather than rewritten.
    pub outcome: Option<TurnOutcome>,
}

/// An explicit decision about an invocation whose outcome is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolveRequest {
    pub turn: TurnId,
    pub invocation: InvocationId,
    pub resolution: Resolution,
}

/// A complete configuration replacement, guarded by the revision it replaces.
#[derive(Debug, Clone)]
pub struct ConfigureRequest {
    pub conversation: ConversationId,
    pub expected: Option<CommitSeq>,
    pub config: ConversationConfig,
}

/// A bounded transcript page request.
#[derive(Debug, Clone, Copy)]
pub struct EntryQuery {
    pub conversation: ConversationId,
    pub after: Option<EntryId>,
    pub limit: u32,
}

/// What a client observes.
///
/// A phase or terminal event carries the commit cursor that was current when it
/// was published, so a watcher that missed events can tell and resnapshot
/// instead of assuming it is up to date.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    TurnPhase {
        turn: TurnId,
        phase: TurnPhase,
        commit: Option<CommitSeq>,
    },
    TurnTerminal {
        turn: TurnId,
        outcome: TurnOutcome,
        commit: Option<CommitSeq>,
    },
    Fenced {
        message: String,
    },
    Closed,
}

/// Requests the client delivers to the single session owner.
pub(crate) enum Request {
    Submit {
        request: SubmitRequest,
        reply: oneshot::Sender<Result<AdmissionReceipt, Error>>,
    },
    Cancel {
        turn: TurnId,
        reply: oneshot::Sender<Result<CancelReceipt, Error>>,
    },
    Resume {
        conversation: ConversationId,
        reply: oneshot::Sender<Result<Option<TurnId>, Error>>,
    },
    Resolve {
        request: ResolveRequest,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    Configure {
        request: ConfigureRequest,
        reply: oneshot::Sender<Result<CommitSeq, Error>>,
    },
    Turn {
        turn: TurnId,
        reply: oneshot::Sender<Result<Option<TurnView>, Error>>,
    },
    Conversation {
        conversation: ConversationId,
        reply: oneshot::Sender<Result<Option<Conversation>, Error>>,
    },
    Entries {
        query: EntryQuery,
        reply: oneshot::Sender<Result<crate::view::EntryPage, Error>>,
    },
    ReadInput {
        input: InputId,
        reply: oneshot::Sender<Result<Option<crate::Input>, Error>>,
    },
    Withdraw {
        input: InputId,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    Close {
        reply: oneshot::Sender<()>,
    },
}
