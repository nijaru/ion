//! Native Anthropic requests, signed prefixes and inline context changes.
use std::io::{self, Write};

use super::tool_ids::ToolIds;
use super::transcript::{Part, Transcript, incompatible_replay, provider_replay};
use super::*;
use ion_ai::{ModelContextEncoding, ModelContextState};
use sha2::{Digest, Sha256};

pub(super) struct AnthropicRequestBody {
    pub(super) body: Value,
    pub(super) prefix_digest: String,
    inline_tools: bool,
}

impl AnthropicRequestBody {
    pub(super) fn beta_header(&self) -> Option<String> {
        // The binding beta follows the compiled adaptive block. In particular,
        // a model that can turn thinking off must not send binding under Off.
        let binding = self.body["thinking"].get("block_binding").is_some();
        let inline_tools = self.inline_tools;
        (binding || inline_tools).then(|| {
            [
                binding.then_some("thinking-binding-controls-2026-08-01"),
                inline_tools.then_some("inline-tools-2026-09-15"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(",")
        })
    }
}

struct AnthropicTimelinePlan<'a> {
    initial: &'a ModelContextState,
    changes: BTreeMap<usize, Vec<Value>>,
}

struct Sha256Writer<'a>(&'a mut Sha256);

impl Write for Sha256Writer<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn json_encoded_len(value: &impl serde::Serialize) -> Result<usize, ProviderError> {
    struct Counter(usize);
    impl Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| io::Error::other("encoded JSON size overflow"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| invalid("Anthropic context measurement failed"))?;
    Ok(counter.0)
}

/// Incremental SHA-256 of canonical compact `{"messages":[...],"system":...,"tools":...}`.
///
/// Only messages guaranteed not to merge with a later same-role message are
/// committed into `hasher`; the final trailing message (at most one) is fed
/// into a cloned hasher on each digest check.
struct AnthropicPrefixHasher {
    hasher: Sha256,
    hashed_messages: usize,
    suffix: Vec<u8>,
}

impl AnthropicPrefixHasher {
    fn new(system: Option<&Value>, tools: Option<&Value>) -> Result<Self, ProviderError> {
        #[derive(serde::Serialize)]
        struct Suffix<'a> {
            system: Option<&'a Value>,
            tools: Option<&'a Value>,
        }
        let encoded_suffix = serde_json::to_vec(&Suffix { system, tools })
            .map_err(|_| invalid("cannot encode Anthropic replay prefix"))?;
        debug_assert_eq!(encoded_suffix.first(), Some(&b'{'));
        let mut suffix = Vec::with_capacity(encoded_suffix.len() + 1);
        suffix.extend_from_slice(b"],");
        suffix.extend_from_slice(&encoded_suffix[1..]);

        let mut hasher = Sha256::new();
        hasher.update(b"{\"messages\":[");
        Ok(Self {
            hasher,
            hashed_messages: 0,
            suffix,
        })
    }

    fn feed_message(
        hasher: &mut Sha256,
        index: usize,
        message: &Value,
    ) -> Result<(), ProviderError> {
        if index > 0 {
            hasher.update(b",");
        }
        serde_json::to_writer(Sha256Writer(hasher), message)
            .map_err(|_| invalid("cannot encode Anthropic replay prefix"))
    }

    fn commit(&mut self, messages: &[Value], up_to: usize) -> Result<(), ProviderError> {
        while self.hashed_messages < up_to {
            Self::feed_message(
                &mut self.hasher,
                self.hashed_messages,
                &messages[self.hashed_messages],
            )?;
            self.hashed_messages += 1;
        }
        Ok(())
    }

    fn digest(&self, messages: &[Value]) -> Result<String, ProviderError> {
        let mut hasher = self.hasher.clone();
        for (offset, message) in messages[self.hashed_messages..].iter().enumerate() {
            Self::feed_message(&mut hasher, self.hashed_messages + offset, message)?;
        }
        hasher.update(&self.suffix);
        Ok(format!("{:x}", hasher.finalize()))
    }
}

