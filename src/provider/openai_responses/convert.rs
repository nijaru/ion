//! Conversion helpers for OpenAI Responses API.

use super::types::{
    ParsedEvent, Reasoning, ResponseContent, ResponseInputItem, ResponsesRequest, TextConfig,
    TextFormat,
};
use crate::provider::types::{ChatRequest, ContentBlock, Role, ToolCallEvent, Usage};
use serde_json::Value;

pub(crate) fn build_request(request: &ChatRequest, stream: bool) -> ResponsesRequest {
    let (instructions, input) = build_instructions_and_input(request);
    let tools = build_tools(request);

    let has_tools = !request.tools.is_empty();

    // Map thinking config to reasoning params
    let reasoning = request.thinking.as_ref().and_then(|t| {
        if t.enabled {
            let effort = match t.budget_tokens {
                Some(b) if b < 5000 => "low",
                Some(b) if b > 20000 => "high",
                _ => "medium",
            };
            Some(Reasoning {
                effort: Some(effort.to_string()),
                summary: Some("auto".to_string()),
            })
        } else {
            None
        }
    });

    ResponsesRequest {
        model: request.model.clone(),
        instructions,
        input,
        tools,
        tool_choice: if has_tools { Some("auto") } else { None },
        parallel_tool_calls: if has_tools { Some(true) } else { None },
        max_output_tokens: request.max_tokens,
        temperature: request.temperature,
        store: false,
        stream,
        reasoning,
        text: Some(TextConfig {
            format: Some(TextFormat { kind: "text" }),
        }),
        truncation: "auto",
    }
}

pub(crate) fn build_instructions_and_input(
    request: &ChatRequest,
) -> (String, Vec<ResponseInputItem>) {
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
                "strict": false,
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

        "response.reasoning_summary_text.delta" => value
            .get("delta")
            .and_then(Value::as_str)
            .map(|delta| ParsedEvent::ThinkingDelta(delta.to_string())),

        "response.function_call_arguments.delta" => {
            let call_id = value.get("item_id").and_then(Value::as_str)?;
            let delta = value.get("delta").and_then(Value::as_str)?;
            Some(ParsedEvent::ToolCallDelta {
                call_id: call_id.to_string(),
                delta: delta.to_string(),
            })
        }

        "response.function_call_arguments.done" => {
            let call_id = value.get("item_id").and_then(Value::as_str)?;
            let name = value.get("name").and_then(Value::as_str).unwrap_or("");
            let arguments = value
                .get("arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            Some(ParsedEvent::ToolCallDone {
                call_id: call_id.to_string(),
                name: name.to_string(),
                arguments: arguments.to_string(),
            })
        }

        "response.output_item.done" => {
            // Only extract tool calls; text was already streamed via output_text.delta
            let item = value.get("item")?;
            extract_tool_call(item).map(ParsedEvent::ToolCall)
        }

        "response.completed" => {
            let usage = value
                .get("response")
                .and_then(|r| r.get("usage"))
                .map(extract_usage);
            if let Some(usage) = usage {
                return Some(ParsedEvent::Usage(usage));
            }
            Some(ParsedEvent::Done)
        }

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

pub(crate) fn extract_usage(usage: &Value) -> Usage {
    let input = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let output = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let cache_read = usage
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
    }
}

