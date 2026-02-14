//! OpenAI-compatible API client.
//!
//! Handles `OpenRouter`, Groq, Kimi, `OpenAI`, Local, `ChatGPT` with provider-specific quirks.

mod client;
mod convert;
mod quirks;
mod request;
mod response;
mod stream;

#[cfg(test)]
mod tests;

pub use client::OpenAICompatClient;
