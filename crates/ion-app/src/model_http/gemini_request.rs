//! Stateless GenerateContent requests. Keep model parts exact and group each
//! batch of tool observations in the order of the corresponding native calls.
use super::gemini::{GEMINI_REPLAY, project_parts};
use super::transcript::{Part, Transcript, incompatible_replay, provider_replay};
use super::*;
use ion_ai::ModelRef;
use std::borrow::Cow;

use crate::catalog::GEMINI_HYDROGEN;

pub(super) fn resolve_base(mut url: Url) -> Result<Url, ProviderError> {
    let path = url.path().trim_end_matches('/');
    if path.contains(':') || path.split('/').any(|segment| segment == "models") {
        return Err(invalid(
            "Gemini endpoint must be an API base, not a model method URL",
        ));
    }
    let versioned = path
        .rsplit('/')
        .next()
        .is_some_and(|segment| matches!(segment, "v1" | "v1beta" | "v1alpha"));
    let trailing = url.path().len().saturating_sub(path.len());
    let mut segments = url
        .path_segments_mut()
        .map_err(|_| invalid("invalid Gemini base path"))?;
    for _ in 0..trailing {
        segments.pop_if_empty();
    }
    if !versioned {
        segments.push("v1beta");
    }
    drop(segments);
    Ok(url)
}

fn validate_model(model: &str) -> Result<(), ProviderError> {
    if model.is_empty()
        || model.len() > 128
        || matches!(model, "." | "..")
        || !model
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    {
        return Err(invalid("invalid Gemini model path segment"));
    }
    Ok(())
}

pub(super) fn stream_url(base: &Url, model: &str) -> Result<Url, ProviderError> {
    validate_model(model)?;
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| invalid("invalid Gemini base path"))?
        .pop_if_empty()
        .push("models")
        .push(&format!("{model}:streamGenerateContent"));
    url.query_pairs_mut().append_pair("alt", "sse");
    Ok(url)
}

pub(super) fn gemini_controls(
    model: &ModelRef,
    controls: &ion_ai::GenerationControls,
) -> Result<Value, ProviderError> {
    validate_model(&model.model)?;
    if controls.temperature.is_some() || controls.top_p.is_some() {
        return Err(unsupported(
            "explicit sampling is not supported by the Gemini adapter",
        ));
    }
    let mut config = json!({"maxOutputTokens":controls.max_output_tokens, "candidateCount":1});
    let level = match controls.reasoning {
        Reasoning::ProviderDefault => return Ok(config),
        Reasoning::Off | Reasoning::BudgetTokens(_) | Reasoning::Max => {
            return Err(unsupported(
                "Gemini GenerateContent does not support this reasoning setting",
            ));
        }
        _ if model.model != GEMINI_HYDROGEN => {
            return Err(unsupported(
                "explicit effort is not verified for this Gemini model",
            ));
        }
        Reasoning::Low => "LOW",
        Reasoning::Medium => "MEDIUM",
        Reasoning::High => "HIGH",
        Reasoning::XHigh => "XHIGH",
    };
    config["thinkingConfig"] = json!({"thinkingLevel":level,"includeThoughts":true});
    Ok(config)
}

fn image(image: &ion_ai::ImageContent) -> Value {
    json!({"inlineData":{"mimeType":image.mime_type().as_str(),"data":image.data()}})
}

fn tool_response(result: &ion_ai::ToolResult, native_id: &str) -> Value {
    let mut envelope = if result.is_error {
        json!({"error":result.result})
    } else {
        json!({"output":result.result})
    };
    let mut parts = Vec::new();
    if !result.images.is_empty() {
        let mut refs = Vec::new();
        for (index, value) in result.images.iter().enumerate() {
            let mut part = image(value);
            let name = format!("ion-image-{index}");
            part["inlineData"]["displayName"] = json!(name);
            refs.push(json!({"$ref":name}));
            parts.push(part);
        }
        envelope["images"] = json!(refs);
    }
    let mut response = json!({"name":result.name,"response":envelope});
    if !native_id.is_empty() {
        response["id"] = json!(native_id);
    }
    if !parts.is_empty() {
        response["parts"] = json!(parts);
    }
    json!({"functionResponse":response})
}

