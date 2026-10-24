//! Native Anthropic requests, signed prefixes and inline context changes.
use super::tool_ids::ToolIds;
use super::transcript::{Part, Transcript, incompatible_replay, provider_replay};
use super::*;
use sha2::{Digest, Sha256};

struct AnthropicTimelinePlan<'a> {
    initial: &'a ModelContextState,
    changes: BTreeMap<usize, Vec<Value>>,
}

/// Replay the original assistant block array only when it still projects to
/// the committed provider-neutral message. A changed visible tool call or
/// answer cannot inherit an old signature.
fn validated_anthropic_replay(
    data: &Value,
    content: &[Content],
    request: &ModelRequest,
    preceding: &[Value],
    digest_context: Option<&ModelContextState>,
) -> Result<Vec<Value>, ProviderError> {
    let blocks = data["blocks"]
        .as_array()
        .filter(|blocks| !blocks.is_empty())
        .ok_or_else(|| invalid("invalid Anthropic content replay"))?;
    let digest = data["prefix_sha256"]
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| invalid("invalid Anthropic replay prefix"))?;
    let mut visible = content
        .iter()
        .filter(|part| !matches!(part, Content::Thinking(_)));
    for block in blocks {
        match block["type"].as_str() {
            Some("thinking") => {
                if block["thinking"].as_str().is_none()
                    || !block["signature"].as_str().is_some_and(|s| !s.is_empty())
                {
                    return Err(invalid("invalid Anthropic thinking replay"));
                }
            }
            Some("redacted_thinking") => {
                if !block["data"].as_str().is_some_and(|s| !s.is_empty()) {
                    return Err(invalid("invalid Anthropic redacted replay"));
                }
            }
            Some("text") => {
                let Some(Content::Text(expected)) = visible.next() else {
                    return Err(invalid("Anthropic replay differs from answer"));
                };
                if block["text"].as_str() != Some(expected) {
                    return Err(invalid("Anthropic replay differs from answer"));
                }
            }
            Some("tool_use") => {
                let Some(Content::ToolCall(expected)) = visible.next() else {
                    return Err(invalid("Anthropic replay differs from tool call"));
                };
                if block["id"].as_str() != Some(expected.id.as_str())
                    || block["name"].as_str() != Some(expected.name.as_str())
                    || block["input"] != expected.arguments
                {
                    return Err(invalid("Anthropic replay differs from tool call"));
                }
            }
            _ => return Err(invalid("unsupported Anthropic replay block")),
        }
    }
    if visible.next().is_some() {
        return Err(invalid("Anthropic replay omitted visible content"));
    }
    if blocks.iter().any(|block| {
        matches!(
            block["type"].as_str(),
            Some("thinking" | "redacted_thinking")
        )
    }) {
        let (instructions, tools) = digest_context.map_or(
            (request.instructions.as_deref(), request.tools.as_slice()),
            |context| (context.instructions.as_deref(), context.tools.as_slice()),
        );
        if anthropic_prefix_digest(instructions, tools, preceding)? != digest {
            return Err(error(
                ProviderErrorKind::ReplayContextChanged,
                "Anthropic signed thinking prefix changed",
            ));
        }
    }
    Ok(blocks.clone())
}

fn anthropic_tool_definition(tool: &ToolSpec) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
    })
}

fn anthropic_context_fields(
    instructions: Option<&str>,
    tools: &[ToolSpec],
) -> (Option<Value>, Option<Value>) {
    let system = instructions
        .filter(|instructions| !instructions.is_empty())
        .map(|instructions| json!(instructions));
    let tools = (!tools.is_empty())
        .then(|| Value::Array(tools.iter().map(anthropic_tool_definition).collect()));
    (system, tools)
}

fn anthropic_prefix_digest(
    instructions: Option<&str>,
    tools: &[ToolSpec],
    preceding: &[Value],
) -> Result<String, ProviderError> {
    let (system, tools) = anthropic_context_fields(instructions, tools);
    digest_anthropic_prefix(system.as_ref(), tools.as_ref(), preceding)
}

fn anthropic_tool_delta(previous: &[ToolSpec], next: &[ToolSpec]) -> Vec<Value> {
    let previous_by_name = previous
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect::<BTreeMap<_, _>>();
    let next_by_name = next
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect::<BTreeMap<_, _>>();
    let mut blocks = Vec::new();

    for name in previous_by_name.keys() {
        if !next_by_name.contains_key(name) {
            blocks.push(json!({
                "type": "tool_removal",
                "tool": {"type": "tool_reference", "name": name},
            }));
        }
    }
    for tool in next {
        if previous_by_name
            .get(tool.name.as_str())
            .is_none_or(|previous| **previous != *tool)
        {
            blocks.push(json!({
                "type": "tool_addition",
                "tool": {
                    "type": "tool_definition",
                    "definition": anthropic_tool_definition(tool),
                },
            }));
        }
    }
    blocks
}

