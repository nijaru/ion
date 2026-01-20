use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Provider error: {0}")]
    Provider(#[from] crate::provider::ProviderError),

    #[error("Tool error: {0}")]
    Tool(#[from] crate::tool::ToolError),

    #[error("Memory error: {0}")]
    Memory(#[from] crate::memory::MemoryError),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Session error: {0}")]
    Session(String),

    #[error("Agent error: {0}")]
    Agent(String),

    #[error("Mcp error: {0}")]
    Mcp(#[from] crate::mcp::McpError),
}

pub type Result<T> = std::result::Result<T, Error>;
