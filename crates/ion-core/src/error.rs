//! Typed command and runtime failures (DESIGN.md §26.1).

use thiserror::Error;

use crate::ids::{EntryId, OperationId};

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CommandError {
    #[error("session command queue is saturated")]
    QueueSaturated,
    #[error("session is closed")]
    Closed,
    #[error("runtime dropped the command before answering")]
    RuntimeDropped,
    #[error("an operation is already running")]
    Busy { operation_id: OperationId },
    #[error("the lane already has a pending next run ({entry_id})")]
    NextRunQueued { entry_id: EntryId },
    #[error("lane {0:?} does not exist")]
    LaneNotFound(String),
    #[error("lane {0:?} already exists")]
    LaneExists(String),
    #[error("lane name cannot be empty")]
    InvalidLaneName,
    #[error("no active operation; the session is idle")]
    NoActiveOperation,
    #[error("operation {operation_id} is not the active operation")]
    NotActive { operation_id: OperationId },
    #[error("operation {operation_id} is not waiting for an approval decision")]
    NoPendingApproval { operation_id: OperationId },
    #[error("model {0:?} is not available from this provider")]
    UnsupportedModel(String),
    #[error("unsupported thinking level: {0} (off/minimal/low/medium/high/xhigh/max)")]
    UnsupportedThinking(String),
    #[error("a shell passthrough is already running")]
    ShellPassthroughBusy,
    #[error("durable write failed: {0}")]
    Persistence(String),
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("operation failed: {0}")]
    OperationFailed(String),
    #[error("operation cancelled")]
    OperationCancelled,
    #[error(
        "approval required: `{tool}` is not allowed in non-interactive mode; \
         grant it explicitly (e.g. --allow {tool})"
    )]
    ApprovalRequired { tool: String },
    #[error("event subscription lagged")]
    SubscriptionLagged,
    #[error("event subscription closed")]
    SubscriptionClosed,
}