fn anthropic_context_plan(
    request: &ModelRequest,
    inline_tools_supported: bool,
) -> Result<Option<AnthropicTimelinePlan<'_>>, ProviderError> {
    if !inline_tools_supported {
        return Ok(None);
    }
    let Some(timeline) = &request.context_timeline else {
        return Ok(None);
    };
    if timeline.initial.instructions != request.instructions
        || timeline
            .changes
            .iter()
            .any(|change| change.context.instructions != timeline.initial.instructions)
    {
        return Ok(None);
    }

    let mut previous = &timeline.initial;
    let mut changes = BTreeMap::<usize, Vec<Value>>::new();
    for change in &timeline.changes {
        let blocks = anthropic_tool_delta(&previous.tools, &change.context.tools);
        previous = &change.context;
        if blocks.is_empty() {
            continue;
        }
        let previous_message = request
            .messages
            .get(change.after_message.saturating_sub(1))
            .map(|message| message.role);
        let next_message = request
            .messages
            .get(change.after_message)
            .map(|message| message.role);
        if !matches!(previous_message, Some(Role::User | Role::Tool))
            || next_message.is_some_and(|role| role != Role::Assistant)
        {
            return Ok(None);
        }
        changes
            .entry(change.after_message)
            .or_default()
            .extend(blocks);
    }
    if changes.is_empty() {
        return Ok(None);
    }
    Ok(Some(AnthropicTimelinePlan {
        initial: &timeline.initial,
        changes,
    }))
}

pub(super) fn anthropic_body_prefix_digest(body: &Value) -> Result<String, ProviderError> {
    let messages = body["messages"]
        .as_array()
        .ok_or_else(|| invalid("invalid Anthropic request messages"))?;
    digest_anthropic_prefix(body.get("system"), body.get("tools"), messages)
}

fn digest_anthropic_prefix(
    system: Option<&Value>,
    tools: Option<&Value>,
    preceding: &[Value],
) -> Result<String, ProviderError> {
    // Match the previous sorted-key JSON encoding so persisted replay digests
    // stay valid, without deep-cloning the preceding wire transcript into a Value.
    #[derive(serde::Serialize)]
    struct Prefix<'a> {
        messages: &'a [Value],
        system: Option<&'a Value>,
        tools: Option<&'a Value>,
    }
    let encoded = serde_json::to_vec(&Prefix {
        messages: preceding,
        system,
        tools,
    })
    .map_err(|_| invalid("cannot encode Anthropic replay prefix"))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

#[cfg(test)]
pub(super) fn anthropic_body(
    request: &ModelRequest,
    native_api: bool,
) -> Result<Value, ProviderError> {
    anthropic_body_for_route(request, native_api, false)
}

pub(super) fn anthropic_body_for_route(
    request: &ModelRequest,
    native_api: bool,
    inline_tools_supported: bool,
) -> Result<Value, ProviderError> {
    validate_request(request)?;
    let mut body = control_fields(
        &request.route.effective,
        &request.controls,
        HttpWire::AnthropicMessages,
        native_api,
    )?;
    let context_plan = anthropic_context_plan(request, native_api && inline_tools_supported)?;
    let messages = transcript_messages(request, context_plan.as_ref())?;
    body["model"] = json!(request.route.effective.model);
    body["messages"] = Value::Array(messages);
    body["stream"] = json!(true);

    let (instructions, top_level_tools) = context_plan.as_ref().map_or(
        (request.instructions.as_deref(), request.tools.as_slice()),
        |plan| {
            (
                plan.initial.instructions.as_deref(),
                plan.initial.tools.as_slice(),
            )
        },
    );
    let (system, tools) = anthropic_context_fields(instructions, top_level_tools);
    if let Some(system) = system {
        body["system"] = system;
    }
    if let Some(tools) = tools {
        body["tools"] = tools;
    }
    // Inline additions can make tools available without any leading definitions.
    // Historical definitions still need restrictions even when later removed.
    if !request.tools.is_empty() || !top_level_tools.is_empty() {
        let mut choice = match &request.controls.tool_choice {
            ToolChoice::None => json!({"type":"none"}),
            ToolChoice::Auto => json!({"type":"auto"}),
            ToolChoice::Required => json!({"type":"any"}),
            ToolChoice::Named(name) => json!({"type":"tool","name":name}),
        };
        if request.controls.tool_choice != ToolChoice::None {
            choice["disable_parallel_tool_use"] = json!(!request.controls.parallel_tool_calls);
        }
        body["tool_choice"] = choice;
    }
    Ok(body)
}

