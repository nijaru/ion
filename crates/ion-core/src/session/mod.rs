//! Durable session topology.
//!
//! A session owns passive append-only conversation history plus active lanes
//! that point into that history. The current operation reducer still lives in
//! `operation`; its temporary re-exports below keep the existing runtime build
//! green while callers migrate to the new domain boundary.

mod lane;
mod tree;

pub(crate) use lane::{LaneConfig, LaneId, LaneState};
pub(crate) use tree::{Entry, EntryId};

// Migration seam: these types describe operation execution, not session
// topology. New code should import them from `crate::operation`.
pub(crate) use crate::operation::{
    Applied, EffectIntent, InboxItem, InboxKind, OperationMachine, OperationOutcome,
    OperationState, SessionEntry, Transition, TransitionError,
};
