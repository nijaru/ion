use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Content {
    Text(String),
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    ToolResult {
        call_id: String,
        name: String,
        result: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<Content>,
    /// Opaque provider replay data may survive without making the semantic
    /// message schema provider-specific.
    pub provider_replay: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelRequest {
    pub model: ModelRef,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelResponse {
    pub message: Message,
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ModelStreamEvent {
    TextDelta(String),
    ToolCall(Content),
    Usage(Usage),
    Completed(ModelResponse),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderErrorKind {
    Authentication,
    Permission,
    InvalidRequest,
    ContextLength,
    RateLimited,
    Transport,
    Server,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
}

pub struct ModelStream {
    receiver: mpsc::UnboundedReceiver<Result<ModelStreamEvent, ProviderError>>,
}

impl ModelStream {
    pub async fn next(&mut self) -> Option<Result<ModelStreamEvent, ProviderError>> {
        self.receiver.recv().await
    }
}

pub trait ModelService: Send + Sync {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>>;
}

enum Script {
    Stream(Vec<ModelStreamEvent>),
    OpenError(ProviderError),
}

struct ScriptedModelService {
    scripts: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<ModelRequest>>,
}

impl ScriptedModelService {
    fn new(scripts: impl IntoIterator<Item = Script>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().expect("request mutex").clone()
    }
}

impl ModelService for ScriptedModelService {
    fn stream<'a>(
        &'a self,
        request: ModelRequest,
    ) -> BoxFuture<'a, Result<ModelStream, ProviderError>> {
        Box::pin(async move {
            self.requests.lock().expect("request mutex").push(request);
            let script = self
                .scripts
                .lock()
                .expect("script mutex")
                .pop_front()
                .expect("scripted response");
            match script {
                Script::OpenError(error) => Err(error),
                Script::Stream(events) => {
                    let (sender, receiver) = mpsc::unbounded_channel();
                    for event in events {
                        sender.send(Ok(event)).expect("stream receiver exists");
                    }
                    drop(sender);
                    Ok(ModelStream { receiver })
                }
            }
        })
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        model: ModelRef {
            provider: "scripted".to_owned(),
            model: "test-model".to_owned(),
        },
        messages: vec![Message {
            role: Role::User,
            content: vec![Content::Text("inspect the file".to_owned())],
            provider_replay: None,
        }],
        tools: vec![ToolSpec {
            name: "read".to_owned(),
            description: "Read a file".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"]
            }),
        }],
    }
}

#[tokio::test]
async fn scripted_service_proves_minimal_provider_neutral_stream_boundary() {
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 4,
    };
    let tool_call = Content::ToolCall {
        id: "call-1".to_owned(),
        name: "read".to_owned(),
        arguments: serde_json::json!({"path": "src/lib.rs"}),
    };
    let response = ModelResponse {
        message: Message {
            role: Role::Assistant,
            content: vec![
                Content::Text("I will inspect it.".to_owned()),
                tool_call.clone(),
            ],
            provider_replay: Some(serde_json::json!({"opaque": "provider-state"})),
        },
        usage,
    };
    let expected = vec![
        ModelStreamEvent::TextDelta("I will inspect it.".to_owned()),
        ModelStreamEvent::ToolCall(tool_call),
        ModelStreamEvent::Usage(usage),
        ModelStreamEvent::Completed(response.clone()),
    ];
    let service = ScriptedModelService::new([Script::Stream(expected.clone())]);
    let service: &dyn ModelService = &service;

    let mut stream = service.stream(request()).await.expect("open stream");
    let mut observed = Vec::new();
    while let Some(event) = stream.next().await {
        observed.push(event.expect("scripted event"));
    }
    assert_eq!(observed, expected);
    assert_eq!(
        observed.last(),
        Some(&ModelStreamEvent::Completed(response))
    );
}

#[tokio::test]
async fn typed_provider_failure_crosses_boundary_without_hidden_retry() {
    let service = ScriptedModelService::new([Script::OpenError(ProviderError {
        kind: ProviderErrorKind::RateLimited,
        message: "try later".to_owned(),
    })]);

    let error = match service.stream(request()).await {
        Ok(_) => panic!("script must fail before stream creation"),
        Err(error) => error,
    };
    assert_eq!(error.kind, ProviderErrorKind::RateLimited);
    assert_eq!(service.requests().len(), 1);
}
