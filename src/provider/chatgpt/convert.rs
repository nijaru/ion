//! Conversion helpers for ChatGPT Responses API.

use super::types::{
    ParsedEvent, ResponseContent, ResponseInputItem, ResponsesRequest,
};
use crate::provider::types::{ChatRequest, ContentBlock, Role, ToolCallEvent};
use serde_json::Value;

pub(crate) fn build_request(request: &ChatRequest, stream: bool) -> ResponsesRequest {
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
        parallel_tool_calls: if request.tools.is_empty() {
            None
        } else {
            Some(true)
        },
        store: false,
        stream,
        include: vec!["reasoning.encrypted_content".to_string()],
    }
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
                        ContentBlock::Text { text } => {
                            Some(ResponseContent::InputText { text: text.clone() })
                        }
                        ContentBlock::Image { media_type, data } => {
                            Some(ResponseContent::InputImage {
                                image_url: format!("data:{media_type};base64,{data}"),
                            })
                        }
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
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } => {
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

pub(crate) fn parse_response_event(data: &str, event_type: Option<&str>) -> Option<ParsedEvent> {
    let value: Value = serde_json::from_str(data).ok()?;
    let event = event_type
        .or_else(|| value.get("type").and_then(Value::as_str))
        .unwrap_or_default();

    match event {
        "response.output_text.delta" => value
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| ParsedEvent::TextDelta(delta.to_string())),
        "response.output_item.done" => {
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

pub(crate) fn extract_output_text(value: &Value) -> String {
    let mut out = String::new();
    let empty = Vec::new();
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let items = if output.is_empty() {
        value
            .get("content")
            .and_then(Value::as_array)
            .unwrap_or(&empty)
    } else {
        output
    };

    for item in items {
        if let Some(contents) = item.get("content").and_then(Value::as_array) {
            for content in contents {
                if content.get("type").and_then(Value::as_str) == Some("output_text")
                    && let Some(text) = content.get("text").and_then(Value::as_str)
                {
                    out.push_str(text);
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
    let arguments_str = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or("{}");
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
    use std::sync::Arc;

    #[test]
    fn builds_input_with_output_text_for_assistant() {
        let request = ChatRequest {
            model: "gpt-test".into(),
            messages: Arc::new(vec![
                Message {
                    role: Role::User,
                    content: Arc::new(vec![ContentBlock::Text { text: "hi".into() }]),
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

        assert!(
            assistant
                .iter()
                .any(|c| matches!(c, ResponseContent::OutputText { .. }))
        );
    }
}
