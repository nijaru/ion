//! Observed tool output and its separate, durable model-context projection.
use std::borrow::Cow;

use ion_ai::{Content, Message, Role, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json_size::encoded_len;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolOutput {
    pub value: Value,
    pub images: Vec<ion_ai::ImageContent>,
    pub is_error: bool,
}

/// Durable execution truth. Synthetic recovery/rejection notices are generated
/// for consumers, never persisted as if a host returned them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ToolOutcome {
    Observed {
        output: ToolOutput,
        projection: ToolResultProjection,
    },
    NotDispatched {
        reason: String,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSettlement {
    pub call_id: String,
    pub name: String,
    pub outcome: ToolOutcome,
}

impl ToolOutcome {
    pub fn inspection_output(&self) -> Cow<'_, ToolOutput> {
        let error = match self {
            Self::Observed { output, .. } => return Cow::Borrowed(output),
            Self::NotDispatched { reason } => format!("Tool call was not dispatched: {reason}"),
            Self::Unknown => "The tool result was not committed. Its external effect is unknown; inspect the working directory before retrying.".into(),
        };
        Cow::Owned(ToolOutput {
            value: serde_json::json!({"error": error}),
            images: Vec::new(),
            is_error: true,
        })
    }

    pub fn projection(&self) -> Option<ToolResultProjection> {
        match self {
            Self::Observed { projection, .. } => Some(*projection),
            Self::NotDispatched { .. } | Self::Unknown => None,
        }
    }
}

impl ToolSettlement {
    #[cfg(test)]
    pub(crate) fn observed(result: ToolResult, projection: ToolResultProjection) -> Self {
        Self {
            call_id: result.call_id,
            name: result.name,
            outcome: ToolOutcome::Observed {
                output: ToolOutput {
                    value: result.result,
                    images: result.images,
                    is_error: result.is_error,
                },
                projection,
            },
        }
    }

    pub(crate) fn message(&self, display: bool) -> Message {
        let error = self
            .outcome
            .projection()
            .filter(|_| !display)
            .and_then(ToolResultProjection::model_error);
        let output = match error {
            Some(error) => ToolOutput {
                value: serde_json::json!({"error":error}),
                images: Vec::new(),
                is_error: true,
            },
            None => self.outcome.inspection_output().into_owned(),
        };
        let result = ToolResult {
            call_id: self.call_id.clone(),
            name: self.name.clone(),
            result: output.value,
            images: output.images,
            is_error: output.is_error,
        };
        Message {
            role: Role::Tool,
            content: vec![Content::ToolResult(result)],
            provider_replay: None,
        }
    }
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
        if encoded_len(&(&self.value, &self.images))? > max_request_bytes {
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

    fn model_error(self) -> Option<&'static str> {
        match self {
            Self::Observed => None,
            Self::ImagesUnsupported => Some(
                "Selected model route does not support image tool results. The observed result is retained for inspection; choose an image-capable model and read the file again.",
            ),
            Self::RequestLimitExceeded => Some(
                "The tool result exceeds this route's request bound and was not shared with the model. Inspect the stored tool result; the tool may already have affected the workspace.",
            ),
        }
    }
}
