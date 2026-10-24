use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl std::str::FromStr for ModelRef {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (provider, model) = value.split_once('/').ok_or("expected PROVIDER/MODEL")?;
        if provider.is_empty() || model.is_empty() {
            return Err("expected nonempty PROVIDER/MODEL");
        }
        Ok(Self {
            provider: provider.to_owned(),
            model: model.to_owned(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRouteReason {
    UserRequest,
    Recovery,
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
