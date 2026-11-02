//! Stateless Responses input and validated native continuation records.
use super::tool_ids::ToolIds;
use super::transcript::{
    Part, Transcript, incompatible_replay, provider_replay, tool_result_content,
};
use super::*;

pub(super) const RESPONSES_REPLAY: &str = "openai_responses_output";

pub(super) fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ProviderError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(&format!("invalid Responses {key}")))
}

/// Preserve native message boundaries, refusals and reasoning in replay; the
/// neutral projection carries only human-readable summaries, answers and calls.
pub(super) fn project_output(
    output: &[Value],
    completed: bool,
) -> Result<Vec<Content>, ProviderError> {
    let mut content = Vec::new();
    let mut ids = BTreeSet::new();
    let mut calls = BTreeSet::new();
    for item in output {
        let id = string(item, "id")?;
        if id.is_empty() || !ids.insert(id) {
            return Err(invalid("duplicate or empty Responses item ID"));
        }
        match string(item, "type")? {
            "message" => {
                if string(item, "role")? != "assistant" {
                    return Err(invalid("Responses output is not assistant content"));
                }
                if let Some(phase) = item.get("phase").filter(|v| !v.is_null())
                    && !matches!(phase.as_str(), Some("commentary" | "final_answer"))
                {
                    return Err(unsupported("unsupported Responses message phase"));
                }
                for part in item["content"]
                    .as_array()
                    .ok_or_else(|| invalid("invalid Responses message content"))?
                {
                    let text = match string(part, "type")? {
                        "output_text" => string(part, "text")?,
                        "refusal" => string(part, "refusal")?,
                        _ => return Err(unsupported("unsupported Responses output content")),
                    };
                    content.push(Content::Text(text.to_owned()));
                }
            }
            "reasoning" => {
                if let Some(ciphertext) = item.get("encrypted_content").filter(|v| !v.is_null())
                    && !ciphertext.is_string()
                {
                    return Err(invalid("invalid Responses encrypted reasoning"));
                }
                let summary = item["summary"]
                    .as_array()
                    .ok_or_else(|| invalid("invalid Responses reasoning summary"))?;
                for part in summary {
                    if string(part, "type")? != "summary_text" {
                        return Err(unsupported("unsupported Responses reasoning summary"));
                    }
                    content.push(Content::Thinking(string(part, "text")?.to_owned()));
                }
                if let Some(parts) = item.get("content").filter(|v| !v.is_null()) {
                    for part in parts
                        .as_array()
                        .ok_or_else(|| invalid("invalid Responses reasoning content"))?
                    {
                        if string(part, "type")? != "reasoning_text" {
                            return Err(unsupported("unsupported Responses reasoning content"));
                        }
                        let text = string(part, "text")?;
                        if summary.is_empty() {
                            content.push(Content::Thinking(text.to_owned()));
                        }
                    }
                }
            }
            "function_call" => {
                if let Some(namespace) = item.get("namespace").filter(|v| !v.is_null()) {
                    let namespace = namespace
                        .as_str()
                        .ok_or_else(|| invalid("invalid Responses function namespace"))?;
                    if !namespace.is_empty() {
                        return Err(unsupported(
                            "Responses namespaced functions are not in the flat tool catalogue",
                        ));
                    }
                }
                if completed && item["status"].as_str() != Some("completed") {
                    return Err(invalid(
                        "Responses completed with an unfinished function call",
                    ));
                }
                let id = string(item, "call_id")?;
                let name = string(item, "name")?;
                if id.is_empty() || !calls.insert(id) || !valid_identifier(name) {
                    return Err(invalid("invalid or duplicate Responses function call"));
                }
                let (arguments, raw_arguments) =
                    parse_tool_arguments(string(item, "arguments")?.to_owned());
                content.push(Content::ToolCall(ToolCall {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    arguments,
                    raw_arguments,
                }));
            }
            _ => return Err(unsupported("unsupported Responses output item")),
        }
    }
    // Opaque-only output still has a valid neutral message, without exposing
    // ciphertext as human thinking or inventing readable provider content.
    if content.is_empty() {
        content.push(Content::Text(String::new()));
    }
    Ok(content)
}

fn visible(content: &[Content]) -> Vec<&Content> {
    content
        .iter()
        .filter(|part| !matches!(part, Content::Thinking(_)))
        .collect()
}

