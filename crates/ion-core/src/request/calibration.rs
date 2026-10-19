//! Capacity estimates from a committed exchange plus newly appended messages.
//!
//! Retained JSON is not the provider wire: it can duplicate visible content in
//! opaque replay and omit adapter-specific context encoding. Do not scale usage
//! by that representation. Instead retain the last known input + assistant output
//! baseline and estimate only messages appended after that assistant, as Pi does.
//! Byte limits and the replay-free fallback remain independent.
use std::sync::{Arc, Weak};

use ion_ai::{
    Content, GenerationControls, Message, ModelRef, ModelRequest, ModelRoute, ModelService,
    PromptCacheIntent, Reasoning, ToolSpec, Usage,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{json_size::encoded_len, prompt_size::NeutralMessages};

const IMAGE_INPUT_ALLOWANCE_TOKENS: u64 = 16_384;

/// One frozen request, including its independently bounded retained bytes.
#[derive(Clone)]
pub(crate) struct RequestMeasurement {
    retained_bytes: usize,
    fallback_input_tokens: u64,
    message_count: usize,
    image_identity: Option<[u8; 32]>,
    trailing_input_tokens: Option<u64>,
}

impl RequestMeasurement {
    pub(crate) fn measure(
        request: &ModelRequest,
        calibration: Option<&RequestCalibration>,
    ) -> Result<Self, serde_json::Error> {
        let retained_bytes = encoded_len(&LatestRequest::new(request, &request.messages))?;
        let neutral_bytes = if request
            .messages
            .iter()
            .any(|message| message.provider_replay.is_some())
        {
            encoded_len(&LatestRequest::new(
                request,
                NeutralMessages(&request.messages),
            ))?
        } else {
            retained_bytes
        };
        let images = measure_images(&request.messages);
        let fallback_input_tokens =
            bytes_to_tokens(neutral_bytes.saturating_sub(images.encoded_bytes))
                .saturating_add(images.count.saturating_mul(IMAGE_INPUT_ALLOWANCE_TOKENS));
        let trailing_input_tokens = match calibration {
            Some(sample) if sample.image_identity == images.identity => {
                match request.messages.get(sample.after_message..) {
                    // Known output usage covers the baseline assistant's replay.
                    // Later opaque content without a newer complete usage sample
                    // cannot be priced by its neutral text alone.
                    Some(trailing)
                        if trailing
                            .iter()
                            .all(|message| message.provider_replay.is_none()) =>
                    {
                        Some(if trailing.is_empty() {
                            0
                        } else {
                            bytes_to_tokens(encoded_len(&NeutralMessages(trailing))?)
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        Ok(Self {
            retained_bytes,
            fallback_input_tokens,
            message_count: request.messages.len(),
            image_identity: images.identity,
            trailing_input_tokens,
        })
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub(crate) fn fallback_input_tokens(&self) -> u64 {
        self.fallback_input_tokens
    }
}

/// Runtime compatibility, not credentials or durable execution authority.
#[derive(Clone)]
pub(crate) struct RequestScope {
    service: Weak<dyn ModelService>,
    model: ModelRef,
    context: [u8; 32],
}

impl RequestScope {
    pub(crate) fn new(
        service: &Arc<dyn ModelService>,
        request: &ModelRequest,
    ) -> Result<Self, serde_json::Error> {
        #[derive(Serialize)]
        struct Context<'a> {
            instructions: &'a Option<String>,
            tools: &'a [ToolSpec],
            reasoning: Reasoning,
        }
        let bytes = serde_json::to_vec(&Context {
            instructions: &request.instructions,
            tools: &request.tools,
            reasoning: request.controls.reasoning,
        })?;
        Ok(Self {
            service: Arc::downgrade(service),
            model: request.route.effective.clone(),
            context: Sha256::digest(bytes).into(),
        })
    }

    pub(crate) fn with_model(mut self, model: ModelRef) -> Self {
        self.model = model;
        self
    }

    fn matches(&self, current: &Self) -> bool {
        self.model == current.model
            && self.context == current.context
            && match (self.service.upgrade(), current.service.upgrade()) {
                (Some(previous), Some(current)) => Arc::ptr_eq(&previous, &current),
                _ => false,
            }
    }
}

/// A known context boundary immediately after one committed assistant message.
/// The prefix already observed by the model remains stable within a compatible
/// epoch; compaction, replay rebasing and configuration changes invalidate this
/// process-local boundary.
#[derive(Clone)]
pub(crate) struct RequestCalibration {
    scope: RequestScope,
    after_message: usize,
    context_tokens: u64,
    image_identity: Option<[u8; 32]>,
}

impl RequestCalibration {
    pub(crate) fn new(
        measurement: &RequestMeasurement,
        scope: RequestScope,
        usage: Usage,
    ) -> Option<Self> {
        let input = usage.input_tokens.filter(|tokens| *tokens != 0)?;
        let output = usage.output_tokens?;
        Some(Self {
            scope,
            after_message: measurement.message_count.checked_add(1)?,
            // Ion input already includes cache buckets. Output is added exactly
            // once for the baseline assistant, which trailing estimation skips.
            context_tokens: input.saturating_add(output),
            image_identity: measurement.image_identity,
        })
    }

    pub(crate) fn matches_scope(&self, scope: &RequestScope) -> bool {
        self.scope.matches(scope)
    }

    pub(crate) fn applies(&self, current: &RequestMeasurement, scope: &RequestScope) -> bool {
        self.matches_scope(scope)
            && self.image_identity == current.image_identity
            && current.trailing_input_tokens.is_some()
    }

    pub(crate) fn estimate(&self, current: &RequestMeasurement) -> u64 {
        current.trailing_input_tokens.map_or_else(
            || current.fallback_input_tokens(),
            |trailing| self.context_tokens.saturating_add(trailing),
        )
    }
}

#[derive(Serialize)]
struct LatestRequest<'a, M> {
    route: &'a ModelRoute,
    provider_session_id: &'a Option<String>,
    instructions: &'a Option<String>,
    messages: M,
    tools: &'a [ToolSpec],
    context_timeline: Option<()>,
    prompt_cache: &'a PromptCacheIntent,
    controls: &'a GenerationControls,
}

impl<'a, M> LatestRequest<'a, M> {
    fn new(request: &'a ModelRequest, messages: M) -> Self {
        let ModelRequest {
            route,
            provider_session_id,
            instructions,
            messages: _,
            tools,
            context_timeline: _,
            prompt_cache,
            controls,
        } = request;
        Self {
            route,
            provider_session_id,
            instructions,
            messages,
            tools,
            context_timeline: None,
            prompt_cache,
            controls,
        }
    }
}

struct ImageMeasurement {
    encoded_bytes: usize,
    count: u64,
    identity: Option<[u8; 32]>,
}

fn measure_images(messages: &[Message]) -> ImageMeasurement {
    let mut encoded_bytes = 0usize;
    let mut count = 0u64;
    let mut identity = Sha256::new();
    for message in messages {
        for part in &message.content {
            match part {
                Content::Image(image) => {
                    hash_image(&mut identity, b'u', image);
                    encoded_bytes = encoded_bytes.saturating_add(image.data().len());
                    count = count.saturating_add(1);
                }
                Content::ToolResult(result) => {
                    for image in &result.images {
                        hash_image(&mut identity, b't', image);
                        encoded_bytes = encoded_bytes.saturating_add(image.data().len());
                        count = count.saturating_add(1);
                    }
                }
                Content::Text(_) | Content::Thinking(_) | Content::ToolCall(_) => {}
            }
        }
    }
    ImageMeasurement {
        encoded_bytes,
        count,
        identity: (count != 0).then(|| identity.finalize().into()),
    }
}

fn hash_image(identity: &mut Sha256, source: u8, image: &ion_ai::ImageContent) {
    identity.update([source]);
    hash_field(identity, image.mime_type().as_str().as_bytes());
    hash_field(identity, image.data().as_bytes());
}

fn hash_field(identity: &mut Sha256, value: &[u8]) {
    identity.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    identity.update(value);
}

fn bytes_to_tokens(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX).div_ceil(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> Arc<dyn ModelService> {
        struct Service;
        impl ModelService for Service {
            fn stream<'a>(
                &'a self,
                _request: ModelRequest,
            ) -> ion_ai::BoxFuture<'a, Result<ion_ai::ModelStream, ion_ai::ProviderError>>
            {
                Box::pin(async { unreachable!("measurement-only test service") })
            }
        }
        Arc::new(Service)
    }

    fn request() -> ModelRequest {
        ModelRequest {
            route: ModelRoute::direct(
                ModelRef {
                    provider: "fixture".into(),
                    model: "fixture".into(),
                },
                ion_ai::ModelRouteReason::UserRequest,
            ),
            provider_session_id: None,
            instructions: Some("instructions".into()),
            messages: Vec::new(),
            tools: Vec::new(),
            context_timeline: None,
            prompt_cache: PromptCacheIntent::Default,
            controls: GenerationControls {
                max_output_tokens: 100,
                temperature: None,
                top_p: None,
                reasoning: Reasoning::ProviderDefault,
                tool_choice: ion_ai::ToolChoice::Auto,
                parallel_tool_calls: true,
            },
        }
    }

    #[test]
    fn context_baseline_counts_output_once_and_cache_as_input_subsets() {
        let service = service();
        let request = request();
        let scope = RequestScope::new(&service, &request).unwrap();
        let mut measurement = RequestMeasurement::measure(&request, None).unwrap();
        let sample = RequestCalibration::new(
            &measurement,
            scope.clone(),
            Usage::known_with_cache(100, 20, 80, 10),
        )
        .unwrap();
        measurement.trailing_input_tokens = Some(7);
        assert_eq!(sample.estimate(&measurement), 127);

        let extreme =
            RequestCalibration::new(&measurement, scope, Usage::known(u64::MAX, u64::MAX)).unwrap();
        measurement.trailing_input_tokens = Some(u64::MAX);
        assert_eq!(extreme.estimate(&measurement), u64::MAX);
    }

    #[test]
    fn incomplete_usage_does_not_create_a_context_baseline() {
        let service = service();
        let request = request();
        let scope = RequestScope::new(&service, &request).unwrap();
        let measurement = RequestMeasurement::measure(&request, None).unwrap();
        for usage in [
            Usage::unknown(),
            Usage {
                output_tokens: None,
                ..Usage::known(100, 0)
            },
            Usage::known(0, 100),
        ] {
            assert!(RequestCalibration::new(&measurement, scope.clone(), usage).is_none());
        }
        assert!(RequestCalibration::new(&measurement, scope, Usage::known(100, 0)).is_some());
    }

    #[test]
    fn context_representation_changes_break_the_sample_scope() {
        let service = service();
        let base_request = request();
        let base_scope = RequestScope::new(&service, &base_request).unwrap();
        assert!(base_scope.matches(&base_scope));
        let mut changed = base_request.clone();
        changed.instructions = Some("different instructions".into());
        assert!(!base_scope.matches(&RequestScope::new(&service, &changed).unwrap()));
        changed = base_request.clone();
        changed.tools.push(ToolSpec {
            name: "different".into(),
            description: "different declaration".into(),
            input_schema: serde_json::json!({"type":"object"}),
        });
        assert!(!base_scope.matches(&RequestScope::new(&service, &changed).unwrap()));
        changed = base_request;
        changed.controls.reasoning = Reasoning::High;
        assert!(!base_scope.matches(&RequestScope::new(&service, &changed).unwrap()));
    }

    #[test]
    fn unmeasured_opaque_suffix_cannot_reuse_an_older_baseline() {
        let service = service();
        let mut request = request();
        let scope = RequestScope::new(&service, &request).unwrap();
        let measurement = RequestMeasurement::measure(&request, None).unwrap();
        let sample =
            RequestCalibration::new(&measurement, scope.clone(), Usage::known(100, 20)).unwrap();
        request.messages = vec![
            Message {
                role: ion_ai::Role::Assistant,
                content: vec![Content::Text("known".into())],
                provider_replay: None,
            },
            Message {
                role: ion_ai::Role::Assistant,
                content: vec![Content::Text("unknown".into())],
                provider_replay: Some(ion_ai::ProviderReplay::new(
                    "fixture",
                    "opaque",
                    serde_json::json!({"hidden":"unmeasured"}),
                )),
            },
        ];
        let current = RequestMeasurement::measure(&request, Some(&sample)).unwrap();
        assert!(!sample.applies(&current, &scope));
        assert_eq!(sample.estimate(&current), current.fallback_input_tokens());
    }

    #[test]
    fn known_replay_is_covered_by_usage_not_retained_json_growth() {
        let service = service();
        let mut request = request();
        let scope = RequestScope::new(&service, &request).unwrap();
        let prior = RequestMeasurement::measure(&request, None).unwrap();
        let sample = RequestCalibration::new(&prior, scope.clone(), Usage::known(100, 20)).unwrap();
        request.messages = vec![
            Message {
                role: ion_ai::Role::Assistant,
                content: vec![Content::Text("known".into())],
                provider_replay: Some(ion_ai::ProviderReplay::new(
                    "fixture",
                    "opaque",
                    serde_json::json!({"duplicate":"known","signature":"s".repeat(20_000)}),
                )),
            },
            Message {
                role: ion_ai::Role::User,
                content: vec![Content::Text("new input".into())],
                provider_replay: None,
            },
        ];
        let expected_tail =
            bytes_to_tokens(encoded_len(&NeutralMessages(&request.messages[1..])).unwrap());
        let current = RequestMeasurement::measure(&request, Some(&sample)).unwrap();
        assert!(sample.applies(&current, &scope));
        assert_eq!(sample.estimate(&current), 120 + expected_tail);
    }

    #[test]
    fn image_identity_distinguishes_user_and_tool_result_images() {
        let image = ion_ai::ImageContent::from_bytes(&[
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ])
        .unwrap();
        let user = measure_images(&[ion_ai::Message {
            role: ion_ai::Role::User,
            content: vec![Content::Image(image.clone())],
            provider_replay: None,
        }]);
        let tool = measure_images(&[ion_ai::Message {
            role: ion_ai::Role::Tool,
            content: vec![Content::ToolResult(ion_ai::ToolResult {
                call_id: "image".into(),
                name: "read".into(),
                result: serde_json::json!({}),
                images: vec![image],
                is_error: false,
            })],
            provider_replay: None,
        }]);
        assert_eq!(user.count, 1);
        assert_eq!(tool.count, 1);
        assert_ne!(user.identity, tool.identity);
    }
}
