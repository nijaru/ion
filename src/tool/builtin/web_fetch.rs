use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

pub struct WebFetchTool {
    client: Client,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebFetchTool {
    pub fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent("ion/0.0.0")
            .build()
            .expect("Failed to create HTTP client");

        Self { client }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch content from a URL. Returns the response body as text."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                },
                "max_length": {
                    "type": "integer",
                    "description": "Maximum response length in bytes (default: 100000)"
                }
            },
            "required": ["url"]
        })
    }

    fn danger_level(&self) -> DangerLevel {
        // Network access has security implications - requires approval
        DangerLevel::Restricted
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArgs("url is required".to_string()))?;

        let max_length = args
            .get("max_length")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(100_000);

        // Validate URL
        let parsed_url = reqwest::Url::parse(url)
            .map_err(|e| ToolError::InvalidArgs(format!("Invalid URL: {}", e)))?;

        // Only allow http/https
        match parsed_url.scheme() {
            "http" | "https" => {}
            scheme => {
                return Err(ToolError::InvalidArgs(format!(
                    "Unsupported URL scheme: {}. Only http and https are allowed.",
                    scheme
                )));
            }
        }

        // Make the request
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Request failed: {}", e)))?;

        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();

        if !status.is_success() {
            return Ok(ToolResult {
                content: format!("HTTP {} {}", status.as_u16(), status.canonical_reason().unwrap_or("")),
                is_error: true,
                metadata: Some(json!({
                    "status": status.as_u16(),
                    "content_type": content_type
                })),
            });
        }

        // Read body with limit
        let bytes = response
            .bytes()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read response: {}", e)))?;

        // Try to convert to string first, then truncate at char boundary
        let content = match String::from_utf8(bytes.to_vec()) {
            Ok(text) => {
                if text.len() > max_length {
                    // Find char boundary for clean truncation
                    let truncate_at = text
                        .char_indices()
                        .take_while(|(i, _)| *i < max_length)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(max_length);
                    format!(
                        "{}\n\n[Truncated: {} bytes total]",
                        &text[..truncate_at],
                        text.len()
                    )
                } else {
                    text
                }
            }
            Err(_) => {
                format!(
                    "[Binary content: {} bytes, content-type: {}]",
                    bytes.len(),
                    content_type
                )
            }
        };
        let truncated = bytes.len() > max_length;

        Ok(ToolResult {
            content,
            is_error: false,
            metadata: Some(json!({
                "status": status.as_u16(),
                "content_type": content_type,
                "length": bytes.len(),
                "truncated": truncated
            })),
        })
    }
}
