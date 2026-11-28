//! Gemini streaming candidates. Signed parts are archived as received, not as
//! the coalesced text or tool projection used by the client.
use super::*;

pub(super) const GEMINI_REPLAY: &str = "gemini_generate_content_parts";
const MAX_PARTS: usize = 65_536;

pub(super) enum GeminiPart<'a> {
    Text {
        text: &'a str,
        thought: bool,
    },
    Call {
        name: &'a str,
        args: Option<&'a Value>,
        id: Option<&'a str>,
    },
    Opaque,
}

pub(super) fn gemini_part(part: &Value) -> Result<GeminiPart<'_>, ProviderError> {
    let fields = part
        .as_object()
        .ok_or_else(|| invalid("Gemini part is not an object"))?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "text" | "thought" | "thoughtSignature" | "functionCall"
        )
    }) {
        return Err(unsupported("Gemini emitted an unsupported part type"));
    }
    let signed = match fields.get("thoughtSignature") {
        None => false,
        Some(value)
            if value.as_str().is_some_and(|sig| {
                !sig.is_empty()
                    && sig
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b))
            }) =>
        {
            true
        }
        Some(_) => return Err(invalid("invalid Gemini thought signature")),
    };
    let thought = fields
        .get("thought")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| invalid("invalid Gemini thought flag"))
        })
        .transpose()?
        .unwrap_or(false);
    match (fields.get("text"), fields.get("functionCall")) {
        (Some(text), None) => Ok(GeminiPart::Text {
            text: text
                .as_str()
                .ok_or_else(|| invalid("invalid Gemini text part"))?,
            thought,
        }),
        (None, Some(call)) if !thought => {
            let call = call
                .as_object()
                .ok_or_else(|| invalid("invalid Gemini function call"))?;
            if call
                .keys()
                .any(|key| !matches!(key.as_str(), "name" | "args" | "id"))
            {
                return Err(unsupported("partial or unsupported Gemini function call"));
            }
            let name = call
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| valid_identifier(name))
                .ok_or_else(|| invalid("invalid Gemini function name"))?;
            let args = call.get("args");
            if args.is_some_and(|value| !value.is_object()) {
                return Err(invalid("Gemini arguments must be an object"));
            }
            let id = call
                .get("id")
                .map(|value| {
                    value
                        .as_str()
                        .filter(|id| {
                            !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
                        })
                        .ok_or_else(|| invalid("invalid Gemini function call ID"))
                })
                .transpose()?;
            Ok(GeminiPart::Call { name, args, id })
        }
        (None, None) if signed => Ok(GeminiPart::Opaque),
        _ => Err(invalid("invalid Gemini part payload")),
    }
}

/// Validate a full assistant response before it can authorize any effects.
/// Internal call IDs are part coordinates; native IDs (including absence)
/// remain untouched in replay and in the wire result pairing.
pub(super) fn project_parts(parts: &[Value]) -> Result<Vec<Content>, ProviderError> {
    if parts.len() > MAX_PARTS {
        return Err(invalid("too many Gemini parts"));
    }
    let mut content = Vec::new();
    let mut ids = BTreeSet::new();
    let mut first_call = true;
    for (index, part) in parts.iter().enumerate() {
        match gemini_part(part)? {
            GeminiPart::Text { text, thought } if !text.is_empty() => {
                match (thought, content.last_mut()) {
                    (true, Some(Content::Thinking(existing)))
                    | (false, Some(Content::Text(existing))) => existing.push_str(text),
                    (true, _) => content.push(Content::Thinking(text.into())),
                    (false, _) => content.push(Content::Text(text.into())),
                }
            }
            GeminiPart::Call { name, args, id } => {
                // Gemini requires a thought signature on the first call in a
                // parallel batch. Never synthesize one for an unsigned call.
                if std::mem::take(&mut first_call) && part.get("thoughtSignature").is_none() {
                    return Err(invalid(
                        "Gemini first function call is missing its thought signature",
                    ));
                }
                if id.is_some_and(|id| !ids.insert(id)) {
                    return Err(invalid("duplicate Gemini function call ID"));
                }
                content.push(Content::ToolCall(ToolCall {
                    id: format!("gemini-{index}"),
                    name: name.into(),
                    arguments: args.cloned().unwrap_or_else(|| json!({})),
                    raw_arguments: None,
                }));
            }
            _ => {}
        }
    }
    if content.is_empty() {
        // Some responses contain opaque reasoning but no readable summary.
        content.push(Content::Text(String::new()));
    }
    Ok(content)
}

