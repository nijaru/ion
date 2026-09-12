use serde::{Deserialize, Serialize};

use crate::{Message, ModelRef, ToolSpec};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: ModelRef,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
}
