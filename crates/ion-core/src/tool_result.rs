//! Observed tool output and its separate, durable model-context projection.
use std::borrow::Cow;

use ion_ai::{Content, Message, Role, ToolResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::json_size::encoded_len;

const ARTIFACT_RETRIEVAL: &str = "Retained Session bytes. Use history action artifact with this result entry and zero-based artifact index; Code Mode children use inspectSaved kind artifact. Offsets and next_offset are raw bytes; data_bytes is this page's original raw byte count, not JS string or encoded length; page encoding is utf8 for readable text or base64 for other bytes. Consume the final page's data before treating null next_offset as EOF; follow nonnull next_offset.";

#[derive(Serialize)]
struct ArtifactModelOutput<'a> {
    entry: u64,
    output: &'a Value,
    artifacts: &'a [crate::ArtifactRef],
    retrieval: &'static str,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolOutput {
    pub value: Value,
    pub images: Vec<ion_ai::ImageContent>,
    pub artifacts: Vec<crate::ArtifactRef>,
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
            artifacts: Vec::new(),
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
                    artifacts: Vec::new(),
                    value: result.result,
                    images: result.images,
                    is_error: result.is_error,
                },
                projection,
            },
        }
    }

    pub(crate) fn message(&self, entry: u64, display: bool) -> Message {
        let mut output = if let ToolOutcome::Observed { output, projection } = &self.outcome
            && !display
            && let Some(notice) = projection.model_notice(entry)
        {
            ToolOutput {
                artifacts: Vec::new(),
                value: serde_json::json!({"output_withheld":projection,"notice":notice}),
                images: Vec::new(),
                is_error: output.is_error,
            }
        } else {
            self.outcome.inspection_output().into_owned()
        };
        let images = std::mem::take(&mut output.images);
        let is_error = output.is_error;
        let result = ToolResult {
            call_id: self.call_id.clone(),
            name: self.name.clone(),
            result: output.into_model_value(entry),
            images,
            is_error,
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
    pub fn success(value: Value) -> Self {
        Self {
            value,
            images: Vec::new(),
            artifacts: Vec::new(),
            is_error: false,
        }
    }

    /// A model-facing envelope derived from typed metadata. Opaque tool JSON
    /// is never scanned for artifact authority or overwritten by reserved keys.
    pub(crate) fn into_model_value(self, entry: u64) -> Value {
        if self.artifacts.is_empty() {
            self.value
        } else {
            serde_json::json!({"entry":entry,"output":self.value,"artifacts":self.artifacts,
                "retrieval":ARTIFACT_RETRIEVAL})
        }
    }

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
        let bytes = if self.artifacts.is_empty() {
            encoded_len(&(&self.value, &self.images))?
        } else {
            encoded_len(&(
                ArtifactModelOutput {
                    entry: u64::MAX,
                    output: &self.value,
                    artifacts: &self.artifacts,
                    retrieval: ARTIFACT_RETRIEVAL,
                },
                &self.images,
            ))?
        };
        if bytes > max_request_bytes {
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

    fn model_notice(self, entry: u64) -> Option<String> {
        match self {
            Self::Observed => None,
            Self::ImagesUnsupported => Some(format!(
                "The observed tool output includes images unsupported by this route and was withheld from model context. Committed Session entry {entry} retains its original output. Withholding does not undo effects or authorize rerunning the tool."
            )),
            Self::RequestLimitExceeded => Some(format!(
                "The observed tool output exceeds this route's request bound and was withheld from model context. Committed Session entry {entry} retains its original output. Withholding does not undo effects or authorize rerunning the tool."
            )),
        }
    }
}
