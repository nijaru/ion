//! Subscription-based OAuth providers.
//!
//! # Warning: Unofficial / Unsupported
//!
//! These providers use OAuth flows borrowed from official CLI tools to access
//! consumer subscriptions (ChatGPT Plus/Pro, Gemini). This approach is:
//!
//! - **Not officially supported** by OpenAI or Google for third-party tools
//! - **Subject to breakage** if the providers change their OAuth endpoints
//! - **Potentially against ToS** - use at your own risk
//!
//! The credential borrowing pattern is used by other CLI tools (Codex CLI,
//! Gemini CLI, Antigravity) but is not an official API.
//!
//! # References
//!
//! - Codex CLI: <https://github.com/openai/codex>
//! - Gemini CLI: <https://github.com/google-gemini/gemini-cli>
//! - Antigravity: <https://github.com/AiHubLabs/Antigravity>

mod chatgpt;
mod gemini;

pub use chatgpt::ChatGptResponsesClient;
pub use gemini::GeminiOAuthClient;
