//! ChatGPT subscription client using Responses API.

use crate::provider::error::Error;
use crate::provider::http::SseParser;
use crate::provider::types::{
    ChatRequest, ContentBlock, Message, Role, StreamEvent, ToolCallEvent,
};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE,
};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

/// ChatGPT Codex backend base URL (matches Codex CLI).
const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
/// Originator header value (match Codex CLI).
const ORIGINATOR: &str = "codex_cli_rs";

/// ChatGPT Responses API client.
pub struct ChatGptResponsesClient {
    client: reqwest::Client,
    access_token: String,
    account_id: Option<String>,
}

impl ChatGptResponsesClient {
    /// Create a new ChatGPT Responses client.
    pub fn new(access_token: impl Into<String>, account_id: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            access_token: access_token.into(),
            account_id,
        }
    }

    fn build_headers(&self, accept_sse: bool) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let auth = format!("Bearer {}", self.access_token);
        headers.insert(AUTHORIZATION, HeaderValue::from_str(&auth).unwrap());
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(if accept_sse {
                "text/event-stream"
            } else {
                "application/json"
            }),
        );
        headers.insert(
            HeaderName::from_static("originator"),
            HeaderValue::from_static(ORIGINATOR),
        );
        // Match Codex CLI user-agent format
        let ua = format!(
            "{ORIGINATOR}/{} ({} {}; {})",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            "ion"
        );
        if let Ok(value) = HeaderValue::from_str(&ua) {
            headers.insert(reqwest::header::USER_AGENT, value);
        }
        if let Some(account_id) = self.account_id.as_deref() {
            if let Ok(value) = HeaderValue::from_str(account_id) {
                headers.insert(
                    HeaderName::from_static("chatgpt-account-id"),
                    value,
                );
            }
        }
        headers
    }

    fn build_request(&self, request: &ChatRequest, stream: bool) -> ResponsesRequest {
        let (instructions, input) = build_instructions_and_input(request);
        let tools = build_tools(request);

        ResponsesRequest {
            model: request.model.clone(),
            instructions,
            input,
            tools,
            tool_choice: if request.tools.is_empty() {
                None
            } else {
                Some("auto")
            },
            parallel_tool_calls: if request.tools.is_empty() { None } else { Some(true) },
            store: false,
            stream,
            include: vec!["reasoning.encrypted_content".to_string()],
        }
    }

    /// Make a non-streaming responses request.
    pub async fn complete(&self, request: ChatRequest) -> Result<Message, Error> {
        let body = self.build_request(&request, false);
        let url = format!("{CHATGPT_BASE_URL}/responses");

        let response = self
            .client
            .post(&url)
            .headers(self.build_headers(false))
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Api(format!("Request failed: {e}")))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| Error::Api(format!("Failed to read response: {e}")))?;

        if !status.is_success() {
            return Err(Error::Api(format!("HTTP {status}: {text}")));
        }

        let value: Value = serde_json::from_str(&text).map_err(|e| {
            Error::Api(format!("Failed to parse response: {e}\nBody: {text}"))
        })?;
        let text = extract_output_text(&value);
        Ok(Message {
            role: Role::Assistant,
            content: Arc::new(vec![ContentBlock::Text { text }]),
        })
    }

    /// Stream a responses request.
    pub async fn stream(
        &self,
        request: ChatRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<(), Error> {
        use futures::StreamExt;

        let body = self.build_request(&request, true);
        let url = format!("{CHATGPT_BASE_URL}/responses");

        let response = self
            .client
            .post(&url)
            .headers(self.build_headers(true))
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Stream(format!("Request failed: {e}")))?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Stream(format!("HTTP {status}: {text}")));
        }

        let mut stream = response.bytes_stream();
        let mut parser = SseParser::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| Error::Stream(format!("Stream error: {e}")))?;
            let text = String::from_utf8_lossy(&chunk);

            for event in parser.feed(&text) {
                if event.data.is_empty() {
                    continue;
                }

                if let Some(stream_event) = parse_response_event(&event.data, event.event.as_deref())
                {
                    match stream_event {
                        ParsedEvent::TextDelta(delta) => {
                            let _ = tx.send(StreamEvent::TextDelta(delta)).await;
                        }
                        ParsedEvent::ToolCall(call) => {
                            let _ = tx.send(StreamEvent::ToolCall(call)).await;
                        }
                        ParsedEvent::Done => {
                            let _ = tx.send(StreamEvent::Done).await;
                            return Ok(());
                        }
                        ParsedEvent::Error(message) => {
                            return Err(Error::Stream(message));
                        }
                    }
                }
            }
        }

        let _ = tx.send(StreamEvent::Done).await;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    instructions: String,
    input: Vec<ResponseInputItem>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    store: bool,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    include: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseInputItem {
    Message {
        role: String,
        content: Vec<ResponseContent>,
    },
    FunctionCall {
        call_id: String,
        name: String,
        arguments: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseContent {
    InputText { text: String },
    OutputText { text: String },
    InputImage { image_url: String },
}


#[derive(Debug)]
enum ParsedEvent {
    TextDelta(String),
    ToolCall(ToolCallEvent),
    Done,
    Error(String),
}

fn build_instructions_and_input(request: &ChatRequest) -> (String, Vec<ResponseInputItem>) {
    let mut instructions = String::new();
    let mut input = Vec::new();

    if let Some(ref sys) = request.system {
        instructions.push_str(sys);
    }

    for msg in request.messages.iter() {
        match msg.role {
            Role::System => {
                let text = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    if !instructions.is_empty() {
                        instructions.push('\n');
                    }
                    instructions.push_str(&text);
                }
            }
            Role::User => {
                let content = msg
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(ResponseContent::InputText {
                            text: text.clone(),
                        }),
                        ContentBlock::Image { media_type, data } => Some(ResponseContent::InputImage {
                            image_url: format!("data:{media_type};base64,{data}"),
                        }),
                        _ => None,
                    })
                    .collect::<Vec<_>>();

                if !content.is_empty() {
                    input.push(ResponseInputItem::Message {
                        role: "user".to_string(),
                        content,
                    });
                }
            }
            Role::Assistant => {
                let mut content = Vec::new();
                for block in msg.content.iter() {
                    match block {
                        ContentBlock::Text { text } => {
                            content.push(ResponseContent::OutputText { text: text.clone() });
                        }
                        ContentBlock::ToolCall { id, name, arguments } => {
                            let args = serde_json::to_string(arguments)
                                .unwrap_or_else(|_| "{}".to_string());
                            input.push(ResponseInputItem::FunctionCall {
                                call_id: id.clone(),
                                name: name.clone(),
                                arguments: args,
                            });
                        }
                        _ => {}
                    }
                }

                if !content.is_empty() {
                    input.push(ResponseInputItem::Message {
                        role: "assistant".to_string(),
                        content,
                    });
                }
            }
            Role::ToolResult => {
                for block in msg.content.iter() {
                    if let ContentBlock::ToolResult {
                        tool_call_id,
                        content,
                        is_error,
                    } = block
                    {
                        let output = if *is_error {
                            format!("[ERROR] {content}")
                        } else {
                            content.clone()
                        };
                        input.push(ResponseInputItem::FunctionCallOutput {
                            call_id: tool_call_id.clone(),
                            output,
                        });
                    }
                }
            }
        }
    }

    (instructions, input)
}

