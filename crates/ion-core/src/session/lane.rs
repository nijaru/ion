use crate::ids::{EntryId, OperationId};
use crate::tool::ToolSelection;

pub(crate) const MAIN: &str = "main";

/// Model-facing execution selection for future work on one lane.
///
/// This is deliberately separate from semantic conversation history. More
/// fields belong here only when the runtime has a real owner for them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Config {
    pub(crate) model_ref: String,
    pub(crate) tools: ToolSelection,
}

impl Config {
    pub(crate) fn new(model_ref: impl Into<String>) -> Self {
        Self {
            model_ref: model_ref.into(),
            tools: ToolSelection::all(),
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
