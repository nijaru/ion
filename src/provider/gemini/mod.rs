//! Gemini OAuth client using Code Assist API.
//!
//! Uses `cloudcode-pa.googleapis.com` with OAuth authentication borrowed from
//! the official Gemini CLI.
//!
//! **Warning:** Unofficial / unsupported — see subscription module docs.

mod client;
mod convert;
mod types;

pub use client::GeminiOAuthClient;
