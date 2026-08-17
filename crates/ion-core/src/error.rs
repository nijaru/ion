//! Typed command and runtime failures.

use thiserror::Error;

use crate::ids::TurnId;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CommandError {
    #[error("runtime command queue is saturated")]
    QueueSaturated,
    #[error("runtime is closed")]
    Closed,
    #[error("runtime dropped the command before answering")]
    RuntimeDropped,
    #[error("a turn is already running")]
    Busy { turn_id: TurnId },
    #[error("turn {turn_id} is not the active turn")]
    TurnNotActive { turn_id: TurnId },
    #[error("no active turn to cancel")]
    NoActiveTurn,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error("turn failed: {0}")]
    TurnFailed(String),
    #[error("turn cancelled")]
    TurnCancelled,
    #[error("event subscription lagged")]
    SubscriptionLagged,
    #[error("event subscription closed")]
    SubscriptionClosed,
}
