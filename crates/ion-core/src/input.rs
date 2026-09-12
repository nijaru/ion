//! Durable input admission identity and receipts.
//!
//! A request key belongs to the caller, not to a transport retry. Repeating
//! the same key with the same semantic binding returns the original receipt;
//! rebinding it to another target, mode, or content is a conflict.

use std::fmt;

use uuid::Uuid;

use crate::ids::{InboxId, OperationId};

/// Caller-stable identity for one durable input admission.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct RequestKey(Uuid);

impl RequestKey {
    /// Generate a fresh key for callers that do not need to retry an unknown
    /// admission result. Retry-capable callers retain and reuse the returned
    /// key instead.
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Uuid::parse_str(text).ok().map(Self)
    }
}

impl fmt::Display for RequestKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "request-{}", self.0)
    }
}

/// Durable acknowledgment that one input was accepted.
///
/// Completion is a separate fact. `accepted_seq` is ordering metadata for the
/// session and is deliberately not the identity of the input, inbox item, or
/// operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InputReceipt {
    pub request_key: RequestKey,
    pub operation_id: OperationId,
    pub inbox_id: InboxId,
    pub accepted_seq: u64,
}

/// Input delivery modes are durable conflict-binding data. Only `Submit` is
/// promoted through the production P1 API so far; later modes must reuse the
/// same admission contract rather than inventing separate dedupe semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputMode {
    Submit,
}

impl InputMode {
    #[must_use]
    pub(crate) const fn as_storage(self) -> &'static str {
        match self {
            Self::Submit => "submit",
        }
    }
}

/// Exact semantic binding of a caller request key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InputAdmission {
    pub(crate) request_key: RequestKey,
    pub(crate) target: String,
    pub(crate) mode: InputMode,
    pub(crate) content: String,
}

impl InputAdmission {
    #[must_use]
    pub(crate) fn submit(
        request_key: RequestKey,
        target: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            request_key,
            target: target.into(),
            mode: InputMode::Submit,
            content: content.into(),
        }
    }
}