#[derive(Default)]
struct GeminiUsage {
    input: Option<u64>,
    cache: Option<u64>,
    candidates: Option<u64>,
    thoughts: Option<u64>,
}

impl GeminiUsage {
    fn update(&mut self, value: &Value) -> Result<Usage, ProviderError> {
        let fields = value
            .as_object()
            .ok_or_else(|| invalid("Gemini usage is not an object"))?;
        for (name, field) in [
            ("promptTokenCount", &mut self.input),
            ("cachedContentTokenCount", &mut self.cache),
            ("candidatesTokenCount", &mut self.candidates),
            ("thoughtsTokenCount", &mut self.thoughts),
        ] {
            if let Some(value) = fields.get(name) {
                *field = Some(
                    value
                        .as_u64()
                        .ok_or_else(|| invalid("invalid Gemini usage count"))?,
                );
            }
        }
        self.value()
    }

    fn value(&self) -> Result<Usage, ProviderError> {
        let output = self
            .candidates
            .map(|tokens| {
                tokens
                    .checked_add(self.thoughts.unwrap_or(0))
                    .ok_or_else(|| invalid("Gemini output usage overflow"))
            })
            .transpose()?;
        Ok(Usage {
            input_tokens: self.input,
            output_tokens: output,
            cache_read_input_tokens: self.cache,
            cache_write_input_tokens: None,
        })
    }
}

#[derive(Default)]
pub(super) struct GeminiState {
    parts: Vec<Value>,
    model: Option<String>,
    usage: GeminiUsage,
    last_thought: Option<bool>,
    blocks: [usize; 2],
    pub(super) budget: ResponseBudget,
}

