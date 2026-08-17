//! Stable identity newtypes for the live runtime.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AgentId(u64);

impl AgentId {
    pub const ROOT: Self = Self(1);

    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "agent-{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TurnId(u64);

impl TurnId {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TurnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "turn-{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct RuntimeCursor(u64);

impl RuntimeCursor {
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl fmt::Display for RuntimeCursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
