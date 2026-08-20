//! Typed command and runtime failures (DESIGN.md §26.1).

use thiserror::Error;

use crate::ids::OperationId;

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
    #[error("no active operation; the session is idle")]
    NoActiveOperation,
    #[error("operation {operation_id} is not the active operation")]
    NotActive { operation_id: OperationId },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("operation failed: {0}")]
    OperationFailed(String),
    #[error("operation cancelled")]
    OperationCancelled,
    #[error("event subscription lagged")]
    SubscriptionLagged,
    #[error("event subscription closed")]
    SubscriptionClosed,
}
