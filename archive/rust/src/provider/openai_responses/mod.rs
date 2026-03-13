//! OpenAI Responses API client.
//!
//! Uses the standard `api.openai.com/v1/responses` endpoint with support for
//! reasoning summaries, usage tracking, and incremental tool call streaming.

mod client;
mod convert;
mod types;

pub use client::OpenAIResponsesClient;
