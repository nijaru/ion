//! Shared transcript validation and call/result pairing, without wire policy.
use super::*;
use std::borrow::Cow;

pub(super) fn provider_replay<'a>(
    message: &'a Message,
    provider: &str,
) -> Result<Option<&'a ProviderReplay>, ProviderError> {
    match &message.provider_replay {
        Some(_) if message.role != Role::Assistant => {
            Err(invalid("provider replay requires an assistant message"))
        }
        Some(replay) if !replay.is_compatible_with(provider) => Err(unsupported(
            "provider replay belongs to a different provider",
        )),
        replay => Ok(replay.as_ref()),
    }
}

pub(super) fn incompatible_replay() -> ProviderError {
    unsupported(
        "provider replay is incompatible with route or wire encoding; explicitly compact or start a new Session",
    )
}

#[derive(Default)]
pub(super) struct Transcript<'a> {
    pending: BTreeMap<&'a str, (&'a str, Cow<'a, str>)>,
}

/// Validated borrowed content. The encoder may assign a destination call ID;
/// its paired result receives that exact ID, not a new alias or text match.
pub(super) enum Part<'a, 'pending> {
    Thinking,
    Text(&'a str),
    Image(&'a ion_ai::ImageContent),
    ToolCall {
        call: &'a ToolCall,
        id: &'pending mut Cow<'a, str>,
    },
    ToolResult {
        result: &'a ion_ai::ToolResult,
        id: Cow<'a, str>,
    },
}

impl<'a> Transcript<'a> {
    pub(super) fn begin(&self, message: &Message) -> Result<(), ProviderError> {
        if message.content.is_empty() {
            return Err(invalid("empty transcript message"));
        }
        if message.role != Role::Tool && !self.pending.is_empty() {
            return Err(invalid("unanswered tool calls"));
        }
        Ok(())
    }

    pub(super) fn part(
        &mut self,
        role: Role,
        content: &'a Content,
    ) -> Result<Part<'a, '_>, ProviderError> {
        match (role, content) {
            (Role::Assistant, Content::Thinking(_)) => Ok(Part::Thinking),
            (Role::User | Role::Assistant, Content::Text(text)) => Ok(Part::Text(text)),
            (Role::User, Content::Image(image)) => {
                image
                    .validate()
                    .map_err(|_| invalid("invalid image data in transcript"))?;
                Ok(Part::Image(image))
            }
            (Role::Assistant, Content::ToolCall(call)) => {
                if !valid_identifier(&call.name)
                    || !call.arguments.is_object()
                    || call.id.is_empty()
                {
                    return Err(invalid("invalid or duplicate tool call"));
                }
                let std::collections::btree_map::Entry::Vacant(entry) =
                    self.pending.entry(&call.id)
                else {
                    return Err(invalid("invalid or duplicate tool call"));
                };
                let (_, id) = entry.insert((&call.name, Cow::Borrowed(&call.id)));
                Ok(Part::ToolCall { call, id })
            }
            (Role::Tool, Content::ToolResult(result)) => {
                let Some((name, id)) = self.pending.remove(result.call_id.as_str()) else {
                    return Err(invalid("orphan or duplicate tool result"));
                };
                if result.name != name {
                    return Err(invalid("tool result name does not match call"));
                }
                for image in &result.images {
                    image
                        .validate()
                        .map_err(|_| invalid("invalid tool image data in transcript"))?;
                }
                Ok(Part::ToolResult { result, id })
            }
            _ => Err(invalid("content does not match transcript role")),
        }
    }

    pub(super) fn finish(self) -> Result<(), ProviderError> {
        if self.pending.is_empty() {
            Ok(())
        } else {
            Err(invalid("unanswered tool calls"))
        }
    }
}
