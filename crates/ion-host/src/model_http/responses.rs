//! Responses SSE reduction. Snapshots can fill gaps, never rewrite observed output.
use super::responses_request::{RESPONSES_REPLAY, project_output, string};
use super::*;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Channel {
    Text(usize),
    Refusal(usize),
    Summary(usize),
    Reasoning(usize),
    Arguments,
}

#[derive(Default)]
struct Accumulation {
    text: String,
    done: bool,
    thinking_block: Option<usize>,
}

struct Item {
    initial: Value,
    channels: BTreeMap<Channel, Accumulation>,
    parts: BTreeMap<Channel, (Value, bool)>,
    final_item: Option<Value>,
}

pub(super) struct ResponsesState {
    pub(super) budget: ResponseBudget,
    id: Option<String>,
    model: Option<String>,
    items: BTreeMap<usize, Item>,
    thinking_blocks: usize,
    usage: Usage,
    pub(super) pending_error: Option<ProviderError>,
}

impl Default for ResponsesState {
    fn default() -> Self {
        Self {
            budget: ResponseBudget::default(),
            id: None,
            model: None,
            items: BTreeMap::new(),
            thinking_blocks: 0,
            usage: Usage::unknown(),
            pending_error: None,
        }
    }
}

fn index(value: &Value, field: &str) -> Result<usize, ProviderError> {
    value[field]
        .as_u64()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or_else(|| invalid(&format!("invalid Responses {field}")))
}

fn bind(
    target: &mut Option<String>,
    value: &Value,
    field: &str,
    budget: &mut ResponseBudget,
) -> Result<(), ProviderError> {
    if let Some(value) = value.get(field) {
        let value = value
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| invalid("invalid Responses identity"))?;
        match target {
            Some(old) if old != value => {
                return Err(invalid("Responses identity changed during stream"));
            }
            Some(_) => {}
            None => {
                budget.retain(value.len())?;
                *target = Some(value.to_owned());
            }
        }
    }
    Ok(())
}

fn stable_identity(initial: &Value, final_item: &Value) -> Result<(), ProviderError> {
    for key in [
        "id",
        "type",
        "call_id",
        "name",
        "namespace",
        "role",
        "phase",
        "encrypted_content",
    ] {
        if let Some(value) = initial
            .get(key)
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
            && final_item.get(key) != Some(value)
        {
            return Err(invalid("Responses item identity changed"));
        }
    }
    Ok(())
}

/// Final item records may omit ciphertext until the terminal response. Every
/// other accepted field is final; adding a missing field is not rewriting one.
fn reconcile_done(old: &Value, new: &Value) -> Result<(), ProviderError> {
    let fields = old
        .as_object()
        .ok_or_else(|| invalid("invalid Responses item"))?;
    for (key, value) in fields {
        if key == "encrypted_content" && value.is_null() {
            continue;
        }
        if new.get(key) != Some(value) {
            return Err(invalid("Responses final item changed"));
        }
    }
    Ok(())
}

fn channel_text(item: &Value, channel: Channel) -> Result<&str, ProviderError> {
    match channel {
        Channel::Text(i) => {
            if item["content"][i]["type"] != "output_text" {
                return Err(invalid("Responses content kind changed"));
            }
            string(&item["content"][i], "text")
        }
        Channel::Refusal(i) => {
            if item["content"][i]["type"] != "refusal" {
                return Err(invalid("Responses content kind changed"));
            }
            string(&item["content"][i], "refusal")
        }
        Channel::Summary(i) => {
            if item["summary"][i]["type"] != "summary_text" {
                return Err(invalid("Responses summary kind changed"));
            }
            string(&item["summary"][i], "text")
        }
        Channel::Reasoning(i) => {
            if item["content"][i]["type"] != "reasoning_text" {
                return Err(invalid("Responses reasoning content kind changed"));
            }
            string(&item["content"][i], "text")
        }
        Channel::Arguments => string(item, "arguments"),
    }
}

