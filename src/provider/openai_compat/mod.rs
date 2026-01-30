//! OpenAI-compatible API client.
//!
//! Handles OpenRouter, Groq, Kimi, OpenAI, Ollama, ChatGPT with provider-specific quirks.

mod client;
mod quirks;
mod request;
mod response;
mod stream;

pub use client::OpenAICompatClient;
pub use quirks::ProviderQuirks;
