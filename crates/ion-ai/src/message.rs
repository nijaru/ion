use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Content;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
    /// Opaque same-provider replay/continuation data. Other providers may ignore it.
    pub provider_replay: Option<Value>,
}