fn replay_output<'a>(
    message: &'a Message,
    request: &ModelRequest,
) -> Result<Option<(&'a [Value], bool)>, ProviderError> {
    let Some(replay) = provider_replay(message, &request.route.effective.provider)? else {
        return Ok(None);
    };
    if replay.kind != RESPONSES_REPLAY {
        return Err(incompatible_replay());
    }
    let output = replay.data["output"]
        .as_array()
        .ok_or_else(|| invalid("invalid Responses output replay"))?;
    // Truncated calls are retained as non-dispatched calls by Core. Their raw
    // argument text must remain coupled to the original continuation record.
    let projection = project_output(output, false)?;
    if visible(&projection) != visible(&message.content) {
        return Err(invalid("Responses replay differs from committed content"));
    }
    let same_model = string(&replay.data, "model")? == request.route.effective.model;
    if !same_model && output.iter().any(|item| item["type"] == "reasoning") {
        return Err(unsupported(
            "Responses reasoning belongs to a different model; explicitly compact or start a new Session",
        ));
    }
    Ok(Some((output, same_model)))
}

fn image(image: &ion_ai::ImageContent) -> Value {
    json!({"type":"input_image", "image_url":format!("data:{};base64,{}",image.mime_type().as_str(),image.data()), "detail":"auto"})
}

pub(super) fn responses_body(request: &ModelRequest) -> Result<Value, ProviderError> {
    validate_request(request)?;
    let mut body = control_fields(
        &request.route.effective,
        &request.controls,
        HttpWire::OpenAiResponses,
        false,
    )?;
    let mut input = Vec::new();
    let mut transcript = Transcript::default();
    let mut ids = ToolIds::default();
    for message in &request.messages {
        transcript.begin(message)?;
        let replay = replay_output(message, request)?;
        let mut parts = Vec::new();
        let mut aliases = BTreeMap::new();
        for part in &message.content {
            match transcript.part(message.role, part)? {
                Part::Thinking => {}
                Part::Text(text) => {
                    if replay.is_none() {
                        parts.push(json!({"type":"input_text","text":text}));
                    }
                }
                Part::Image(value) => parts.push(image(value)),
                Part::ToolCall { call, id } => {
                    *id =
                        ids.for_call(&call.id, replay.is_some_and(|(_, same)| same), "Responses")?;
                    aliases.insert(call.id.as_str(), id.clone());
                    if replay.is_none() {
                        if !parts.is_empty() {
                            input.push(json!({"type":"message","role":"assistant","content":std::mem::take(&mut parts)}));
                        }
                        input.push(json!({"type":"function_call","call_id":id.as_ref(),"name":call.name,
                            "arguments":call.raw_arguments.clone().unwrap_or_else(||call.arguments.to_string())}));
                    }
                }
                Part::ToolResult { result, id } => {
                    let text = tool_result_content(result)?;
                    let output = if result.images.is_empty() {
                        json!(text)
                    } else {
                        let mut output = vec![json!({"type":"input_text","text":text})];
                        output.extend(result.images.iter().map(image));
                        Value::Array(output)
                    };
                    input.push(json!({"type":"function_call_output","call_id":id,"output":output}));
                }
            }
        }
        if let Some((output, same_model)) = replay {
            for item in output {
                let mut item = item.clone();
                if !same_model && item["type"] == "function_call" {
                    // Call item IDs may be paired with reasoning from the old
                    // model. Plain message IDs/phases remain native records.
                    item.as_object_mut().unwrap().remove("id");
                }
                if item["type"] == "function_call" {
                    let original = string(&item, "call_id")?;
                    item["call_id"] = json!(
                        aliases
                            .get(original)
                            .ok_or_else(|| invalid("Responses replay omitted a call"))?
                    );
                }
                input.push(item);
            }
        } else if !parts.is_empty() {
            input.push(json!({"type":"message","role":if message.role == Role::User {"user"} else {"assistant"},"content":parts}));
        }
    }
    transcript.finish()?;
    body["model"] = json!(request.route.effective.model);
    body["input"] = Value::Array(input);
    if let Some(instructions) = &request.instructions {
        body["instructions"] = json!(instructions);
    }
    body["stream"] = json!(true);
    body["store"] = json!(false);
    body["include"] = json!(["reasoning.encrypted_content"]);
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({"type":"function","name":tool.name,
            "description":tool.description,"parameters":tool.input_schema,"strict":false})
                })
                .collect(),
        );
        body["parallel_tool_calls"] = json!(request.controls.parallel_tool_calls);
        body["tool_choice"] = match &request.controls.tool_choice {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::None => json!("none"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Named(name) => json!({"type":"function","name":name}),
        };
    }
    Ok(body)
}
