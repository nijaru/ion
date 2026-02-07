use crate::agent::designer::Plan;
use crate::provider::ModelInfo;

pub enum AgentEvent {
    TextDelta(String),
    ThinkingDelta(String),
    /// Tool call started: (id, name, arguments)
    ToolCallStart(String, String, serde_json::Value),
    ToolCallResult(String, String, bool),
    PlanGenerated(Plan),
    CompactionStatus {
        before: usize,
        after: usize,
    },
    TokenUsage {
        used: usize,
        max: usize,
    },
    InputTokens(usize),
    OutputTokensDelta(usize),
    /// Provider-reported token usage (more accurate than local estimates).
    ProviderUsage {
        input_tokens: usize,
        output_tokens: usize,
        cache_read_tokens: usize,
        cache_write_tokens: usize,
    },
    /// Retry in progress: (reason, `delay_seconds`)
    Retry(String, u64),
    Finished(String),
    Error(String),
    ModelsFetched(Vec<ModelInfo>),
    ModelFetchError(String),
}