fn replay_parts<'a>(
    message: &'a Message,
    model: &ModelRef,
) -> Result<Option<&'a [Value]>, ProviderError> {
    let Some(replay) = provider_replay(message, &model.provider)? else {
        return Ok(None);
    };
    if replay.kind != GEMINI_REPLAY || replay.data["model"].as_str() != Some(model.model.as_str()) {
        return Err(incompatible_replay());
    }
    let parts = replay.data["parts"]
        .as_array()
        .filter(|parts| !parts.is_empty())
        .ok_or_else(|| invalid("invalid Gemini replay parts"))?;
    // Core removes human-readable thinking from request context. The native
    // parts still carry the exact summaries and signatures on the same route.
    let projected = project_parts(parts)?;
    let visible = |part: &&Content| !matches!(part, Content::Thinking(_));
    if projected
        .iter()
        .filter(visible)
        .ne(message.content.iter().filter(visible))
    {
        return Err(invalid("Gemini native parts differ from committed content"));
    }
    Ok(Some(parts))
}

fn flush_tools<'a>(
    contents: &mut Vec<Value>,
    ordered: &mut Vec<&'a str>,
    responses: &mut BTreeMap<&'a str, Value>,
) -> Result<(), ProviderError> {
    if !ordered.is_empty() {
        let parts = ordered
            .drain(..)
            .map(|id| {
                responses
                    .remove(id)
                    .ok_or_else(|| invalid("missing Gemini function response"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        contents.push(json!({"role":"user","parts":parts}));
    }
    Ok(())
}

pub(super) fn gemini_body(request: &ModelRequest) -> Result<Value, ProviderError> {
    validate_request(request)?;
    let model = &request.route.effective;
    let mut transcript = Transcript::default();
    let mut contents = Vec::new();
    let mut order = Vec::new();
    let mut responses = BTreeMap::new();
    for message in &request.messages {
        transcript.begin(message)?;
        if message.role != Role::Tool {
            flush_tools(&mut contents, &mut order, &mut responses)?;
        }
        let replay = replay_parts(message, model)?;
        let mut native_calls = replay
            .into_iter()
            .flatten()
            .filter_map(|part| part.get("functionCall"));
        let mut parts = Vec::new();
        for part in &message.content {
            match transcript.part(message.role, part)? {
                Part::Thinking => {}
                Part::Text(text) if replay.is_none() => parts.push(json!({"text":text})),
                Part::Text(_) => {}
                Part::Image(value) => parts.push(image(value)),
                Part::ToolCall { call, id } => {
                    let native = native_calls.next().ok_or_else(|| unsupported(
                        "Gemini needs native signed function calls; explicitly compact or start a new Session",
                    ))?;
                    // Empty is only a wire mapping: the local Transcript pairs
                    // this result using the nonempty, stable part coordinate.
                    *id = Cow::Borrowed(native["id"].as_str().unwrap_or(""));
                    order.push(call.id.as_str());
                }
                Part::ToolResult { result, id } => {
                    responses.insert(result.call_id.as_str(), tool_response(result, &id));
                }
            }
        }
        if message.role != Role::Tool {
            if let Some(replay) = replay {
                parts.extend_from_slice(replay);
            }
            if parts.is_empty() {
                return Err(invalid("empty Gemini message"));
            }
            let role = if message.role == Role::Assistant {
                "model"
            } else {
                "user"
            };
            contents.push(json!({"role":role,"parts":parts}));
        }
    }
    transcript.finish()?;
    flush_tools(&mut contents, &mut order, &mut responses)?;
    let mut body = json!({"contents":contents,"store":false,
        "generationConfig":control_fields(model, &request.controls, HttpWire::GeminiGenerateContent, false)?});
    if let Some(instructions) = &request.instructions {
        body["systemInstruction"] = json!({"parts":[{"text":instructions}]});
    }
    if !request.tools.is_empty() && request.controls.tool_choice.uses_tools() {
        let declarations = request
            .tools
            .iter()
            .map(|tool| {
                json!({"name":tool.name,
            "description":tool.description,"parametersJsonSchema":tool.input_schema})
            })
            .collect::<Vec<_>>();
        body["tools"] = json!([{"functionDeclarations":declarations}]);
        let call = match &request.controls.tool_choice {
            ToolChoice::Auto => json!({"mode":"AUTO"}),
            ToolChoice::Required => json!({"mode":"ANY"}),
            ToolChoice::Named(name) => json!({"mode":"ANY","allowedFunctionNames":[name]}),
            ToolChoice::None => unreachable!("no catalog for None"),
        };
        body["toolConfig"] = json!({"functionCallingConfig":call});
    }
    Ok(body)
}
