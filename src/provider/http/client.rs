//! HTTP client wrapper for LLM API requests.

use crate::provider::error::Error;
use bytes::Bytes;
use futures::Stream;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
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
    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        match &self.auth {
            AuthConfig::Bearer(token) => {
                let value = HeaderValue::from_str(&format!("Bearer {token}"))
                    .expect("Bearer token contains invalid header characters");
                headers.insert(AUTHORIZATION, value);
            }
            AuthConfig::ApiKey { header, key } => {
                let name = reqwest::header::HeaderName::try_from(header)
                    .expect("API key header name is invalid");
                let value =
                    HeaderValue::from_str(key).expect("API key contains invalid header characters");
                headers.insert(name, value);
            }
        }

        headers
    }

    /// Make a POST request with JSON body and deserialize the response.
    pub async fn post_json<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, Error> {
        let url = format!("{}{path}", self.base_url);
        let headers = self.build_headers();

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(body)
            .send()
            .await?;

        let status = response.status();
        let text = response.text().await?;

        if !status.is_success() {
            return Err(Error::Api(format!("HTTP {status}: {text}")));
        }

        serde_json::from_str(&text)
            .map_err(|e| Error::Api(format!("Failed to parse response: {e}\nBody: {text}")))
    }

    /// Make a POST request for streaming response.
    pub async fn post_stream<T: Serialize>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<impl Stream<Item = Result<Bytes, reqwest::Error>>, Error> {
        let url = format!("{}{path}", self.base_url);
        let headers = self.build_headers();

        let response = self
            .client
            .post(&url)
            .headers(headers)
            .json(body)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(Error::Api(format!("HTTP {status}: {text}")));
        }

        Ok(response.bytes_stream())
    }

    /// Add extra headers to subsequent requests.
    /// Returns a new client with additional headers.
    pub fn with_extra_headers(self, extra: HeaderMap) -> Self {
        let mut headers = self.build_headers();
        headers.extend(extra);

        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .default_headers(headers)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            client,
            base_url: self.base_url,
            auth: self.auth,
        }
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
        let headers = client.build_headers();
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
        let headers = client.build_headers();
        assert_eq!(headers.get("x-api-key").unwrap(), "secret");
    }
}