impl GeminiState {
    pub(super) fn accept(
        &mut self,
        value: &Value,
        output: &OutputContract,
    ) -> Result<Vec<ModelStreamEvent>, ProviderError> {
        if !value.is_object() || value.get("error").is_some() {
            return Err(invalid("Gemini returned an invalid streaming response"));
        }
        let mut events = Vec::new();
        if let Some(usage) = value.get("usageMetadata") {
            events.push(ModelStreamEvent::Usage(self.usage.update(usage)?));
        }
        if let Some(version) = value.get("modelVersion") {
            let version = version
                .as_str()
                .filter(|text| !text.is_empty() && text.len() <= 256)
                .ok_or_else(|| invalid("invalid Gemini model version"))?;
            match &self.model {
                Some(prior) if prior != version => {
                    return Err(invalid("Gemini model version changed while streaming"));
                }
                None => {
                    self.budget.retain(version.len())?;
                    self.model = Some(version.into());
                }
                _ => {}
            }
        }
        if let Some(reason) = value.pointer("/promptFeedback/blockReason") {
            let reason = reason
                .as_str()
                .ok_or_else(|| invalid("invalid Gemini prompt feedback"))?;
            if !matches!(reason, "" | "BLOCK_REASON_UNSPECIFIED") {
                return Err(invalid("Gemini blocked the prompt"));
            }
        }
        let Some(candidates) = value.get("candidates") else {
            return Ok(events);
        };
        let candidates = candidates
            .as_array()
            .ok_or_else(|| invalid("invalid Gemini candidates"))?;
        let candidate = match candidates.as_slice() {
            [] => return Ok(events),
            [candidate]
                if candidate.is_object()
                    && candidate
                        .get("index")
                        .is_none_or(|index| index.as_u64() == Some(0)) =>
            {
                candidate
            }
            _ => {
                return Err(invalid(
                    "Gemini returned more than one candidate or an invalid candidate index",
                ));
            }
        };
        if let Some(content) = candidate.get("content") {
            if !content.is_object()
                || content
                    .get("role")
                    .is_some_and(|role| role.as_str() != Some("model"))
            {
                return Err(invalid("invalid Gemini candidate content"));
            }
            if let Some(parts) = content.get("parts") {
                let parts = parts
                    .as_array()
                    .ok_or_else(|| invalid("invalid Gemini candidate parts"))?;
                for part in parts {
                    self.accept_part(part, &mut events)?;
                }
            }
        }
        let Some(reason) = candidate.get("finishReason") else {
            return Ok(events);
        };
        let termination = match reason.as_str() {
            Some("" | "FINISH_REASON_UNSPECIFIED") => return Ok(events),
            Some("STOP") => ResponseTermination::Completed,
            Some("MAX_TOKENS") => {
                ResponseTermination::Incomplete(IncompleteReason::MaxOutputTokens)
            }
            Some("SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII") => {
                ResponseTermination::Incomplete(IncompleteReason::ContentFilter)
            }
            Some("CONTINUATION") => {
                return Err(unsupported(
                    "Gemini long-decode continuation is not supported",
                ));
            }
            _ => return Err(invalid("Gemini returned a failed or unknown finish reason")),
        };
        if termination == ResponseTermination::Completed && self.parts.is_empty() {
            return Err(invalid("Gemini completed without any parts"));
        }
        let content = project_parts(&self.parts)?;
        if termination != ResponseTermination::Completed
            && content
                .iter()
                .any(|part| matches!(part, Content::ToolCall(_)))
        {
            return Err(invalid("Gemini stopped before its tool calls could settle"));
        }
        let parts = std::mem::take(&mut self.parts);
        let replay = (!parts.is_empty()).then(|| {
            ProviderReplay::new(
                &output.model.provider,
                GEMINI_REPLAY,
                json!({"model":output.model.model,"parts":parts}),
            )
        });
        let response = ModelResponse {
            message: Message {
                role: Role::Assistant,
                content,
                provider_replay: replay,
            },
            usage: self.usage.value()?,
            termination,
            returned_model: self.model.take(),
        };
        if response.is_complete() {
            validate_output(output, &response)?;
        }
        events.push(ModelStreamEvent::Completed(response));
        Ok(events)
    }

    fn accept_part(
        &mut self,
        part: &Value,
        events: &mut Vec<ModelStreamEvent>,
    ) -> Result<(), ProviderError> {
        if self.parts.len() == MAX_PARTS {
            return Err(invalid("too many Gemini streamed parts"));
        }
        let decoded = gemini_part(part)?;
        self.budget.retain_value(part)?;
        match decoded {
            GeminiPart::Text { text, thought } if !text.is_empty() => {
                // Both the provisional text and its native copy are retained.
                self.budget.retain(text.len())?;
                let count = &mut self.blocks[usize::from(thought)];
                if self.last_thought.replace(thought) != Some(thought) {
                    *count += 1;
                }
                let block = *count - 1;
                events.push(if thought {
                    ModelStreamEvent::ThinkingDelta {
                        block,
                        text: text.into(),
                    }
                } else {
                    ModelStreamEvent::TextDelta {
                        block,
                        text: text.into(),
                    }
                });
            }
            GeminiPart::Call { .. } => {
                self.last_thought = None;
                events.push(ModelStreamEvent::OutputObserved);
            }
            _ if part.get("thoughtSignature").is_some() => {
                events.push(ModelStreamEvent::OutputObserved)
            }
            _ => {}
        }
        self.parts.push(part.clone());
        Ok(())
    }
}
