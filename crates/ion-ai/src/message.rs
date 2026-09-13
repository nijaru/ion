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
    /// Opaque provider-specific continuation material.
    ///
    /// It is preserved with its origin so an adapter can decide whether reusing
    /// it is valid; it is not silently portable across providers. Dropping or
    /// reconstructing it is an explicit adapter decision, not a default.
    pub provider_replay: Option<ProviderReplay>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderReplay {
    /// Provider that produced this material. An adapter must not reuse it when
    /// the target provider differs.
    pub provider: String,
    /// Adapter-defined discriminator, for example `reasoning` or `thinking`.
    pub kind: String,
    pub data: Value,
}

impl ProviderReplay {
    #[must_use]
    pub fn new(provider: impl Into<String>, kind: impl Into<String>, data: Value) -> Self {
        Self {
            provider: provider.into(),
            kind: kind.into(),
            data,
        }
    }

    #[must_use]
    pub fn is_compatible_with(&self, provider: &str) -> bool {
        self.provider == provider
    }
}
