//! Identity types.
//!
//! A session id is globally unique. Every other identity is a session-local
//! number drawn from one private monotonic sequence, so two records of
//! different kinds can never share a durable number, and a caller that holds a
//! number cannot reach the allocator.

use std::fmt;
use std::num::NonZeroI64;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for SessionId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "i64", into = "i64")]
pub(crate) struct LocalSeq(NonZeroI64);

impl LocalSeq {
    pub(crate) fn new(value: i64) -> Result<Self, IdError> {
        NonZeroI64::new(value)
            .filter(|value| value.get() > 0)
            .map(Self)
            .ok_or(IdError::NonPositive(value))
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0.get()
    }

    #[cfg(test)]
    pub(crate) fn next(self) -> Result<Self, IdError> {
        let next = self.get().checked_add(1).ok_or(IdError::Exhausted)?;
        Self::new(next)
    }
}

impl TryFrom<i64> for LocalSeq {
    type Error = IdError;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<LocalSeq> for i64 {
    fn from(value: LocalSeq) -> Self {
        value.get()
    }
}

impl fmt::Display for LocalSeq {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IdError {
    #[error("session-local sequence values must be positive, got {0}")]
    NonPositive(i64),
    #[error("session-local sequence space is exhausted")]
    Exhausted,
}

macro_rules! local_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        pub struct $name(LocalSeq);

        impl $name {
            pub fn new(value: i64) -> Result<Self, IdError> {
                LocalSeq::new(value).map(Self)
            }

            /// The numeric transport value of this session-local identifier.
            ///
            /// The backing sequence namespace stays crate-private: callers may
            /// move the number, not the allocator.
            #[must_use]
            pub const fn get(self) -> i64 {
                self.0.get()
            }
        }

        impl TryFrom<i64> for $name {
            type Error = IdError;

            fn try_from(value: i64) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for LocalSeq {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl From<LocalSeq> for $name {
            fn from(value: LocalSeq) -> Self {
                Self(value)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.get().fmt(formatter)
            }
        }
    };
}

local_id!(ConversationId);
local_id!(EntryId);
local_id!(InputId);
local_id!(TurnId);
local_id!(StepId);
local_id!(AttemptId);
local_id!(InvocationId);
local_id!(CommitSeq);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ids_share_ordered_storage_without_sharing_rust_types() {
        let sequence = LocalSeq::new(41).expect("valid local sequence");
        let entry = EntryId::new(sequence.get()).expect("entry id");
        let turn = TurnId::new(sequence.next().expect("next sequence").get()).expect("turn id");

        assert_eq!(entry.get(), 41);
        assert_eq!(turn.get(), 42);
        assert!(EntryId::new(0).is_err());
        assert!(TurnId::new(-1).is_err());
    }

    #[test]
    fn serde_rejects_non_positive_local_sequence() {
        assert!(serde_json::from_str::<LocalSeq>("0").is_err());
        assert!(serde_json::from_str::<LocalSeq>("-1").is_err());
        assert_eq!(
            serde_json::from_str::<LocalSeq>("7")
                .expect("positive local sequence")
                .get(),
            7
        );
    }

    #[test]
    fn session_id_round_trips_through_text() {
        let id = SessionId::new();
        let encoded = id.to_string();
        assert_eq!(encoded.parse::<SessionId>().expect("parse session id"), id);
    }
}
