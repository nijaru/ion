//! OpenAI-compatible API client.
//!
//! Handles `OpenRouter`, Groq, Kimi, `OpenAI`, Ollama, `ChatGPT` with provider-specific quirks.

mod client;
mod quirks;
mod request;
mod request_builder;
mod response;
mod stream;
mod stream_handler;

#[cfg(test)]
mod tests;

pub use client::OpenAICompatClient;