impl Item {
    fn check(&self, value: &Value) -> Result<(), ProviderError> {
        stable_identity(&self.initial, value)?;
        if let Some(old) = &self.final_item {
            reconcile_done(old, value)?;
        }
        for (channel, accumulation) in &self.channels {
            let text = channel_text(value, *channel)?;
            if if accumulation.done {
                text != accumulation.text
            } else {
                !text.starts_with(&accumulation.text)
            } {
                return Err(invalid("Responses snapshot contradicts streamed content"));
            }
        }
        for (channel, (part, done)) in &self.parts {
            let (key, i) = match channel {
                Channel::Text(i) | Channel::Refusal(i) | Channel::Reasoning(i) => ("content", *i),
                Channel::Summary(i) => ("summary", *i),
                Channel::Arguments => unreachable!(),
            };
            if *done {
                reconcile_done(part, &value[key][i])?;
            } else if part["type"] != value[key][i]["type"] {
                return Err(invalid("Responses part kind changed"));
            }
        }
        Ok(())
    }
}

impl ResponsesState {
    pub(super) fn accept(
        &mut self,
        value: &Value,
        request: &ModelRequest,
    ) -> Result<Vec<ModelStreamEvent>, ProviderError> {
        let kind = string(value, "type")?;
        let mut events = Vec::new();
        match kind {
            "response.created" | "response.in_progress" => {
                self.metadata(&value["response"])?;
                if value["response"]["output"]
                    .as_array()
                    .is_some_and(|output| !output.is_empty())
                {
                    events.push(ModelStreamEvent::OutputObserved);
                }
            }
            "response.output_item.added" => {
                let i = index(value, "output_index")?;
                let item = &value["item"];
                if self.items.contains_key(&i) {
                    return Err(invalid("duplicate Responses output index"));
                }
                let id = string(item, "id")?;
                if id.is_empty() || self.items.values().any(|old| old.initial["id"] == id) {
                    return Err(invalid("duplicate or empty Responses item ID"));
                }
                if !matches!(
                    string(item, "type")?,
                    "message" | "function_call" | "reasoning"
                ) {
                    return Err(unsupported("unsupported Responses output item"));
                }
                self.budget.retain_value(item)?;
                let mut state = Item {
                    initial: item.clone(),
                    channels: BTreeMap::new(),
                    parts: BTreeMap::new(),
                    final_item: None,
                };
                events.push(ModelStreamEvent::OutputObserved);
                let mut initial = Vec::new();
                match string(item, "type")? {
                    "function_call" => {
                        if let Some(text) = item.get("arguments") {
                            initial.push((
                                Channel::Arguments,
                                text.as_str().ok_or_else(|| {
                                    invalid("invalid Responses initial arguments")
                                })?,
                            ));
                        }
                    }
                    "message" => {
                        if let Some(parts) = item.get("content") {
                            for (i, part) in parts
                                .as_array()
                                .ok_or_else(|| invalid("invalid Responses initial content"))?
                                .iter()
                                .enumerate()
                            {
                                initial.push(match string(part, "type")? {
                                    "output_text" => (Channel::Text(i), string(part, "text")?),
                                    "refusal" => (Channel::Refusal(i), string(part, "refusal")?),
                                    _ => {
                                        return Err(unsupported(
                                            "unsupported Responses initial content",
                                        ));
                                    }
                                });
                            }
                        }
                    }
                    "reasoning" => {
                        if let Some(parts) = item.get("summary") {
                            for (i, part) in parts
                                .as_array()
                                .ok_or_else(|| invalid("invalid Responses initial summary"))?
                                .iter()
                                .enumerate()
                            {
                                if string(part, "type")? != "summary_text" {
                                    return Err(unsupported(
                                        "unsupported Responses initial summary",
                                    ));
                                }
                                initial.push((Channel::Summary(i), string(part, "text")?));
                            }
                        }
                        if let Some(parts) = item.get("content").filter(|v| !v.is_null()) {
                            for (i, part) in parts
                                .as_array()
                                .ok_or_else(|| {
                                    invalid("invalid Responses initial reasoning content")
                                })?
                                .iter()
                                .enumerate()
                            {
                                if string(part, "type")? != "reasoning_text" {
                                    return Err(unsupported(
                                        "unsupported Responses reasoning content",
                                    ));
                                }
                                initial.push((Channel::Reasoning(i), string(part, "text")?));
                            }
                        }
                    }
                    _ => unreachable!(),
                }
                for (channel, text) in initial {
                    update(
                        &mut state,
                        channel,
                        text,
                        Update::Snapshot,
                        &mut self.budget,
                        &mut self.thinking_blocks,
                        &mut events,
                    )?;
                }
                self.items.insert(i, state);
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta"
            | "response.reasoning_text.delta"
            | "response.function_call_arguments.delta"
            | "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_summary_text.done"
            | "response.reasoning_text.done"
            | "response.function_call_arguments.done" => {
                let i = index(value, "output_index")?;
                let channel = if kind.contains("function_call_arguments") {
                    Channel::Arguments
                } else if kind.contains("reasoning_summary_text") {
                    Channel::Summary(index(value, "summary_index")?)
                } else if kind.contains("reasoning_text") {
                    Channel::Reasoning(index(value, "content_index")?)
                } else if kind.contains("refusal") {
                    Channel::Refusal(index(value, "content_index")?)
                } else {
                    Channel::Text(index(value, "content_index")?)
                };
                let item = self
                    .items
                    .get_mut(&i)
                    .ok_or_else(|| invalid("Responses delta has no output item"))?;
                if item.final_item.is_some()
                    || string(value, "item_id")? != string(&item.initial, "id")?
                {
                    return Err(invalid(
                        "Responses delta targets a finalized or different item",
                    ));
                }
                let done = kind.ends_with(".done");
                let text = string(
                    value,
                    if !done {
                        "delta"
                    } else {
                        match channel {
                            Channel::Arguments => "arguments",
                            Channel::Refusal(_) => "refusal",
                            _ => "text",
                        }
                    },
                )?;
                update(
                    item,
                    channel,
                    text,
                    if done { Update::Final } else { Update::Delta },
                    &mut self.budget,
                    &mut self.thinking_blocks,
                    &mut events,
                )?;
            }
            "response.content_part.added" | "response.reasoning_summary_part.added" => {
                let i = index(value, "output_index")?;
                let item = self
                    .items
                    .get_mut(&i)
                    .ok_or_else(|| invalid("Responses part has no output item"))?;
                if item.final_item.is_some()
                    || string(value, "item_id")? != string(&item.initial, "id")?
                {
                    return Err(invalid(
                        "Responses part targets a finalized or different item",
                    ));
                }
                let channel = part_channel(kind, value)?;
                if item.parts.contains_key(&channel) {
                    return Err(invalid("duplicate Responses content part"));
                }
                let part = &value["part"];
                let key = if matches!(channel, Channel::Refusal(_)) {
                    "refusal"
                } else {
                    "text"
                };
                let text = string(part, key)?;
                self.budget.retain_value(part)?;
                update(
                    item,
                    channel,
                    text,
                    Update::Snapshot,
                    &mut self.budget,
                    &mut self.thinking_blocks,
                    &mut events,
                )?;
                item.parts.insert(channel, (part.clone(), false));
            }
            "response.content_part.done" | "response.reasoning_summary_part.done" => {
                let i = index(value, "output_index")?;
                let item = self
                    .items
                    .get_mut(&i)
                    .ok_or_else(|| invalid("Responses part has no output item"))?;
                if item.final_item.is_some()
                    || string(value, "item_id")? != string(&item.initial, "id")?
                {
                    return Err(invalid(
                        "Responses part targets a finalized or different item",
                    ));
                }
                let channel = part_channel(kind, value)?;
                let part = &value["part"];
                if let Some((old, done)) = item.parts.get(&channel) {
                    if *done {
                        reconcile_done(old, part)?;
                    } else if old["type"] != part["type"] {
                        return Err(invalid("Responses part kind changed"));
                    }
                }
                let key = if matches!(channel, Channel::Refusal(_)) {
                    "refusal"
                } else {
                    "text"
                };
                let text = string(part, key)?;
                update(
                    item,
                    channel,
                    text,
                    Update::Final,
                    &mut self.budget,
                    &mut self.thinking_blocks,
                    &mut events,
                )?;
                self.budget.retain_value(part)?;
                item.parts.insert(channel, (part.clone(), true));
            }
            "response.output_item.done" => {
                let i = index(value, "output_index")?;
                let item = self
                    .items
                    .get_mut(&i)
                    .ok_or_else(|| invalid("Responses done has no output item"))?;
                if item.final_item.is_some() {
                    return Err(invalid("duplicate Responses item finalization"));
                }
                item.check(&value["item"])?;
                project_output(std::slice::from_ref(&value["item"]), false)?;
                self.budget.retain_value(&value["item"])?;
                item.final_item = Some(value["item"].clone());
                item.channels.clear();
                item.parts.clear();
                events.push(ModelStreamEvent::OutputObserved);
            }
            "response.completed" | "response.incomplete" => {
                let response = &value["response"];
                self.metadata(response)?;
                let completed = kind == "response.completed";
                let expected = if completed { "completed" } else { "incomplete" };
                if string(response, "status")? != expected {
                    return Err(invalid("Responses terminal status disagrees with event"));
                }
                let output = response["output"]
                    .as_array()
                    .ok_or_else(|| invalid("invalid Responses terminal output"))?;
                for (i, item) in &self.items {
                    item.check(
                        output
                            .get(*i)
                            .ok_or_else(|| invalid("Responses terminal output omitted an item"))?,
                    )?;
                }
                // Retain the native terminal array, including terminal-only
                // reasoning. It remains coupled to its neutral projection.
                self.budget.retain_value(&response["output"])?;
                let content = project_output(output, completed)?;
                let termination = if completed {
                    ResponseTermination::Completed
                } else {
                    ResponseTermination::Incomplete(
                        match string(&response["incomplete_details"], "reason")? {
                            "max_output_tokens" => IncompleteReason::MaxOutputTokens,
                            "content_filter" => IncompleteReason::ContentFilter,
                            "context_length_exceeded" => IncompleteReason::ContextLength,
                            other => IncompleteReason::Other(other.to_owned()),
                        },
                    )
                };
                let response = ModelResponse {
                    message: Message {
                        role: Role::Assistant,
                        content,
                        provider_replay: Some(ProviderReplay::new(
                            &request.route.effective.provider,
                            RESPONSES_REPLAY,
                            json!({"model":request.route.effective.model,"output":output}),
                        )),
                    },
                    usage: self.usage,
                    termination,
                    returned_model: self.model.clone(),
                };
                if response.is_complete() {
                    validate_output(request, &response)?;
                }
                events.push(ModelStreamEvent::Usage(self.usage));
                events.push(ModelStreamEvent::Completed(response));
            }
            "response.failed" | "error" => {
                let mut failure = if kind == "response.failed" {
                    self.metadata(&value["response"])?;
                    chat_stream_error(&json!({"error":value["response"]["error"]}))
                } else {
                    chat_stream_error(&json!({"error":value}))
                };
                // A failed terminal record can itself contain generated output.
                // Publish that evidence before raising the classified error.
                if kind == "response.failed"
                    && value["response"]["output"]
                        .as_array()
                        .is_some_and(|output| !output.is_empty())
                {
                    events.push(ModelStreamEvent::OutputObserved);
                }
                if failure.message.is_empty() {
                    failure.message = "Responses generation failed".into();
                }
                self.pending_error = Some(failure);
            }
            // Annotation/accounting extension events do not authorize calls or
            // replace the terminal snapshot. Common transport budgets bound them.
            _ if kind.starts_with("response.") => {}
            _ => return Err(invalid("invalid Responses stream event")),
        }
        Ok(events)
    }

    fn metadata(&mut self, value: &Value) -> Result<(), ProviderError> {
        if !value.is_object() {
            return Err(invalid("invalid Responses response object"));
        }
        bind(&mut self.id, value, "id", &mut self.budget)?;
        bind(&mut self.model, value, "model", &mut self.budget)?;
        if let Some(usage) = value.get("usage").filter(|v| !v.is_null()) {
            set_usage(&mut self.usage, usage)?;
        }
        Ok(())
    }
}

enum Update {
    Delta,
    Snapshot,
    Final,
}

fn update(
    item: &mut Item,
    channel: Channel,
    text: &str,
    update: Update,
    budget: &mut ResponseBudget,
    blocks: &mut usize,
    events: &mut Vec<ModelStreamEvent>,
) -> Result<(), ProviderError> {
    let expected = match channel {
        Channel::Arguments => "function_call",
        Channel::Summary(_) | Channel::Reasoning(_) => "reasoning",
        _ => "message",
    };
    if item.initial["type"] != expected {
        return Err(invalid("Responses content kind disagrees with item"));
    }
    if !item.channels.contains_key(&channel) {
        budget.retain(std::mem::size_of::<Accumulation>())?;
    }
    let accumulation = item.channels.entry(channel).or_default();
    let appended = match update {
        Update::Delta if accumulation.done => {
            return Err(invalid("Responses content changed after finalization"));
        }
        Update::Delta => text,
        Update::Snapshot if accumulation.text.starts_with(text) => "",
        Update::Snapshot if accumulation.done => {
            return Err(invalid("Responses finalized content changed"));
        }
        Update::Final if accumulation.done && text != accumulation.text => {
            return Err(invalid("Responses finalized content changed"));
        }
        Update::Snapshot | Update::Final => text
            .strip_prefix(&accumulation.text)
            .ok_or_else(|| invalid("Responses snapshot contradicts delta"))?,
    };
    budget.retain(appended.len())?;
    accumulation.text.push_str(appended);
    if matches!(update, Update::Final) {
        accumulation.done = true;
    }
    if !appended.is_empty() {
        match channel {
            Channel::Text(_) | Channel::Refusal(_) => {
                events.push(ModelStreamEvent::TextDelta(appended.to_owned()))
            }
            Channel::Summary(_) | Channel::Reasoning(_) => {
                let block = *accumulation.thinking_block.get_or_insert_with(|| {
                    let block = *blocks;
                    *blocks += 1;
                    block
                });
                events.push(ModelStreamEvent::ThinkingDelta {
                    block,
                    text: appended.to_owned(),
                });
            }
            Channel::Arguments => events.push(ModelStreamEvent::OutputObserved),
        }
    }
    Ok(())
}

fn part_channel(kind: &str, value: &Value) -> Result<Channel, ProviderError> {
    let part = &value["part"];
    Ok(if kind.contains("reasoning_summary_part") {
        if string(part, "type")? != "summary_text" {
            return Err(invalid("invalid Responses summary part"));
        }
        Channel::Summary(index(value, "summary_index")?)
    } else {
        let i = index(value, "content_index")?;
        match string(part, "type")? {
            "output_text" => Channel::Text(i),
            "refusal" => Channel::Refusal(i),
            "reasoning_text" => Channel::Reasoning(i),
            _ => return Err(unsupported("unsupported Responses content part")),
        }
    })
}

fn count(value: &Value, key: &str) -> Result<Option<u64>, ProviderError> {
    value
        .get(key)
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| invalid("invalid Responses usage count"))
        })
        .transpose()
}
fn set_usage(usage: &mut Usage, value: &Value) -> Result<(), ProviderError> {
    if !value.is_object() {
        return Err(invalid("invalid Responses usage"));
    }
    if let Some(n) = count(value, "input_tokens")? {
        usage.input_tokens = Some(n);
    }
    if let Some(n) = count(value, "output_tokens")? {
        usage.output_tokens = Some(n);
    }
    for key in ["input_tokens_details", "output_tokens_details"] {
        if value
            .get(key)
            .is_some_and(|v| !v.is_null() && !v.is_object())
        {
            return Err(invalid("invalid Responses usage details"));
        }
    }
    if let Some(n) = count(&value["input_tokens_details"], "cached_tokens")? {
        usage.cache_read_input_tokens = Some(n);
    }
    if let (Some(total), Some(cached)) = (usage.input_tokens, usage.cache_read_input_tokens)
        && cached > total
    {
        return Err(invalid("Responses cached tokens exceed input"));
    }
    if let (Some(total), Some(reasoning)) = (
        usage.output_tokens,
        count(&value["output_tokens_details"], "reasoning_tokens")?,
    ) && reasoning > total
    {
        return Err(invalid("Responses reasoning tokens exceed output"));
    }
    if let (Some(total), Some(input), Some(output)) = (
        count(value, "total_tokens")?,
        usage.input_tokens,
        usage.output_tokens,
    ) && input.checked_add(output) != Some(total)
    {
        return Err(invalid("Responses total usage disagrees"));
    }
    Ok(())
}
