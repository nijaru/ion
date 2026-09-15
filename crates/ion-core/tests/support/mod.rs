//! Shared fixtures for the turn-engine regressions.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ion_ai::{
    BoxFuture, Content, GenerationControls, Message, ModelRef, ModelResponse, ModelStreamEvent,
    ProviderError, Reasoning, ResponseTermination, Role, ToolCall, ToolChoice, ToolSpec, Usage,
};
use ion_core::{
    ContextPolicy, ConversationConfig, RunLimits, Services, SessionLimits, SessionSpec, Tool,
    ToolOutcome, ToolRegistry,
};

pub fn config() -> ConversationConfig {
    ConversationConfig {
        model: ModelRef {
            provider: "scripted".to_owned(),
            model: "test-model".to_owned(),
        },
        instructions: "be careful".to_owned(),
        controls: GenerationControls {
            max_output_tokens: 4096,
            temperature: None,
            top_p: None,
            reasoning: Reasoning::ProviderDefault,
            tool_choice: ToolChoice::Auto,
            parallel_tool_calls: false,
        },
        project_context: vec![Message {
            role: Role::User,
            content: vec![Content::Text("project rules".to_owned())],
            provider_replay: None,
        }],
        tool_names: Vec::new(),
        context: ContextPolicy {
            max_request_bytes: 1024 * 1024,
            max_input_tokens: 100_000,
        },
        limits: RunLimits {
            max_model_steps: 8,
            max_attempts_per_step: 3,
            max_cost_microusd: None,
            deadline_ms: 30_000,
            max_response_bytes: 1024 * 1024,
            max_tool_output_bytes: 64 * 1024,
        },
    }
}

pub fn spec() -> SessionSpec {
    SessionSpec {
        limits: SessionLimits::default(),
        config: config(),
    }
}

/// A unique temporary database path; the caller removes it.
pub fn database(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ion-c1-{}-{}-{}",
        name,
        std::process::id(),
        uuid_like()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.join("session.sqlite")
}

fn uuid_like() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default()
}

/// A tool that never returns, so cancellation and parking can be observed.
pub struct PendingTool {
    name: String,
}

impl PendingTool {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
        })
    }
}

impl Tool for PendingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "never returns".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn identity(&self) -> String {
        format!("{}@pending-1", self.name)
    }

    fn execute<'a>(&'a self, _call: &'a ToolCall) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async {
            std::future::pending::<()>().await;
            unreachable!("the pending tool never completes")
        })
    }
}

/// A tool whose first call reports an unresolved outcome.
pub struct UncertainTool {
    name: String,
}

impl UncertainTool {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
        })
    }
}

impl Tool for UncertainTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "may have happened".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn identity(&self) -> String {
        format!("{}@uncertain-1", self.name)
    }

    fn execute<'a>(&'a self, _call: &'a ToolCall) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { ToolOutcome::Indeterminate("the write may have reached disk".to_owned()) })
    }
}

/// Build the services a session runs with.
pub fn services(model: Arc<dyn ion_ai::ModelService>, tools: ToolRegistry) -> Services {
    Services::new(model, Arc::new(tools))
}

/// A completed assistant answer with no tool calls.
pub fn answer(text: &str) -> ModelResponse {
    ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::Text(text.to_owned())],
            provider_replay: None,
        },
        usage: Usage::known(10, 4),
        termination: ResponseTermination::Completed,
    }
}

/// A completed assistant answer that requests one tool call.
pub fn tool_answer(call: ToolCall) -> ModelResponse {
    ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![Content::ToolCall(call.clone())],
            provider_replay: None,
        },
        usage: Usage::known(12, 6),
        termination: ResponseTermination::Completed,
    }
}

pub fn stream(response: ModelResponse) -> Vec<ModelStreamEvent> {
    vec![ModelStreamEvent::Completed(response)]
}

pub fn open_error(kind: ion_ai::ProviderErrorKind, message: &str) -> ProviderError {
    ProviderError {
        kind,
        message: message.to_owned(),
    }
}

/// Poll until `check` returns `Some`, or fail after a bounded wait.
pub async fn eventually<T>(mut check: impl AsyncFnMut() -> Option<T>) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(value) = check().await {
            return value;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "condition did not become true before the deadline"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
