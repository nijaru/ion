//! Chat-compatible request encoding, including reasoning and tool-image replay.
use super::transcript::{
    Part, Transcript, incompatible_replay, provider_replay, tool_result_content,
};
use super::*;

pub(super) fn chat_body(request: &ModelRequest, wire: HttpWire) -> Result<Value, ProviderError> {
    validate_request(request)?;
    let mut body = control_fields(&request.route.effective, &request.controls, wire, false)?;
    let mut messages = Vec::new();
    if let Some(instructions) = &request.instructions {
        messages.push(json!({"role":"system","content":instructions}));
    }
    messages.extend(transcript_messages(request, wire)?);
    body["model"] = json!(request.route.effective.model);
    body["messages"] = Value::Array(messages);
    body["stream"] = json!(true);
    body["stream_options"] = json!({"include_usage":true});
    if wire == HttpWire::OpenRouterChat
        && let Some(id) = &request.provider_session_id
    {
        if id.is_empty() || id.chars().count() > 256 {
            return Err(invalid(
                "OpenRouter session ID must contain 1 to 256 characters",
            ));
        }
        body["session_id"] = json!(id);
    }

    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({"type":"function","function":{
            "name":tool.name,"description":tool.description,"parameters":tool.input_schema}})
                })
                .collect(),
        );
        body["tool_choice"] = match &request.controls.tool_choice {
            ToolChoice::None => json!("none"),
            ToolChoice::Auto => json!("auto"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Named(name) => json!({"type":"function","function":{"name":name}}),
        };
        body["parallel_tool_calls"] = json!(request.controls.parallel_tool_calls);
    }

    Ok(body)
}

fn transcript_messages(
    request: &ModelRequest,
    wire: HttpWire,
) -> Result<Vec<Value>, ProviderError> {
    let mut messages = Vec::new();
    let mut transcript = Transcript::default();
    let mut tool_images = Vec::new();
    for message in &request.messages {
        let reasoning_content = match provider_replay(message, &request.route.effective.provider)? {
            Some(replay)
                if matches!(wire, HttpWire::DeepSeekChat | HttpWire::MiMoChat)
                    && replay.kind == CHAT_REASONING_CONTENT_REPLAY =>
            {
                Some(
                    replay
                        .data
                        .as_str()
                        .ok_or_else(|| invalid("invalid reasoning-content replay"))?,
                )
            }
            Some(replay)
                if wire == HttpWire::OpenRouterChat
                    && replay.kind == OPENROUTER_PLAIN_REASONING_REPLAY =>
            {
                Some(
                    replay
                        .data
                        .as_str()
                        .ok_or_else(|| invalid("invalid OpenRouter reasoning replay"))?,
                )
            }
            Some(replay)
                if wire == HttpWire::OpenRouterChat && replay.kind == OPENROUTER_DETAILS_REPLAY =>
            {
                if !valid_openrouter_details(&replay.data) {
                    return Err(invalid("invalid OpenRouter reasoning replay"));
                }
                None
            }
            Some(_) => return Err(incompatible_replay()),
            None => None,
        };
        transcript.begin(message)?;
        if message.role != Role::Tool && !tool_images.is_empty() {
            append_tool_images(&mut messages, &mut tool_images);
        }
        let mut text = String::new();
        let mut user_blocks = Vec::new();
        let mut has_image = false;
        let mut calls = Vec::new();
        let mut results = Vec::new();
        for item in &message.content {
            match transcript.part(message.role, item)? {
                Part::Thinking => {}
                Part::Text(part) => {
                    text.push_str(part);
                    if message.role == Role::User && !part.is_empty() {
                        user_blocks.push(json!({"type":"text","text":part}));
                    }
                }
                Part::Image(image) => {
                    has_image = true;
                    user_blocks.push(json!({"type":"image_url","image_url":{
                        "url":format!("data:{};base64,{}", image.mime_type().as_str(), image.data())
                    }}));
                }
                Part::ToolCall { call, id } => {
                    calls.push(json!({"id":id,"type":"function","function":{
                        "name":call.name,"arguments":call.arguments.to_string()
                    }}));
                }
                Part::ToolResult { result, id } => {
                    results.push(json!({"role":"tool","tool_call_id":id,"content":tool_result_content(result)?}));
                    for image in &result.images {
                        tool_images.push(json!({"type":"image_url","image_url":{
                            "url":format!("data:{};base64,{}", image.mime_type().as_str(), image.data())
                        }}));
                    }
                }
            }
        }
        if message.role == Role::Tool {
            messages.extend(results);
            continue;
        }
        let role = if message.role == Role::Assistant {
            "assistant"
        } else {
            "user"
        };
        let content = if has_image {
            Value::Array(user_blocks)
        } else if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        };
        let mut value = json!({"role":role,"content":content});
        if !calls.is_empty() {
            value["tool_calls"] = Value::Array(calls);
        }
        if let Some(reasoning) = reasoning_content {
            let field = if wire == HttpWire::OpenRouterChat {
                "reasoning"
            } else {
                "reasoning_content"
            };
            value[field] = json!(reasoning);
        } else if message.role == Role::Assistant
            && !request.tools.is_empty()
            && matches!(wire, HttpWire::DeepSeekChat | HttpWire::MiMoChat)
        {
            value["reasoning_content"] = json!("");
        }
        if let Some(replay) = &message.provider_replay
            && replay.kind == OPENROUTER_DETAILS_REPLAY
        {
            value["reasoning_details"] = replay.data.clone();
        }
        messages.push(value);
    }
    if !tool_images.is_empty() {
        append_tool_images(&mut messages, &mut tool_images);
    }
    transcript.finish()?;
    Ok(messages)
}

fn append_tool_images(messages: &mut Vec<Value>, images: &mut Vec<Value>) {
    let mut content = vec![json!({"type":"text","text":"Attached image(s) from tool result:"})];
    content.append(images);
    messages.push(json!({"role":"user","content":content}));
}
