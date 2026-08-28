//! Durable session ownership boundary.
//!
//! Session topology is passive semantic history plus active lane state. The
//! execution reducer lives in `operation`; the re-exports below are a narrow
//! migration seam for current runtime callers.

pub(crate) mod lane;
pub(crate) mod tree;

pub(crate) use crate::operation::{
    EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome, OperationState,
    SessionEntry, Transition,
};
