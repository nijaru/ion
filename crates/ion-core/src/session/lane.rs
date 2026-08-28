use std::fmt;

use crate::ids::OperationId;

use super::EntryId;

const MAIN_LANE: &str = "main";

/// Stable application-facing identity of one lane in a durable session.
///
/// Lane names are intentionally not UUIDs: callers may bind them to useful
/// external identities. Empty names are rejected at the boundary.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub(crate) struct LaneId(String);

impl LaneId {
    #[must_use]
    pub(crate) fn main() -> Self {
        Self(MAIN_LANE.to_owned())
    }

    #[must_use]
    pub(crate) fn parse(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.is_empty()).then_some(Self(value))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for LaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Current total state of one lane.
///
/// Conversation history is passive and shared; the lane owns only its cursor
/// and operation-local activity. Queue state will join this value when the
/// runtime migrates from the current operation-owned inbox representation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LaneState {
    pub(crate) leaf: Option<EntryId>,
    pub(crate) operation: Option<OperationId>,
}

impl LaneState {
    #[must_use]
    pub(crate) const fn idle(leaf: Option<EntryId>) -> Self {
        Self {
            leaf,
            operation: None,
        }
    }
}

/// Model-facing configuration selected for future work on one lane.
///
/// Configuration is deliberately outside the conversation tree: changing a
/// model or tool activation changes how future work executes, not what was
/// said. A model step snapshots the effective configuration before execution.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LaneConfig {
    pub(crate) model_ref: String,
    pub(crate) active_tools: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_lane_has_a_stable_name() {
        let lane = LaneId::main();
        assert_eq!(lane.as_str(), MAIN_LANE);
        assert_eq!(lane.to_string(), MAIN_LANE);
    }

    #[test]
    fn lane_names_reject_only_the_empty_identity() {
        assert!(LaneId::parse("").is_none());
        assert_eq!(LaneId::parse("worker:review").unwrap().as_str(), "worker:review");
    }

    #[test]
    fn independent_lanes_can_point_at_the_same_leaf() {
        let leaf = EntryId::generate();
        let a = LaneState::idle(Some(leaf));
        let b = LaneState::idle(Some(leaf));
        assert_eq!(a.leaf, b.leaf);
        assert!(a.operation.is_none());
        assert!(b.operation.is_none());
    }
}
