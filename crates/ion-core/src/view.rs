//! Readable views over durable state.
//!
//! A view reports what is stored now; it is not a subscription and not a cache.
//! A watcher that falls behind receives an explicit lag signal and must
//! resnapshot through one of these calls rather than assume it is current.

use crate::attempt::{ModelAttempt, ModelStep};
use crate::entry::Entry;
use crate::invocation::ToolInvocation;
use crate::turn::Turn;

/// A bounded page of one conversation's transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryPage {
    pub entries: Vec<Entry>,
    /// Whether more entries follow the last one in this page.
    pub has_more: bool,
}

/// One turn and everything it currently references.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnView {
    pub turn: Turn,
    /// The frozen basis of the step the turn is currently in, if any.
    pub step: Option<ModelStep>,
    pub attempts: Vec<ModelAttempt>,
    pub invocations: Vec<ToolInvocation>,
}
