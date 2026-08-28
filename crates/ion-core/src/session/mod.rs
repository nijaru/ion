//! Durable session ownership boundary.
//!
//! Conversation-tree and lane state will live here once they are introduced
//! together with their persistence transactions. The existing execution
//! reducer has moved to `operation`; this narrow migration seam keeps current
//! runtime callers building while they move to the correct module.

pub(crate) use crate::operation::{
    EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome, OperationState,
    SessionEntry, Transition,
};
