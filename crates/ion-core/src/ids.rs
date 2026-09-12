//! Durable identity newtypes.
//!
//! Durable semantic identities currently use UUIDv7 and stay independent from
//! storage ordering. The target architecture keeps this semantic separation;
//! P1/P2 may still change the physical representation before schema stability.
//! Live runtime events keep a separate in-memory cursor.

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
pub struct AgentId(Uuid);

impl AgentId {
    #[must_use]
    pub fn generate() -> Self {
        Self(Uuid::now_v7())
    }

    /// Deterministic durable identity of one session family's root agent.
    #[must_use]
    pub const fn root(session_id: SessionId) -> Self {
        Self(session_id.as_uuid())
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

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "agent-{}", self.0)
    }
}

/// Durable identity of one semantic conversation.
///
/// Conversation ancestry and agent supervision are separate relationships; a
/// conversation id therefore never doubles as an agent id or commit position.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct ConversationId(Uuid);

impl ConversationId {
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

impl fmt::Display for ConversationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "conversation-{}", self.0)
    }
}

/// Durable identity of one generic runtime task.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct TaskId(Uuid);

impl TaskId {
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

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task-{}", self.0)
    }
}

/// Durable identity of one admitted input, independent from a caller supplied
/// idempotency/request key.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct InputId(Uuid);

impl InputId {
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

impl fmt::Display for InputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "input-{}", self.0)
    }
}

/// Legacy operation identity retained while the operation/lane execution path
/// is replaced by generic durable tasks.
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
    /// Provision a semantic conversation-entry identity before persistence.
    /// Sequence numbers remain independent storage-order metadata.
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
    fn semantic_identity_is_uuid_v7_and_independent_from_ordering() {
        let entry = EntryId::generate();
        let conversation = ConversationId::generate();
        let task = TaskId::generate();
        let input = InputId::generate();

        assert_eq!(EntryId::parse(&entry.as_uuid().to_string()), Some(entry));
        assert_eq!(
            ConversationId::parse(&conversation.as_uuid().to_string()),
            Some(conversation)
        );
        assert_eq!(TaskId::parse(&task.as_uuid().to_string()), Some(task));
        assert_eq!(InputId::parse(&input.as_uuid().to_string()), Some(input));
        assert_eq!(entry.as_uuid().get_version_num(), 7);
        assert_eq!(conversation.as_uuid().get_version_num(), 7);
        assert_eq!(task.as_uuid().get_version_num(), 7);
        assert_eq!(input.as_uuid().get_version_num(), 7);
    }
}
