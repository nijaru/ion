//! Shared HTTP utilities for LLM providers.

mod client;
mod sse;

pub use client::{AuthConfig, HttpClient};
pub use sse::SseParser;
