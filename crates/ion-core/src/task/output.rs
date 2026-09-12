use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ArtifactId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskOutput {
    pub value: Value,
    pub artifact: Option<ArtifactId>,
}
