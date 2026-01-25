use crate::tool::{DangerLevel, Tool, ToolContext, ToolError, ToolResult};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use std::time::Duration;

/// Check if URL points to private/internal addresses (SSRF protection).
fn is_private_or_internal(url: &reqwest::Url) -> bool {
    match url.host_str() {
        Some(host) => {
            // Check domain names
            if host == "localhost"
                || host.ends_with(".local")
                || host.ends_with(".internal")
                || host == "metadata.google.internal"
            {
                return true;
            }

            // Try to parse as IP address
            if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                return match ip {
                    std::net::IpAddr::V4(ipv4) => {
                        ipv4.is_loopback()       // 127.0.0.0/8
                            || ipv4.is_private() // 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16
                            || ipv4.is_link_local() // 169.254.0.0/16 (AWS/cloud metadata)
                            || ipv4.is_broadcast()
                            || ipv4.is_unspecified()
                    }
                    std::net::IpAddr::V6(ipv6) => {
                        ipv6.is_loopback() || ipv6.is_unspecified() || is_ipv6_private(&ipv6)
                    }
                };
            }

            false
        }
        None => true, // Block if no host
    }
}

/// Check if IPv6 address is private/internal.
fn is_ipv6_private(ip: &std::net::Ipv6Addr) -> bool {
    // Convert to check if it's an IPv4-mapped address
    if let Some(ipv4) = ip.to_ipv4_mapped() {
        return ipv4.is_loopback() || ipv4.is_private() || ipv4.is_link_local();
    }

    let segments = ip.segments();

    // Unique local (fc00::/7)
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }

    // Link-local (fe80::/10)
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }

    false
}

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

        // Block private/internal IPs (SSRF protection)
        if is_private_or_internal(&parsed_url) {
            return Err(ToolError::InvalidArgs(
                "Cannot fetch private/internal URLs (localhost, private IPs, link-local)".to_string(),
            ));
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

        // Check Content-Length to reject obviously huge responses early
        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok());

        // Stream body with size limit (don't load entire response into memory)
        let read_limit = max_length + 1; // Read 1 extra byte to detect truncation
        let mut bytes = Vec::with_capacity(read_limit.min(content_length.unwrap_or(read_limit)));
        let mut stream = response.bytes_stream();

        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| ToolError::ExecutionFailed(format!("Failed to read response: {}", e)))?;
            let remaining = read_limit.saturating_sub(bytes.len());
            if remaining == 0 {
                break;
            }
            let take = chunk.len().min(remaining);
            bytes.extend_from_slice(&chunk[..take]);
        }

        let was_truncated = bytes.len() > max_length;
        if was_truncated {
            bytes.truncate(max_length);
        }

        // Try to convert to string, truncate at char boundary if needed
        let (content, truncated) = match String::from_utf8(bytes.clone()) {
            Ok(mut text) => {
                if was_truncated {
                    // Find last valid char boundary
                    let truncate_at = text
                        .char_indices()
                        .take_while(|(i, _)| *i < max_length)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(text.len());
                    text.truncate(truncate_at);
                    let total = content_length.map_or("unknown".to_string(), |l| l.to_string());
                    (format!("{}\n\n[Truncated: {} bytes total]", text, total), true)
                } else {
                    (text, false)
                }
            }
            Err(_) => {
                let total = content_length.unwrap_or(bytes.len());
                (
                    format!(
                        "[Binary content: {} bytes, content-type: {}]",
                        total, content_type
                    ),
                    false,
                )
            }
        };

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