/// Extract all output content blocks (text + tool calls) from a non-streaming response.
pub(crate) fn extract_output(value: &Value) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    let empty = Vec::new();
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(contents) = item.get("content").and_then(Value::as_array) {
                    for content in contents {
                        if content.get("type").and_then(Value::as_str) == Some("output_text")
                            && let Some(text) = content.get("text").and_then(Value::as_str)
                        {
                            blocks.push(ContentBlock::Text {
                                text: text.to_string(),
                            });
                        }
                    }
                }
            }
            Some("function_call") => {
                if let Some(call) = extract_tool_call(item) {
                    blocks.push(ContentBlock::ToolCall {
                        id: call.id,
                        name: call.name,
                        arguments: call.arguments,
                    });
                }
            }
            _ => {}
        }
    }

    // Fallback: if no output items, try extracting text from content array
    if blocks.is_empty() {
        let text = extract_output_text(value);
        if !text.is_empty() {
            blocks.push(ContentBlock::Text { text });
        }
    }

    blocks
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
    use crate::provider::types::{ContentBlock, Message, ThinkingConfig, ToolDefinition};
    use std::sync::Arc;

    #[test]
    fn build_request_structure() {
        let request = ChatRequest {
            model: "gpt-4.1".into(),
            messages: Arc::new(vec![Message {
                role: Role::User,
                content: Arc::new(vec![ContentBlock::Text {
                    text: "hello".into(),
                }]),
            }]),
            system: Some("You are helpful.".into()),
            tools: Arc::new(vec![ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            max_tokens: Some(4096),
            temperature: None,
            thinking: None,
        };

        let req = build_request(&request, true);
        assert_eq!(req.model, "gpt-4.1");
        assert!(req.stream);
        assert_eq!(req.tool_choice, Some("auto"));
        assert_eq!(req.parallel_tool_calls, Some(true));
        assert_eq!(req.max_output_tokens, Some(4096));
        assert_eq!(req.truncation, "auto");
        assert!(req.reasoning.is_none());
        assert!(!req.tools.is_empty());
    }

    #[test]
    fn build_request_with_thinking() {
        let request = ChatRequest {
            model: "o3".into(),
            messages: Arc::new(vec![Message {
                role: Role::User,
                content: Arc::new(vec![ContentBlock::Text {
                    text: "think".into(),
                }]),
            }]),
            system: None,
            tools: Arc::new(Vec::new()),
            max_tokens: None,
            temperature: None,
            thinking: Some(ThinkingConfig {
                enabled: true,
                budget_tokens: Some(10000),
            }),
        };

        let req = build_request(&request, false);
        let reasoning = req.reasoning.unwrap();
        assert_eq!(reasoning.effort.as_deref(), Some("medium"));
        assert_eq!(reasoning.summary.as_deref(), Some("auto"));
    }

    #[test]
    fn build_request_maps_budget_to_effort() {
        let make = |budget| {
            let request = ChatRequest {
                model: "o3".into(),
                messages: Arc::new(vec![]),
                system: None,
                tools: Arc::new(vec![]),
                max_tokens: None,
                temperature: None,
                thinking: Some(ThinkingConfig {
                    enabled: true,
                    budget_tokens: budget,
                }),
            };
            build_request(&request, false)
                .reasoning
                .unwrap()
                .effort
                .unwrap()
        };

        assert_eq!(make(Some(1000)), "low");
        assert_eq!(make(Some(10000)), "medium");
        assert_eq!(make(Some(50000)), "high");
        assert_eq!(make(None), "medium");
    }

    #[test]
    fn build_request_forwards_temperature() {
        let request = ChatRequest {
            model: "gpt-4.1".into(),
            messages: Arc::new(vec![]),
            system: None,
            tools: Arc::new(vec![]),
            max_tokens: None,
            temperature: Some(0.7),
            thinking: None,
        };
        let req = build_request(&request, false);
        assert_eq!(req.temperature, Some(0.7));
    }

    #[test]
    fn build_tools_includes_strict_false() {
        let request = ChatRequest {
            model: "gpt-4.1".into(),
            messages: Arc::new(vec![]),
            system: None,
            tools: Arc::new(vec![ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({"type": "object"}),
            }]),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };
        let req = build_request(&request, false);
        assert_eq!(req.tools[0]["strict"], serde_json::json!(false));
    }

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

        assert!(assistant
            .iter()
            .any(|c| matches!(c, ResponseContent::OutputText { .. })));
    }

    #[test]
    fn builds_tool_call_and_result() {
        let request = ChatRequest {
            model: "gpt-test".into(),
            messages: Arc::new(vec![
                Message {
                    role: Role::Assistant,
                    content: Arc::new(vec![ContentBlock::ToolCall {
                        id: "call_1".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({"path": "/tmp/test"}),
                    }]),
                },
                Message {
                    role: Role::ToolResult,
                    content: Arc::new(vec![ContentBlock::ToolResult {
                        tool_call_id: "call_1".into(),
                        content: "file contents".into(),
                        is_error: false,
                    }]),
                },
            ]),
            system: None,
            tools: Arc::new(Vec::new()),
            max_tokens: None,
            temperature: None,
            thinking: None,
        };

        let (_, input) = build_instructions_and_input(&request);
        assert!(input
            .iter()
            .any(|i| matches!(i, ResponseInputItem::FunctionCall { name, .. } if name == "read")));
        assert!(input.iter().any(
            |i| matches!(i, ResponseInputItem::FunctionCallOutput { output, .. } if output == "file contents")
        ));
    }

    #[test]
    fn parse_text_delta() {
        let data = r#"{"type":"response.output_text.delta","delta":"hello"}"#;
        let event = parse_response_event(data, Some("response.output_text.delta"));
        assert!(matches!(event, Some(ParsedEvent::TextDelta(ref s)) if s == "hello"));
    }

    #[test]
    fn parse_thinking_delta() {
        let data = r#"{"type":"response.reasoning_summary_text.delta","delta":"thinking..."}"#;
        let event = parse_response_event(data, Some("response.reasoning_summary_text.delta"));
        assert!(matches!(event, Some(ParsedEvent::ThinkingDelta(ref s)) if s == "thinking..."));
    }

    #[test]
    fn parse_tool_call_arguments_delta() {
        let data =
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":0,"delta":"{\"pa"}"#;
        let event = parse_response_event(data, Some("response.function_call_arguments.delta"));
        assert!(
            matches!(event, Some(ParsedEvent::ToolCallDelta { call_id, delta }) if call_id == "fc_1" && delta == "{\"pa")
        );
    }

    #[test]
    fn parse_tool_call_arguments_done() {
        let data = r#"{"type":"response.function_call_arguments.done","item_id":"fc_1","name":"read","arguments":"{\"path\":\"/tmp\"}"}"#;
        let event = parse_response_event(data, Some("response.function_call_arguments.done"));
        assert!(
            matches!(event, Some(ParsedEvent::ToolCallDone { call_id, arguments, .. }) if call_id == "fc_1" && arguments == "{\"path\":\"/tmp\"}")
        );
    }

    #[test]
    fn parse_output_item_done_tool_call() {
        let data = r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"fc_1","name":"read","arguments":"{\"path\":\"/tmp\"}"}}"#;
        let event = parse_response_event(data, Some("response.output_item.done"));
        assert!(matches!(event, Some(ParsedEvent::ToolCall(ref tc)) if tc.name == "read"));
    }

    #[test]
    fn parse_completed_with_usage() {
        let data = r#"{"type":"response.completed","response":{"usage":{"input_tokens":100,"output_tokens":50,"input_tokens_details":{"cached_tokens":20}}}}"#;
        let event = parse_response_event(data, Some("response.completed"));
        match event {
            Some(ParsedEvent::Usage(u)) => {
                assert_eq!(u.input_tokens, 100);
                assert_eq!(u.output_tokens, 50);
                assert_eq!(u.cache_read_tokens, 20);
            }
            _ => panic!("expected Usage event"),
        }
    }

    #[test]
    fn parse_completed_without_usage() {
        let data = r#"{"type":"response.completed","response":{}}"#;
        let event = parse_response_event(data, Some("response.completed"));
        assert!(matches!(event, Some(ParsedEvent::Done)));
    }

    #[test]
    fn parse_failed() {
        let data =
            r#"{"type":"response.failed","response":{"error":{"message":"rate limited"}}}"#;
        let event = parse_response_event(data, Some("response.failed"));
        assert!(matches!(event, Some(ParsedEvent::Error(ref s)) if s == "rate limited"));
    }
}
