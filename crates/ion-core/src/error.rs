//! Typed command and runtime failures (DESIGN.md §26.1).

use thiserror::Error;

use crate::ids::{EntryId, OperationId};
use crate::input::RequestKey;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CommandError {
    #[error("host configuration is busy; finish active work before reloading")]
    ConfigurationBusy,
    #[error("host configuration update failed; reconcile configuration before starting work")]
    ConfigurationFailed,
    #[error("session command queue is saturated")]
    QueueSaturated,
    #[error("session is closed")]
    Closed,
    #[error("runtime dropped the command before answering")]
    RuntimeDropped,
    #[error("an operation is already running")]
    Busy { operation_id: OperationId },
    #[error("request key {request_key} is already bound to different input")]
    IdempotencyConflict { request_key: RequestKey },
    #[error("the lane already has a pending next run ({entry_id})")]
    NextRunQueued { entry_id: EntryId },
    #[error("entry {0} does not exist in this session")]
    EntryNotFound(EntryId),
    #[error("entry {0} ends within a tool exchange; select a point after all tool results")]
    IncompleteToolExchange(EntryId),
    #[error("lane {0:?} does not exist")]
    LaneNotFound(String),
    #[error("lane {0:?} already exists")]
    LaneExists(String),
    #[error("lane name cannot be empty")]
    InvalidLaneName,
    #[error("scope {0:?} is not configured for this host")]
    ScopeNotConfigured(String),
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
    #[error("checkpoint must be an immutable Git object ID")]
    InvalidCheckpoint,
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
