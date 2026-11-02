//! Validated, secret-safe header configuration captured before HTTP I/O.
#[cfg(test)]
use super::values::MAX_CONFIGURED_BYTES;
use super::{
    config::McpHttpServer,
    values::{self, McpValue},
};
use anyhow::{Result, ensure};
use reqwest::header::{AUTHORIZATION, HeaderName, HeaderValue};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use std::{
    collections::{BTreeMap, HashSet},
    ffi::{OsStr, OsString},
    path::Path,
};

pub(super) fn validate(server: &McpHttpServer) -> Result<()> {
    let mut names = HashSet::new();
    for (name, value) in &server.headers {
        let name = configured_name(name)?;
        ensure!(
            server.bearer_token_env.is_none() || name != AUTHORIZATION,
            "MCP Authorization header conflicts with bearer_token_env"
        );
        ensure!(names.insert(name), "duplicate MCP HTTP header name");
        value.validate()?;
        if let McpValue::Text(value) = value {
            header_value(value)?;
        }
    }
    Ok(())
}

pub(super) async fn configuration(
    mut server: McpHttpServer,
    inherited: &BTreeMap<OsString, OsString>,
    cwd: &Path,
    stop: tokio_util::sync::CancellationToken,
) -> Result<StreamableHttpClientTransportConfig> {
    validate(&server)?;
    let mut config = StreamableHttpClientTransportConfig::with_uri(server.url);
    if let Some(name) = server.bearer_token_env {
        ensure!(
            inherited
                .get(OsStr::new(&name))
                .is_some_and(|value| !value.is_empty()),
            "MCP environment needs {name}"
        );
        // One typed header owner also keeps bearer credentials out of the SDK's
        // plain String auth_header Debug output. Both forms have identical I/O.
        server.headers.insert(
            AUTHORIZATION.to_string(),
            format!("Bearer ${{{name}}}").into(),
        );
    }
    let headers = values::resolve(&server.headers, inherited, cwd, stop, |value| {
        header_value(value).map(|_| ())
    })
    .await?;
    for (name, value) in headers {
        config
            .custom_headers
            .insert(configured_name(&name)?, header_value(&value)?);
    }
    Ok(config)
}

fn header_value(value: &str) -> Result<HeaderValue> {
    let mut value = HeaderValue::from_str(value)
        .map_err(|_| anyhow::anyhow!("invalid MCP HTTP header value"))?;
    // Custom names can contain credentials just as Authorization can.
    value.set_sensitive(true);
    Ok(value)
}

fn configured_name(name: &str) -> Result<HeaderName> {
    let name = HeaderName::from_bytes(name.as_bytes())
        .map_err(|_| anyhow::anyhow!("invalid MCP HTTP header name"))?;
    // HTTP framing and MCP routing/session fields belong to the transport.
    ensure!(
        ![
            "accept",
            "accept-encoding",
            "connection",
            "content-length",
            "content-type",
            "expect",
            "host",
            "last-event-id",
            "mcp-method",
            "mcp-name",
            "mcp-protocol-version",
            "mcp-session-id",
            "te",
            "trailer",
            "transfer-encoding",
            "upgrade",
        ]
        .contains(&name.as_str())
            && !name.as_str().starts_with("mcp-param-"),
        "reserved MCP HTTP header {name}"
    );
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::McpExposure;

    fn server() -> McpHttpServer {
        McpHttpServer {
            enabled: true,
            description: None,
            url: "https://example.test/mcp".into(),
            bearer_token_env: None,
            headers: BTreeMap::new(),
            exposure: McpExposure::Direct,
            tool_exposure: Default::default(),
        }
    }

    #[tokio::test]
    async fn headers_and_bearer_share_bounded_sensitive_capture() {
        let inherited = BTreeMap::from([("TOKEN".into(), "secret${UNSET}".into())]);
        let mut server = server();
        server.bearer_token_env = Some("TOKEN".into());
        server.headers.insert("X-Key".into(), "${TOKEN}".into());
        server.headers.insert("X-Empty".into(), "${UNSET:-}".into());
        let config = configuration(
            server.clone(),
            &inherited,
            Path::new("."),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(config.auth_header.is_none());
        assert_eq!(
            config.custom_headers[&AUTHORIZATION],
            "Bearer secret${UNSET}"
        );
        assert_eq!(
            config.custom_headers[&HeaderName::from_static("x-key")],
            "secret${UNSET}"
        );
        assert_eq!(
            config.custom_headers[&HeaderName::from_static("x-empty")],
            ""
        );
        assert!(
            config
                .custom_headers
                .values()
                .all(HeaderValue::is_sensitive)
        );
        assert!(!format!("{config:?}").contains("secret"));
        assert!(
            configuration(
                server.clone(),
                &BTreeMap::new(),
                Path::new("."),
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert!(
            configuration(
                server.clone(),
                &BTreeMap::from([("TOKEN".into(), "".into())]),
                Path::new("."),
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
        );
        let injected = BTreeMap::from([("TOKEN".into(), "secret\r\nInjected: yes".into())]);
        let error = configuration(
            server.clone(),
            &injected,
            Path::new("."),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(!format!("{error:#}").contains("secret"));
        server.bearer_token_env = None;
        server.headers = BTreeMap::from([
            ("A".into(), "a".repeat(MAX_CONFIGURED_BYTES / 2 - 1).into()),
            ("B".into(), "b".repeat(MAX_CONFIGURED_BYTES / 2 - 1).into()),
        ]);
        assert!(
            configuration(
                server.clone(),
                &inherited,
                Path::new("."),
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_ok()
        );
        server.headers.insert("C".into(), "".into());
        assert!(
            configuration(
                server,
                &inherited,
                Path::new("."),
                tokio_util::sync::CancellationToken::new()
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn framing_collisions_and_ambiguous_authorization_are_rejected() {
        for name in [
            "Accept",
            "Host",
            "Content-Type",
            "Content-Length",
            "Transfer-Encoding",
            "Mcp-Session-Id",
            "MCP-Protocol-Version",
            "Last-Event-Id",
            "Mcp-Method",
            "Mcp-Name",
            "Mcp-Param-Cursor",
            "bad name",
            "",
        ] {
            assert!(configured_name(name).is_err(), "{name}");
        }
        let mut server = server();
        server.headers.insert("X-Key".into(), "one".into());
        server.headers.insert("x-key".into(), "two".into());
        assert!(validate(&server).is_err());
        server.headers = BTreeMap::from([("Authorization".into(), "Basic abc".into())]);
        assert!(validate(&server).is_ok());
        server.bearer_token_env = Some("TOKEN".into());
        assert!(validate(&server).is_err());
        server.bearer_token_env = None;
        server.headers = BTreeMap::from([("X-Key".into(), "secret\nvalue".into())]);
        assert!(validate(&server).is_err());
    }
}
