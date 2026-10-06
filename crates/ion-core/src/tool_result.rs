//! Observed tool output and its separate, durable model-context projection.
use ion_ai::{Content, Message, Role, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolOutput {
    pub value: Value,
    pub images: Vec<ion_ai::ImageContent>,
    pub is_error: bool,
}

/// A route limit changes model delivery, not the observed external outcome.
/// Persist the decision beside the raw result so reopen/compaction cannot
/// accidentally reintroduce withheld payloads into model context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultProjection {
    Observed,
    ImagesUnsupported,
    RequestLimitExceeded,
}

impl ToolOutput {
    pub(crate) fn model_projection(
        &self,
        image_input: bool,
        max_request_bytes: usize,
    ) -> Result<ToolResultProjection, serde_json::Error> {
        if !image_input && !self.images.is_empty() {
            return Ok(ToolResultProjection::ImagesUnsupported);
        }
        // ImageContent validates at construction/deserialization. It is not an
        // arbitrary base64 string requiring another tolerant validation branch.
        if serde_json::to_vec(&(&self.value, &self.images))?.len() > max_request_bytes {
            return Ok(ToolResultProjection::RequestLimitExceeded);
        }
        Ok(ToolResultProjection::Observed)
    }
}

impl ToolResultProjection {
    /// Notice for human inspection; this is not a tool failure classification.
    pub fn notice(self) -> Option<&'static str> {
        match self {
            Self::Observed => None,
            Self::ImagesUnsupported => Some("Image result not shared with this text-only model"),
            Self::RequestLimitExceeded => {
                Some("Result not shared with model: exceeds request limit")
            }
        }
    }

    pub(crate) fn message(self, observed: &ToolResult) -> Message {
        let error = match self {
            Self::Observed => None,
            Self::ImagesUnsupported => Some(
                "Selected model route does not support image tool results. The observed result is retained for inspection; choose an image-capable model and read the file again.",
            ),
            Self::RequestLimitExceeded => Some(
                "The tool result exceeds this route's request bound and was not shared with the model. Inspect the stored tool result; the tool may already have affected the workspace.",
            ),
        };
        let result = error.map_or_else(
            || observed.clone(),
            |error| ToolResult {
                call_id: observed.call_id.clone(),
                name: observed.name.clone(),
                result: serde_json::json!({"error": error}),
                images: Vec::new(),
                is_error: true,
            },
        );
        Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(result)],
            provider_replay: None,
        }
    }
}
