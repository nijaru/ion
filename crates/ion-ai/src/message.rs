use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Content, LoadedImage};

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

impl Message {
    /// Build the user message shared by terminal, headless and RPC clients.
    #[must_use]
    pub fn user_input(prompt: String, images: impl IntoIterator<Item = LoadedImage>) -> Self {
        Self {
            role: Role::User,
            content: std::iter::once(Content::Text(prompt))
                .chain(images.into_iter().flat_map(LoadedImage::into_parts))
                .collect(),
            provider_replay: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderReplay {
    /// Provider that produced this material. An adapter must not reuse it when
    /// the target provider differs.
    pub provider: String,
    /// Adapter-defined discriminator, for example `reasoning` or `thinking`.
    pub kind: String,
    pub data: Value,
    /// A tool continuation must keep the producing request prefix stable
    /// while this assistant response and its tool results are in flight.
    #[serde(default)]
    pub prefix_bound: bool,
}

impl ProviderReplay {
    #[must_use]
    pub fn new(provider: impl Into<String>, kind: impl Into<String>, data: Value) -> Self {
        Self {
            provider: provider.into(),
            kind: kind.into(),
            data,
            prefix_bound: false,
        }
    }

    #[must_use]
    pub fn with_prefix_binding(mut self, enabled: bool) -> Self {
        self.prefix_bound = enabled;
        self
    }

    #[must_use]
    pub fn is_compatible_with(&self, provider: &str) -> bool {
        self.provider == provider
    }
}
