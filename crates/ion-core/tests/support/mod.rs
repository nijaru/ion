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
    ContextPolicy, ConversationConfig, RunLimits, Services, SessionLimits, SessionSpec, Stop, Tool,
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

/// Session limits that reach a stop grace quickly.
pub fn limits() -> SessionLimits {
    SessionLimits {
        // Long enough for a cooperative action to report, short enough for a
        // test to reach the bounded join.
        execution_join_grace_ms: 150,
        ..SessionLimits::default()
    }
}

pub fn spec() -> SessionSpec {
    SessionSpec {
        limits: limits(),
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

/// A tool that honors a stop request and reports that it never took effect.
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
            description: "runs until it is asked to stop".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn identity(&self) -> String {
        format!("{}@pending-1", self.name)
    }

    fn execute<'a>(&'a self, _call: &'a ToolCall, stop: &'a Stop) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            stop.requested().await;
            ToolOutcome::KnownFailure("the action was stopped before it took effect".to_owned())
        })
    }
}

/// A tool that ignores a stop request and completes only when a test says so.
///
/// It exists to observe the other half of the stop contract: an action that
/// outlives its turn is still owned, and its late report is published as
/// evidence rather than used to revise a settled turn.
pub struct DeafTool {
    name: String,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

impl DeafTool {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        })
    }

    /// Resolves once the action has begun.
    pub async fn started(&self) {
        self.started.notified().await;
    }

    /// Let the action finish, whatever the session asked it to do.
    pub fn release(&self) {
        self.release.notify_waiters();
    }
}

impl Tool for DeafTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "ignores stop requests".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn identity(&self) -> String {
        format!("{}@deaf-1", self.name)
    }

    fn execute<'a>(&'a self, _call: &'a ToolCall, _stop: &'a Stop) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.started.notify_one();
            self.release.notified().await;
            ToolOutcome::Completed(serde_json::json!({"completed": true}))
        })
    }
}

/// A tool that ignores a stop request and never reports.
pub struct ImmortalTool {
    name: String,
    started: tokio::sync::Notify,
}

impl ImmortalTool {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
            started: tokio::sync::Notify::new(),
        })
    }

    /// Resolves once the action has begun.
    pub async fn started(&self) {
        self.started.notified().await;
    }
}

impl Tool for ImmortalTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "never reports".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn identity(&self) -> String {
        format!("{}@immortal-1", self.name)
    }

    fn execute<'a>(&'a self, _call: &'a ToolCall, _stop: &'a Stop) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            self.started.notify_one();
            std::future::pending::<()>().await;
            unreachable!("the immortal tool never completes")
        })
    }
}

/// A tool that panics while it runs.
pub struct PanickingTool {
    name: String,
}

impl PanickingTool {
    pub fn new(name: &str) -> Arc<Self> {
        Arc::new(Self {
            name: name.to_owned(),
        })
    }
}

impl Tool for PanickingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.clone(),
            description: "panics".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn identity(&self) -> String {
        format!("{}@panicking-1", self.name)
    }

    fn execute<'a>(&'a self, _call: &'a ToolCall, _stop: &'a Stop) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async { panic!("the action blew up while it was running") })
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

    fn execute<'a>(&'a self, _call: &'a ToolCall, _stop: &'a Stop) -> BoxFuture<'a, ToolOutcome> {
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