pub(super) fn anthropic_body_uses_inline_tools(body: &Value) -> bool {
    body["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|message| message["role"] == "system")
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .any(|block| {
            matches!(
                block["type"].as_str(),
                Some("tool_addition" | "tool_removal")
            )
        })
}

pub(super) fn managed_anthropic_thinking(model: &str) -> bool {
    matches!(
        model,
        "claude-fable-5-1" | "claude-opus-5-5" | "claude-sonnet-5-5"
    )
}

fn transcript_messages(
    request: &ModelRequest,
    context_plan: Option<&AnthropicTimelinePlan<'_>>,
) -> Result<Vec<Value>, ProviderError> {
    let mut messages = Vec::new();
    let mut transcript = Transcript::default();
    let mut ids = ToolIds::default();
    for (message_index, message) in request.messages.iter().enumerate() {
        let replay = provider_replay(message, &request.route.effective.provider)?;
        if replay.is_some_and(|replay| replay.kind != ANTHROPIC_CONTENT_REPLAY) {
            return Err(incompatible_replay());
        }
        transcript.begin(message)?;
        let mut blocks = Vec::new();
        for item in &message.content {
            match transcript.part(message.role, item)? {
                Part::Thinking => {}
                Part::Text(part) => {
                    if !part.is_empty() {
                        blocks.push(json!({"type":"text","text":part}));
                    }
                }
                Part::Image(image) => {
                    blocks.push(json!({"type":"image","source":{
                        "type":"base64","media_type":image.mime_type().as_str(),"data":image.data()
                    }}));
                }
                Part::ToolCall { call, id } => {
                    *id = ids.for_call(&call.id, replay.is_some(), "Anthropic")?;
                    blocks.push(
                        json!({"type":"tool_use","id":id,"name":call.name,"input":call.arguments}),
                    );
                }
                Part::ToolResult { result, id } => {
                    let mut content = vec![json!({"type":"text","text":result.result.to_string()})];
                    for image in &result.images {
                        content.push(json!({"type":"image","source":{
                            "type":"base64","media_type":image.mime_type().as_str(),"data":image.data()
                        }}));
                    }
                    blocks.push(json!({"type":"tool_result","tool_use_id":id,"content":content,"is_error":result.is_error}));
                }
            }
        }
        if let Some(replay) = replay {
            blocks = validated_anthropic_replay(
                &replay.data,
                &message.content,
                request,
                &messages,
                context_plan.map(|plan| plan.initial),
            )?;
        }
        if blocks.is_empty() {
            return Err(invalid("empty Anthropic message"));
        }
        let role = if message.role == Role::Assistant {
            "assistant"
        } else {
            "user"
        };
        if let Some(previous) = messages
            .last_mut()
            .filter(|m: &&mut Value| m["role"] == role)
        {
            previous["content"]
                .as_array_mut()
                .expect("constructed content array")
                .extend(blocks);
        } else {
            messages.push(json!({"role":role,"content":blocks}));
        }
        if let Some(change_blocks) = context_plan
            .and_then(|plan| plan.changes.get(&(message_index + 1)))
            .filter(|blocks| !blocks.is_empty())
        {
            messages.push(json!({"role":"system","content":change_blocks}));
        }
    }
    transcript.finish()?;
    if messages[0]["role"] != "user"
        || messages
            .last()
            .is_some_and(|message| message["role"] == "assistant")
    {
        return Err(unsupported("Anthropic assistant prefill is unsupported"));
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_prefix_fingerprints_preserve_persisted_wire_bytes() {
        // Independently checked with Python hashlib over canonical compact JSON.
        for (body, expected) in [
            (
                json!({
                    "messages":[{"role":"user","content":[{"type":"text","text":"hi α"}]}],
                    "system":"rules",
                    "tools":[{"name":"inspect","description":"read only","input_schema":{"type":"object"}}]
                }),
                "764b62dcd7b0a47807f736925bee00ad0772424c6287989752e42bb7d4ad7c77",
            ),
            (
                json!({"messages":[],"system":null,"tools":null}),
                "d6fd557e955c6e0dd86a9ff861a2f8f8d216d145f84c791fe26aa25adf901029",
            ),
        ] {
            assert_eq!(anthropic_body_prefix_digest(&body).unwrap(), expected);
        }
    }
}
