//! HTTP client wrapper for LLM API requests.

use crate::provider::error::Error;
use bytes::Bytes;
use futures::Stream;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER};
use serde::{Serialize, de::DeserializeOwned};
use std::time::Duration;

/// HTTP request timeout.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Connection timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Authentication configuration.
#[derive(Clone)]
pub enum AuthConfig {
    /// Bearer token authentication (Authorization: Bearer {token}).
    Bearer(String),
    /// Custom header authentication (e.g., x-api-key: {key}).
    ApiKey { header: String, key: String },
}

impl std::fmt::Debug for AuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bearer(_) => f.debug_tuple("Bearer").field(&"[REDACTED]").finish(),
            Self::ApiKey { header, .. } => f
                .debug_struct("ApiKey")
                .field("header", header)
                .field("key", &"[REDACTED]")
                .finish(),
        }
    }
}

/// HTTP client for LLM API requests.
#[derive(Debug)]
pub struct HttpClient {
    client: reqwest::Client,
    base_url: String,
    auth: AuthConfig,
}

impl HttpClient {
    /// Create a new HTTP client.
    pub fn new(base_url: impl Into<String>, auth: AuthConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            base_url: base_url.into(),
            auth,
        }
    }

    /// Build headers including authentication.
    fn build_headers(&self) -> Result<HeaderMap, Error> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        match &self.auth {
            AuthConfig::Bearer(token) => {
                let value = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| Error::Api("Bearer token contains invalid header characters".into()))?;
                headers.insert(AUTHORIZATION, value);
            }
            AuthConfig::ApiKey { header, key } => {
                let name = reqwest::header::HeaderName::try_from(header)
                    .map_err(|_| Error::Api("API key header name is invalid".into()))?;
                let value = HeaderValue::from_str(key)
                    .map_err(|_| Error::Api("API key contains invalid header characters".into()))?;
                headers.insert(name, value);
            }
        }

        Ok(headers)
    }

    /// Make a POST request with JSON body and deserialize the response.
    pub async fn post_json<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, Error> {
        let url = format!("{}{path}", self.base_url);
        let headers = self.build_headers()?;

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(body)
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = parse_retry_after(&response);
            return Err(Error::RateLimited { retry_after });
        }
        let text = response.text().await?;

        if !status.is_success() {
            return Err(Error::Api(format!("HTTP {status}: {text}")));
        }

        serde_json::from_str(&text)
            .map_err(|e| Error::Api(format!("Failed to parse response: {e}\nBody: {text}")))
    }

    /// Make a POST request for streaming response.
    ///
    /// Automatically sets `Accept: text/event-stream` for SSE compatibility.
    pub async fn post_stream<T: Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<impl Stream<Item = Result<Bytes, reqwest::Error>>, Error> {
        let url = format!("{}{path}", self.base_url);
        let mut headers = self.build_headers()?;
        headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(body)
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = parse_retry_after(&response);
            return Err(Error::RateLimited { retry_after });
        }
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Api(format!("HTTP {status}: {text}")));
        }

        Ok(response.bytes_stream())
    }

    /// Add extra headers to subsequent requests.
    /// Returns a new client with additional default headers.
    pub fn with_extra_headers(self, extra: HeaderMap) -> Self {
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .default_headers(extra)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            base_url: self.base_url,
            auth: self.auth,
        }
    }
}

/// Extract and parse `Retry-After` header from a response.
fn parse_retry_after(response: &reqwest::Response) -> Option<u64> {
    let value = response.headers().get(RETRY_AFTER)?;
    let s = value.to_str().ok()?;
    parse_retry_after_value(s)
}

/// Parse a `Retry-After` header value as seconds.
///
/// Handles integer and fractional seconds (rounds up). Ignores HTTP-date
/// format and non-finite values — returns None.
fn parse_retry_after_value(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Ok(secs) = s.parse::<u64>() {
        Some(secs.max(1))
    } else if let Ok(f) = s.parse::<f64>() {
        if f.is_finite() && f > 0.0 {
            Some((f.ceil() as u64).max(1))
        } else {
            None
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bearer_auth() {
        let client = HttpClient::new(
            "https://api.example.com",
            AuthConfig::Bearer("test-token".into()),
        );
        let headers = client.build_headers().unwrap();
        assert_eq!(headers.get(AUTHORIZATION).unwrap(), "Bearer test-token");
    }

    #[test]
    fn test_api_key_auth() {
        let client = HttpClient::new(
            "https://api.example.com",
            AuthConfig::ApiKey {
                header: "x-api-key".into(),
                key: "secret".into(),
            },
        );
        let headers = client.build_headers().unwrap();
        assert_eq!(headers.get("x-api-key").unwrap(), "secret");
    }

    #[test]
    fn test_parse_retry_after_integer() {
        assert_eq!(parse_retry_after_value("30"), Some(30));
        assert_eq!(parse_retry_after_value("1"), Some(1));
        assert_eq!(parse_retry_after_value("120"), Some(120));
    }

    #[test]
    fn test_parse_retry_after_fractional() {
        assert_eq!(parse_retry_after_value("2.5"), Some(3)); // ceil
        assert_eq!(parse_retry_after_value("0.1"), Some(1)); // ceil + max(1)
        assert_eq!(parse_retry_after_value("59.9"), Some(60));
    }

    #[test]
    fn test_parse_retry_after_zero() {
        // Integer 0 → clamped to 1
        assert_eq!(parse_retry_after_value("0"), Some(1));
    }

    #[test]
    fn test_parse_retry_after_invalid() {
        assert_eq!(parse_retry_after_value(""), None);
        assert_eq!(
            parse_retry_after_value("Thu, 01 Jan 2026 00:00:00 GMT"),
            None
        );
        assert_eq!(parse_retry_after_value("abc"), None);
        assert_eq!(parse_retry_after_value("-1"), None); // negative integer parses as i64, not u64
        assert_eq!(parse_retry_after_value("-1.5"), None); // negative float, filtered
        assert_eq!(parse_retry_after_value("Infinity"), None);
        assert_eq!(parse_retry_after_value("NaN"), None);
    }

    #[test]
    fn test_parse_retry_after_whitespace() {
        assert_eq!(parse_retry_after_value("  30  "), Some(30));
    }
}
