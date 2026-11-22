//! Read-only account telemetry for a bound official provider route.
use std::{sync::Arc, time::Duration};

use reqwest::{Client, Url};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{CredentialResolver, CredentialStore, Selection};

const DEEPSEEK_PROVIDER: &str = "deepseek";
const DEEPSEEK_BALANCE: &str = "https://api.deepseek.com/user/balance";
const MAX_RESPONSE_BYTES: usize = 16 * 1024;

pub struct ProviderBalance {
    credentials: Arc<dyn CredentialResolver>,
}

#[derive(Debug, Deserialize)]
pub struct Balance {
    pub is_available: bool,
    pub balance_infos: Vec<BalanceAmount>,
}

#[derive(Debug, Deserialize)]
pub struct BalanceAmount {
    pub currency: Currency,
    pub total_balance: String,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
pub enum Currency {
    #[serde(rename = "USD")]
    Usd,
    #[serde(rename = "CNY")]
    Cny,
}

impl ProviderBalance {
    pub fn provider(&self) -> &'static str {
        // A balance source today is only constructed for the official DeepSeek route.
        DEEPSEEK_PROVIDER
    }

    pub(crate) fn for_selection(
        selection: &Selection,
        store: &CredentialStore,
    ) -> Option<Arc<Self>> {
        if selection.provider != DEEPSEEK_PROVIDER || !official_deepseek(&selection.endpoint) {
            return None;
        }
        let credentials = store
            .resolver(&selection.provider, selection.api_key_env.as_deref())
            .ok()?;
        Some(Arc::new(Self { credentials }))
    }

    /// No inference request, redirect, automatic retry, or unbounded body read.
    /// Failures yield no displayable balance, and response bodies are never logged.
    pub async fn read(&self) -> Option<Balance> {
        let key = self
            .credentials
            .resolve(CancellationToken::new())
            .await
            .ok()??;
        read_deepseek(&balance_client()?, DEEPSEEK_BALANCE, &key).await
    }
}

fn balance_client() -> Option<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(5))
        .build()
        .ok()
}

fn official_deepseek(endpoint: &str) -> bool {
    Url::parse(endpoint).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some("api.deepseek.com")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

async fn read_deepseek(client: &Client, endpoint: &str, key: &str) -> Option<Balance> {
    let mut response = client.get(endpoint).bearer_auth(key).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if chunk.len() > MAX_RESPONSE_BYTES - bytes.len() {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    parse_deepseek(&bytes)
}

fn parse_deepseek(bytes: &[u8]) -> Option<Balance> {
    let balance: Balance = serde_json::from_slice(bytes).ok()?;
    let amounts = &balance.balance_infos;
    if amounts.is_empty()
        || amounts.len() > 2
        || (amounts.len() == 2 && amounts[0].currency == amounts[1].currency)
    {
        return None;
    }
    amounts
        .iter()
        .all(|amount| decimal(&amount.total_balance))
        .then_some(balance)
}

fn decimal(value: &str) -> bool {
    if value.len() > 32 {
        return false;
    }
    let unsigned = value.strip_prefix('-').unwrap_or(value);
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    unsigned
        .split_once('.')
        .map_or_else(|| digits(unsigned), |(a, b)| digits(a) && digits(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn authenticated_balance_request_rejects_redirects_and_oversized_bodies() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serve = tokio::spawn(async move {
            for (status, headers, body) in [
                ("200 OK", String::new(), br#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"5.00"}]}"#.to_vec()),
                ("302 Found", format!("Location: http://{addr}/must-not-follow\r\n"), Vec::new()),
                ("200 OK", String::new(), vec![b' '; MAX_RESPONSE_BYTES + 1]),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 1024);
                    request.push(socket.read_u8().await.unwrap());
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with("GET /user/balance HTTP/1.1\r\n"));
                assert!(request.to_ascii_lowercase().contains("authorization: bearer fixture-key\r\n"));
                let head = format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                socket.write_all(head.as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
            }
        });
        let client = balance_client().unwrap();
        let endpoint = format!("http://{addr}/user/balance");
        let read = || read_deepseek(&client, &endpoint, "fixture-key");
        assert_eq!(read().await.unwrap().balance_infos[0].total_balance, "5.00");
        assert!(read().await.is_none());
        assert!(read().await.is_none());
        serve.await.unwrap();
    }

    #[test]
    fn restricts_account_credentials_to_the_official_origin() {
        assert!(official_deepseek(
            "https://api.deepseek.com/v1/chat/completions"
        ));
        for route in [
            "http://api.deepseek.com",
            "https://api.deepseek.com:8443",
            "https://api.deepseek.com.example.net",
            "https://alice@api.deepseek.com",
            "http://desktop:8080",
        ] {
            assert!(!official_deepseek(route), "{route}");
        }
    }

    #[test]
    fn accepts_exact_reported_balances_but_not_duplicate_currencies_or_remote_text() {
        let report =
            |a: &str, b: &str| format!(r#"{{"is_available":false,"balance_infos":[{a}{b}]}}"#);
        let usd = r#"{"currency":"USD","total_balance":"0.00005"}"#;
        let parsed = parse_deepseek(report(usd, "").as_bytes()).unwrap();
        assert!(!parsed.is_available);
        assert_eq!(parsed.balance_infos[0].total_balance, "0.00005");
        assert!(parse_deepseek(report(usd, &format!(",{usd}")).as_bytes()).is_none());
        assert!(
            parse_deepseek(
                report(r#"{"currency":"USD","total_balance":"12\u001b[2J"}"#, "").as_bytes()
            )
            .is_none()
        );
    }
}
