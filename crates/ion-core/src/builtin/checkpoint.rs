//! Reading the durable checkpoint a previous invocation left behind.
//!
//! A checkpoint is evidence about a task's own execution, and recovery has three
//! cases rather than two. Absence means no invocation has recorded anything yet,
//! so a first attempt may derive its work from current state. A readable
//! checkpoint is the recorded evidence, and the invocation continues from it. A
//! checkpoint that exists but cannot be interpreted is neither: treating it as
//! absent is what lets a replacement invocation silently rebuild a frozen
//! request from changed history or repeat an action whose external outcome is
//! unknown.
//!
//! Decoding is therefore explicit about which of the three it found, and every
//! caller fails closed on `Unreadable`.

use serde::de::DeserializeOwned;
use serde_json::Value;

/// What a task's durable checkpoint says about the invocation that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Checkpoint<T> {
    /// No checkpoint: nothing has been recorded for this task yet.
    Absent,
    /// The previous invocation left evidence this kind can continue from.
    Valid(T),
    /// A checkpoint exists but is not evidence this kind can act on.
    Unreadable,
}

/// Decode a checkpoint as this kind's evidence.
///
/// A malformed payload is `Unreadable`, never `Absent`: the distinction is the
/// whole point, because the callers treat those two cases differently.
pub(super) fn decode<T: DeserializeOwned>(checkpoint: Option<&Value>) -> Checkpoint<T> {
    match checkpoint {
        None => Checkpoint::Absent,
        Some(value) => match serde_json::from_value(value.clone()) {
            Ok(decoded) => Checkpoint::Valid(decoded),
            Err(_) => Checkpoint::Unreadable,
        },
    }
}
