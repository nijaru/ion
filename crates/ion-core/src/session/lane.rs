use std::collections::BTreeSet;

use crate::ids::{EntryId, OperationId};
use crate::tool::ToolSelection;

pub(crate) const MAIN: &str = "main";

/// Durable dynamic capability scopes structurally admitted to one lane.
/// Core tools are inherent and therefore never appear here. `LegacyAll` is
/// only a decode bridge for pre-Step-7 lane rows and is materialized before
/// resumed work can recover or accept a command.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ScopeGrant {
    LegacyAll,
    Only(BTreeSet<String>),
}

impl ScopeGrant {
    fn legacy_all() -> Self {
        Self::LegacyAll
    }

    #[must_use]
    pub(crate) fn none() -> Self {
        Self::Only(BTreeSet::new())
    }

    #[must_use]
    pub(crate) fn from_published(scopes: BTreeSet<String>) -> Self {
        Self::Only(scopes)
    }

    pub(crate) fn materialize(&mut self, published: &BTreeSet<String>) -> bool {
        if matches!(self, Self::LegacyAll) {
            *self = Self::Only(published.clone());
            true
        } else {
            false
        }
    }

    pub(crate) fn insert(&mut self, scope: String) -> bool {
        match self {
            Self::LegacyAll => false,
            Self::Only(scopes) => scopes.insert(scope),
        }
    }

    #[must_use]
    pub(crate) fn allows(&self, scope: &str) -> bool {
        match self {
            Self::LegacyAll => true,
            Self::Only(scopes) => scopes.contains(scope),
        }
    }

    #[must_use]
    pub(crate) fn is_subset_of(&self, parent: &Self) -> bool {
        match (self, parent) {
            (_, Self::LegacyAll) => true,
            (Self::LegacyAll, Self::Only(_)) => false,
            (Self::Only(child), Self::Only(parent)) => child.is_subset(parent),
        }
    }
}

/// Model-facing execution selection for future work on one lane.
///
/// This is deliberately separate from semantic conversation history. More
/// fields belong here only when the runtime has a real owner for them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Config {
    pub(crate) model_ref: String,
    /// Reasoning-effort selection for future model steps on this lane
    /// (pi-parity thinking levels: off/minimal/low/medium/high/xhigh/max).
    /// `None` keeps the provider adapter's own default. Like `model_ref`
    /// this is model-facing configuration, frozen into each step's
    /// ModelConfig, never semantic history (§14.8 pattern).
    #[serde(default)]
    pub(crate) thinking: Option<String>,
    pub(crate) tools: ToolSelection,
    #[serde(default = "ScopeGrant::legacy_all")]
    pub(crate) scopes: ScopeGrant,
}

impl Config {
    pub(crate) fn new(model_ref: impl Into<String>) -> Self {
        Self {
            model_ref: model_ref.into(),
            thinking: None,
            tools: ToolSelection::all(),
            scopes: ScopeGrant::none(),
        }
    }
}

/// Semantic input reserved for the next run while a lane is busy.
///
/// The entry identity is provisioned when queueing is acknowledged. There is
/// deliberately no operation identity until the lane actually accepts the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NextRun {
    pub(crate) entry_id: EntryId,
    pub(crate) prompt: String,
}

impl NextRun {
    pub(crate) fn reserve(prompt: String) -> Self {
        Self {
            entry_id: EntryId::generate(),
            prompt,
        }
    }
}

/// Directly readable current state of one durable lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct State {
    pub(crate) leaf: Option<EntryId>,
    pub(crate) current_operation: Option<OperationId>,
    pub(crate) pending_next_run: Option<NextRun>,
}

/// Current durable state and configuration of a lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lane {
    pub(crate) name: String,
    pub(crate) state: State,
    pub(crate) config: Config,
}
