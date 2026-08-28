use crate::ids::EntryId;

pub(crate) const MAIN: &str = "main";

/// Model-facing execution selection for future work on one lane.
///
/// This is deliberately separate from semantic conversation history. More
/// fields belong here only when the runtime has a real owner for them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Config {
    pub(crate) model_ref: String,
}

impl Config {
    pub(crate) fn new(model_ref: impl Into<String>) -> Self {
        Self {
            model_ref: model_ref.into(),
        }
    }
}

/// Current durable position and configuration of a lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lane {
    pub(crate) name: String,
    pub(crate) leaf: Option<EntryId>,
    pub(crate) config: Config,
}
