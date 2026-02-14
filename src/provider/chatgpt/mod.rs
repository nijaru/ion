//! ChatGPT subscription client using Responses API.
//!
//! Uses the ChatGPT Codex backend (`chatgpt.com/backend-api/codex`) with OAuth
//! authentication. This approach is borrowed from the official Codex CLI.
//!
//! **Warning:** Unofficial / unsupported API surface.

mod client;
mod convert;
mod types;

pub use client::ChatGptResponsesClient;