fn build_tools(request: &ChatRequest) -> Vec<Value> {
    request
        .tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            })
        })
        .collect()
}

fn parse_response_event(data: &str, event_type: Option<&str>) -> Option<ParsedEvent> {
    let value: Value = serde_json::from_str(data).ok()?;
    let event = event_type
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or_default();

    match event {
        "response.output_text.delta" => value
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| ParsedEvent::TextDelta(delta.to_string())),
        "response.output_item.done" | "response.output_item.added" => {
            if let Some(item) = value.get("item") {
                if let Some(tool_call) = extract_tool_call(item) {
                    return Some(ParsedEvent::ToolCall(tool_call));
                }
                let text = extract_output_text(item);
                if !text.is_empty() {
                    return Some(ParsedEvent::TextDelta(text));
                }
            }
            None
        }
        "response.completed" => Some(ParsedEvent::Done),
        "response.failed" => {
            let message = value
                .get("response")
                .and_then(|v| v.get("error"))
                .and_then(|v| v.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("response.failed")
                .to_string();
            Some(ParsedEvent::Error(message))
        }
        _ => None,
    }
}

fn extract_output_text(value: &Value) -> String {
    let mut out = String::new();
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let items = if output.is_empty() {
        value.get("content").and_then(Value::as_array).cloned().unwrap_or_default()
    } else {
        output
    };

    for item in items {
        if let Some(contents) = item.get("content").and_then(Value::as_array) {
            for content in contents {
                if content.get("type").and_then(Value::as_str) == Some("output_text") {
                    if let Some(text) = content.get("text").and_then(Value::as_str) {
                        out.push_str(text);
                    }
                }
            }
        }
    }

    out
}

fn extract_tool_call(item: &Value) -> Option<ToolCallEvent> {
    let item_type = item.get("type").and_then(Value::as_str)?;
    if item_type != "function_call" {
        return None;
    }
    let call_id = item.get("call_id").and_then(Value::as_str)?;
    let name = item.get("name").and_then(Value::as_str)?;
    let arguments_str = item.get("arguments").and_then(Value::as_str).unwrap_or("{}");
    let arguments = serde_json::from_str(arguments_str).unwrap_or(Value::Null);
    Some(ToolCallEvent {
        id: call_id.to_string(),
        name: name.to_string(),
        arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::{ContentBlock, Message};

    #[test]
    fn builds_input_with_output_text_for_assistant() {
        let request = ChatRequest {
            model: "gpt-test".into(),
            messages: Arc::new(vec![
                Message {
                    role: Role::User,
                    content: Arc::new(vec![ContentBlock::Text {
                        text: "hi".into(),
                    }]),
                },
                Message {
                    role: Role::Assistant,
                    content: Arc::new(vec![ContentBlock::Text {
                        text: "hello".into(),
                    }]),
                },
            ]),
            system: None,
            tools: Arc::new(Vec::new()),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let (_instructions, input) = build_instructions_and_input(&request);
        let assistant = input
            .iter()
            .find_map(|item| match item {
                ResponseInputItem::Message { role, content } if role == "assistant" => {
                    Some(content)
                }
                _ => None,
            })
            .expect("assistant message");

        assert!(assistant.iter().any(|c| matches!(c, ResponseContent::OutputText { .. })));
    }
}