/// Replay the original assistant block array only when it still projects to
/// the committed provider-neutral message. A changed visible tool call or
/// answer cannot inherit an old signature.
fn validated_anthropic_replay(
    data: &Value,
    content: &[Content],
    preceding: &[Value],
    prefix_hasher: &AnthropicPrefixHasher,
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
    }) && prefix_hasher.digest(preceding)? != digest
    {
        return Err(error(
            ProviderErrorKind::ReplayContextChanged,
            "Anthropic signed thinking prefix changed",
        ));
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

fn anthropic_tool_choice_field(
    controls: &ion_ai::GenerationControls,
    has_tools: bool,
) -> Option<Value> {
    if !has_tools {
        return None;
    }
    let mut choice = match &controls.tool_choice {
        ToolChoice::None => json!({"type":"none"}),
        ToolChoice::Auto => json!({"type":"auto"}),
        ToolChoice::Required => json!({"type":"any"}),
        ToolChoice::Named(name) => json!({"type":"tool","name":name}),
    };
    if controls.tool_choice != ToolChoice::None {
        choice["disable_parallel_tool_use"] = json!(!controls.parallel_tool_calls);
    }
    Some(choice)
}

fn anthropic_request_context_fields(
    context: ModelContextEncoding<'_>,
    instructions: Option<&str>,
    tools: &[ToolSpec],
) -> serde_json::Map<String, Value> {
    let (system, definitions) = anthropic_context_fields(instructions, tools);
    let mut fields = serde_json::Map::new();
    if let Some(system) = system {
        fields.insert("system".into(), system);
    }
    if let Some(definitions) = definitions {
        fields.insert("tools".into(), definitions);
    }
    // Historical leading tools still need restrictions after their removal.
    if let Some(choice) = anthropic_tool_choice_field(
        context.controls,
        !context.tools.is_empty() || !tools.is_empty(),
    ) {
        fields.insert("tool_choice".into(), choice);
    }
    fields
}

fn anthropic_context_change(blocks: &[Value]) -> Value {
    json!({"role":"system","content":blocks})
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

fn anthropic_context_plan<'a>(
    context: ModelContextEncoding<'a>,
    inline_tools_supported: bool,
) -> Result<Option<AnthropicTimelinePlan<'a>>, ProviderError> {
    if !inline_tools_supported {
        return Ok(None);
    }
    let Some(timeline) = context.context_timeline else {
        return Ok(None);
    };
    if timeline.initial.instructions.as_deref() != context.instructions
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
        let previous_message = context
            .message_roles
            .get(change.after_message.saturating_sub(1));
        let next_message = context.message_roles.get(change.after_message);
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

fn anthropic_context_fields_bytes(
    fields: serde_json::Map<String, Value>,
) -> Result<usize, ProviderError> {
    if fields.is_empty() {
        return Ok(0);
    }
    // Drop the braces, but include the separator from the other body fields.
    Ok(json_encoded_len(&fields)? - 1)
}

pub(super) fn anthropic_context_encoding_overhead(
    context: ModelContextEncoding<'_>,
    native_api: bool,
    inline_tools_supported: bool,
) -> Result<usize, ProviderError> {
    let Some(plan) = anthropic_context_plan(context, native_api && inline_tools_supported)? else {
        return Ok(0);
    };
    // Only compile context fields: validating signed transcript replay here
    // would move ReplayContextChanged outside Core's issuance recovery path.
    let initial = anthropic_context_fields_bytes(anthropic_request_context_fields(
        context,
        plan.initial.instructions.as_deref(),
        &plan.initial.tools,
    ))?;
    let latest = anthropic_context_fields_bytes(anthropic_request_context_fields(
        context,
        context.instructions,
        context.tools,
    ))?;
    let mut bytes = initial;
    for blocks in plan.changes.values() {
        let message = anthropic_context_change(blocks);
        let length = json_encoded_len(&message)?;
        // Each emitted message also adds a separator to the nonempty transcript.
        bytes = bytes.saturating_add(length).saturating_add(1);
    }
    Ok(bytes.saturating_sub(latest))
}

#[cfg(test)]
pub(super) fn anthropic_body_prefix_digest(body: &Value) -> Result<String, ProviderError> {
    let messages = body["messages"]
        .as_array()
        .ok_or_else(|| invalid("invalid Anthropic request messages"))?;
    let hasher = AnthropicPrefixHasher::new(body.get("system"), body.get("tools"))?;
    hasher.digest(messages)
}

#[cfg(test)]
pub(super) fn anthropic_body(
    request: &ModelRequest,
    native_api: bool,
) -> Result<Value, ProviderError> {
    anthropic_body_for_route(request, native_api, false)
}

#[cfg(test)]
pub(super) fn anthropic_body_for_route(
    request: &ModelRequest,
    native_api: bool,
    inline_tools_supported: bool,
) -> Result<Value, ProviderError> {
    anthropic_request_for_route(request, native_api, inline_tools_supported)
        .map(|prepared| prepared.body)
}

pub(super) fn anthropic_request_for_route(
    request: &ModelRequest,
    native_api: bool,
    inline_tools_supported: bool,
) -> Result<AnthropicRequestBody, ProviderError> {
    validate_request(request)?;
    let mut body = control_fields(
        &request.route.effective,
        &request.controls,
        HttpWire::AnthropicMessages,
        native_api,
    )?;
    let context_plan = anthropic_context_plan(
        ModelContextEncoding::from_request(request),
        native_api && inline_tools_supported,
    )?;
    let inline_tools = context_plan.is_some();
    let (instructions, top_level_tools) = context_plan.as_ref().map_or(
        (request.instructions.as_deref(), request.tools.as_slice()),
        |plan| {
            (
                plan.initial.instructions.as_deref(),
                plan.initial.tools.as_slice(),
            )
        },
    );
    let (system, definitions) = anthropic_context_fields(instructions, top_level_tools);
    let mut prefix_hasher = AnthropicPrefixHasher::new(system.as_ref(), definitions.as_ref())?;
    let messages = transcript_messages(request, context_plan.as_ref(), &mut prefix_hasher)?;
    let prefix_digest = prefix_hasher.digest(&messages)?;
    body["model"] = json!(request.route.effective.model);
    body["messages"] = Value::Array(messages);
    body["stream"] = json!(true);
    if let Some(system) = system {
        body["system"] = system;
    }
    if let Some(definitions) = definitions {
        body["tools"] = definitions;
    }
    if let Some(choice) = anthropic_tool_choice_field(
        &request.controls,
        !request.tools.is_empty() || !top_level_tools.is_empty(),
    ) {
        body["tool_choice"] = choice;
    }
    Ok(AnthropicRequestBody {
        body,
        prefix_digest,
        inline_tools,
    })
}

#[cfg(test)]
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

fn transcript_messages(
    request: &ModelRequest,
    context_plan: Option<&AnthropicTimelinePlan<'_>>,
    prefix_hasher: &mut AnthropicPrefixHasher,
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
                    if replay.is_none() && !part.is_empty() {
                        blocks.push(json!({"type":"text","text":part}));
                    }
                }
                Part::Image(image) => {
                    if replay.is_none() {
                        blocks.push(json!({"type":"image","source":{
                            "type":"base64","media_type":image.mime_type().as_str(),"data":image.data()
                        }}));
                    }
                }
                Part::ToolCall { call, id } => {
                    *id = ids.for_call(&call.id, replay.is_some(), "Anthropic")?;
                    if replay.is_none() {
                        blocks.push(
                            json!({"type":"tool_use","id":id,"name":call.name,"input":call.arguments}),
                        );
                    }
                }
                Part::ToolResult { result, id } => {
                    if replay.is_none() {
                        let mut content =
                            vec![json!({"type":"text","text":result.result.to_string()})];
                        for image in &result.images {
                            content.push(json!({"type":"image","source":{
                                "type":"base64","media_type":image.mime_type().as_str(),"data":image.data()
                            }}));
                        }
                        blocks.push(json!({"type":"tool_result","tool_use_id":id,"content":content,"is_error":result.is_error}));
                    }
                }
            }
        }
        if let Some(replay) = replay {
            blocks = validated_anthropic_replay(
                &replay.data,
                &message.content,
                &messages,
                prefix_hasher,
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
            prefix_hasher.commit(&messages, messages.len())?;
            messages.push(json!({"role":role,"content":blocks}));
        }
        if let Some(change_blocks) = context_plan
            .and_then(|plan| plan.changes.get(&(message_index + 1)))
            .filter(|blocks| !blocks.is_empty())
        {
            prefix_hasher.commit(&messages, messages.len())?;
            messages.push(anthropic_context_change(change_blocks));
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

    #[test]
    fn incremental_prefix_hasher_matches_merged_roles_and_rejects_tampered_prefix() {
        let model = ion_ai::ModelRef {
            provider: "anthropic".into(),
            model: "claude-opus-5-5".into(),
        };
        let tool = ToolSpec {
            name: "read".into(),
            description: "read a file".into(),
            input_schema: json!({"type":"object"}),
        };
        let mut request = ModelRequest {
            route: ModelRoute::direct(model, ModelRouteReason::UserRequest),
            provider_session_id: None,
            instructions: Some("rules".into()),
            messages: vec![
                Message::user_input("first".into(), []),
                Message::user_input("merged user".into(), []),
            ],
            tools: vec![tool],
            context_timeline: None,
            prompt_cache: PromptCacheIntent::Reusable,
            controls: ion_ai::GenerationControls {
                max_output_tokens: 1024,
                temperature: None,
                top_p: None,
                reasoning: Reasoning::ProviderDefault,
                tool_choice: ToolChoice::Auto,
                parallel_tool_calls: false,
            },
        };
        let first_prepared = anthropic_request_for_route(&request, true, true).unwrap();
        assert_eq!(
            first_prepared.prefix_digest,
            anthropic_body_prefix_digest(&first_prepared.body).unwrap()
        );
        assert!(!first_prepared.inline_tools);
        request.messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::Text("step 1".into())],
            provider_replay: Some(
                ProviderReplay::new(
                    "anthropic",
                    ANTHROPIC_CONTENT_REPLAY,
                    json!({
                        "blocks": [
                            {"type":"thinking","thinking":"t1","signature":"sig1"},
                            {"type":"text","text":"step 1"}
                        ],
                        "prefix_sha256": first_prepared.prefix_digest,
                    }),
                )
                .with_prefix_binding(true),
            ),
        });
        request
            .messages
            .push(Message::user_input("second".into(), []));
        let second_prepared = anthropic_request_for_route(&request, true, true).unwrap();
        assert_eq!(
            second_prepared.prefix_digest,
            anthropic_body_prefix_digest(&second_prepared.body).unwrap()
        );
        request.messages.push(Message {
            role: Role::Assistant,
            content: vec![Content::Text("step 2".into())],
            provider_replay: Some(
                ProviderReplay::new(
                    "anthropic",
                    ANTHROPIC_CONTENT_REPLAY,
                    json!({
                        "blocks": [
                            {"type":"thinking","thinking":"t2","signature":"sig2"},
                            {"type":"text","text":"step 2"}
                        ],
                        "prefix_sha256": second_prepared.prefix_digest,
                    }),
                )
                .with_prefix_binding(true),
            ),
        });
        request
            .messages
            .push(Message::user_input("third".into(), []));
        let third_prepared = anthropic_request_for_route(&request, true, true).unwrap();
        assert_eq!(
            third_prepared.prefix_digest,
            anthropic_body_prefix_digest(&third_prepared.body).unwrap()
        );
        assert_eq!(third_prepared.body["messages"].as_array().unwrap().len(), 5);

        // Mutating an earlier merged user message must invalidate the signed prefix.
        request.messages[1] = Message::user_input("tampered".into(), []);
        assert_eq!(
            anthropic_request_for_route(&request, true, true)
                .err()
                .expect("tampered prefix must fail")
                .kind,
            ProviderErrorKind::ReplayContextChanged
        );
    }
}
