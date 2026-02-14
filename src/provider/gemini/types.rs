//! Gemini Code Assist API types.

use crate::provider::types::{ContentBlock, Message, Role};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Code Assist API request wrapper (matching Gemini CLI format).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeAssistRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_prompt_id: Option<String>,
    pub request: VertexRequest,
}

/// Inner request structure for Code Assist API.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VertexRequest {
    pub contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiRequest {
    pub contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_response: Option<GeminiFunctionResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GeminiFunctionCall {
    pub name: String,
    pub args: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct GeminiFunctionResponse {
    pub name: String,
    pub response: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiTool {
    pub function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GeminiFunctionDeclaration {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

/// Code Assist API response wrapper.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodeAssistResponse {
    pub response: GeminiResponse,
    #[allow(dead_code)]
    pub trace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiResponse {
    pub candidates: Option<Vec<GeminiCandidate>>,
    #[allow(dead_code)]
    pub usage_metadata: Option<GeminiUsageMetadata>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiCandidate {
    pub content: Option<GeminiContent>,
    #[allow(dead_code)]
    pub finish_reason: Option<String>,
}

#[allow(dead_code, clippy::struct_field_names)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GeminiUsageMetadata {
    pub prompt_token_count: Option<u32>,
    pub candidates_token_count: Option<u32>,
    pub total_token_count: Option<u32>,
}

impl GeminiResponse {
    pub fn get_text(&self) -> Option<String> {
        self.candidates
            .as_ref()?
            .first()?
            .content
            .as_ref()?
            .parts
            .first()?
            .text
            .clone()
    }

    pub fn into_message(self) -> Message {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let mut content_blocks = Vec::new();

        if let Some(content) = self
            .candidates
            .and_then(|mut c| c.pop())
            .and_then(|c| c.content)
        {
            for part in content.parts {
                if let Some(text) = part.text {
                    content_blocks.push(ContentBlock::Text { text });
                }
                if let Some(fc) = part.function_call {
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_millis())
                        .unwrap_or(0);
                    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
                    let id = format!("call_{}_{ts}_{count}", fc.name);

                    content_blocks.push(ContentBlock::ToolCall {
                        id,
                        name: fc.name,
                        arguments: fc.args,
                    });
                }
            }
        }

        Message {
            role: Role::Assistant,
            content: Arc::new(content_blocks),
        }
    }
}
