use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouteReason {
    UserRequest,
    ToolContinuation,
    Steering,
    Retry,
    Auxiliary,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRoute {
    pub logical: ModelRef,
    pub effective: ModelRef,
    pub reason: ModelRouteReason,
}

impl ModelRoute {
    #[must_use]
    pub fn direct(model: ModelRef, reason: ModelRouteReason) -> Self {
        Self {
            logical: model.clone(),
            effective: model,
            reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelExecution {
    pub route: ModelRoute,
    #[serde(default)]
    pub returned_model: Option<String>,
}
