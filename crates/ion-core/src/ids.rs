//! Durable identity newtypes (DESIGN.md §11.2).
//!
//! Sessions, operations, and effects use UUIDv7. Conversation entry identity
//! is a deterministic UUIDv5 derived from the owning session and its immutable
//! per-session sequence: the sequence remains the durable ordering key while
//! callers use an opaque typed id. Live runtime events keep a separate
//! in-memory cursor.

use std::fmt;

use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
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

    /// Parse the bare UUID form stored in SQLite (no display prefix).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Uuid::parse_str(text).ok().map(Self)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "session-{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct OperationId(Uuid);

impl OperationId {
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
}

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "op-{}", self.0)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct EntryId(Uuid);

impl EntryId {
    /// Stable identity for a semantic entry whose per-session sequence has
    /// already been provisioned by the single session writer.
    #[must_use]
    pub fn for_seq(session_id: SessionId, seq: u64) -> Self {
        let namespace = session_id.as_uuid();
        Self(Uuid::new_v5(&namespace, &seq.to_be_bytes()))
    }

    #[must_use]
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }

    /// Parse the bare UUID form stored in SQLite (no display prefix).
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Uuid::parse_str(text).ok().map(Self)
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "entry-{}", self.0)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct EffectId(Uuid);

impl EffectId {
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
}

impl fmt::Display for EffectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "effect-{}", self.0)
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct InboxId(Uuid);

impl InboxId {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RuntimeCursor(u64);

impl RuntimeCursor {
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for RuntimeCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cursor-{}", self.0)
    }
}

/// Process-local identity of one loaded session runtime incarnation.
///
/// A reopened session starts its live cursor at zero again, so frontends use
/// this identity to reject stale events from an earlier runtime instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuntimeInstanceId(Uuid);

impl RuntimeInstanceId {
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    #[must_use]
    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl fmt::Display for RuntimeInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "runtime-{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_identity_is_stable_but_not_the_ordering_key() {
        let session = SessionId::from_uuid(Uuid::nil());
        let one = EntryId::for_seq(session, 1);
        assert_eq!(one, EntryId::for_seq(session, 1));
        assert_ne!(one, EntryId::for_seq(session, 2));
        assert_eq!(EntryId::parse(&one.as_uuid().to_string()), Some(one));
    }
}
