//! ask_user tool — agent requests clarifying input from the user.

use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::oneshot;

/// A request from the agent for user input.
pub struct AskUserRequest {
    pub question: String,
    pub options: Vec<String>,
    pub response_tx: oneshot::Sender<String>,
}

pub type AskUserSender = tokio::sync::mpsc::Sender<AskUserRequest>;
pub type AskUserReceiver = tokio::sync::mpsc::Receiver<AskUserRequest>;

/// Create a bounded channel for ask_user requests (capacity 1: agent blocks until answered).
pub fn ask_user_channel() -> (AskUserSender, AskUserReceiver) {
    tokio::sync::mpsc::channel(1)
}

/// Tool that lets the agent pause and ask the user a question.
pub struct AskUserTool {
    tx: AskUserSender,
}

impl AskUserTool {
    pub fn new(tx: AskUserSender) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &'static str {
        "ask_user"
    }

    fn description(&self) -> &'static str {
        "Ask the user a question and wait for their response. Use only when required information is missing and cannot be inferred. Provide options array for multiple-choice questions."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user."
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of choices. If provided, displays as a numbered list."
                }
            },
            "required": ["question"]
        })
    }

    fn danger_level(&self) -> DangerLevel {
        DangerLevel::Safe
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("question is required".to_string()))?
            .to_string();

        let options: Vec<String> = args
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let (response_tx, response_rx) = oneshot::channel();

        self.tx
            .send(AskUserRequest {
                question,
                options,
                response_tx,
            })
            .await
            .map_err(|_| ToolError::ExecutionFailed("ask_user channel closed".to_string()))?;

        tokio::select! {
            result = response_rx => {
                let response = result.map_err(|_| {
                    ToolError::ExecutionFailed("ask_user cancelled".to_string())
                })?;
                Ok(ToolResult {
                    content: response,
                    is_error: false,
                    metadata: None,
                })
            }
            () = ctx.abort_signal.cancelled() => {
                Err(ToolError::ExecutionFailed(
                    "Task cancelled while waiting for user input".to_string(),
                ))
            }
        }
    }
}
