//! Read-only provider balance and key usage for a bound official route.
use std::{sync::Arc, time::Duration};

use reqwest::{Client, Url};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{CredentialResolver, CredentialStore, Selection};

const DEEPSEEK_BALANCE: &str = "https://api.deepseek.com/user/balance";
const OPENROUTER_KEY: &str = "https://openrouter.ai/api/v1/key";
const MAX_RESPONSE_BYTES: usize = 16 * 1024;

pub struct ProviderUsage {
    kind: ProviderUsageKind,
    credentials: Arc<dyn CredentialResolver>,
}

#[derive(Clone, Copy)]
pub enum ProviderUsageKind {
    DeepSeekBalance,
    OpenRouterKey,
}

impl ProviderUsageKind {
    fn provider(self) -> &'static str {
        match self {
            Self::DeepSeekBalance => "deepseek",
            Self::OpenRouterKey => "openrouter",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::DeepSeekBalance => DEEPSEEK_BALANCE,
            Self::OpenRouterKey => OPENROUTER_KEY,
        }
    }
}

pub enum ProviderUsageReading {
    Balance(Balance),
    KeyCapRemaining(serde_json::Number),
    KeySpend(serde_json::Number),
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

impl ProviderUsage {
    pub fn provider(&self) -> &'static str {
        self.kind.provider()
    }

    pub fn kind(&self) -> ProviderUsageKind {
        self.kind
    }

    pub(crate) fn for_selection(
        selection: &Selection,
        store: &CredentialStore,
    ) -> Option<Arc<Self>> {
        let kind = match selection.provider.as_str() {
            "deepseek" if official_origin(&selection.endpoint, "api.deepseek.com") => {
                ProviderUsageKind::DeepSeekBalance
            }
            "openrouter" if official_origin(&selection.endpoint, "openrouter.ai") => {
                ProviderUsageKind::OpenRouterKey
            }
            _ => return None,
        };
        let credentials = store
            .resolver(&selection.provider, selection.api_key_env.as_deref())
            .ok()?;
        Some(Arc::new(Self { kind, credentials }))
    }

    /// No inference request, redirect, automatic retry, or unbounded body read.
    /// Failures yield no displayable balance, and response bodies are never logged.
    pub async fn read(&self) -> Option<ProviderUsageReading> {
        let key = self
            .credentials
            .resolve(CancellationToken::new())
            .await
            .ok()??;
        let bytes = read_usage(&usage_client()?, self.kind.endpoint(), &key).await?;
        match self.kind {
            ProviderUsageKind::DeepSeekBalance => {
                parse_deepseek(&bytes).map(ProviderUsageReading::Balance)
            }
            ProviderUsageKind::OpenRouterKey => parse_openrouter(&bytes),
        }
    }
}

fn usage_client() -> Option<Client> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(5))
        .build()
        .ok()
}

fn official_origin(endpoint: &str, host: &str) -> bool {
    Url::parse(endpoint).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str() == Some(host)
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
    })
}

async fn read_usage(client: &Client, endpoint: &str, key: &str) -> Option<Vec<u8>> {
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
    Some(bytes)
}

fn parse_openrouter(bytes: &[u8]) -> Option<ProviderUsageReading> {
    let payload: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let data = payload.get("data")?.as_object()?;
    let amount = |name| {
        let number = data.get(name)?.as_number()?;
        (number.as_f64()? >= 0.0).then(|| number.clone())
    };
    // A null or missing cap never implies zero remaining or account credit.
    // Use the provider-reported remainder, not limit minus lifetime spend.
    if amount("limit").is_some()
        && let Some(remaining) = data
            .get("limit_remaining")
            .and_then(serde_json::Value::as_number)
    {
        return Some(ProviderUsageReading::KeyCapRemaining(remaining.clone()));
    }
    amount("usage").map(ProviderUsageReading::KeySpend)
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
    async fn authenticated_usage_requests_reject_redirects_and_oversized_bodies() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let serve = tokio::spawn(async move {
            for (path, status, headers, body) in [
                ("/user/balance", "200 OK", String::new(), br#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"5.00"}]}"#.to_vec()),
                ("/api/v1/key", "200 OK", String::new(), br#"{"data":{"limit":50,"limit_remaining":17.5}}"#.to_vec()),
                ("/user/balance", "302 Found", format!("Location: http://{addr}/must-not-follow\r\n"), Vec::new()),
                ("/user/balance", "200 OK", String::new(), vec![b' '; MAX_RESPONSE_BYTES + 1]),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 1024);
                    request.push(socket.read_u8().await.unwrap());
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with(&format!("GET {path} HTTP/1.1\r\n")));
                assert!(request.to_ascii_lowercase().contains("authorization: bearer fixture-key\r\n"));
                let head = format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                socket.write_all(head.as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
            }
        });
        let client = usage_client().unwrap();
        let endpoint = format!("http://{addr}/user/balance");
        let read = || read_usage(&client, &endpoint, "fixture-key");
        let body = read().await.unwrap();
        assert_eq!(
            parse_deepseek(&body).unwrap().balance_infos[0].total_balance,
            "5.00"
        );
        let key_endpoint = format!(
            "http://{addr}{}",
            Url::parse(ProviderUsageKind::OpenRouterKey.endpoint())
                .unwrap()
                .path()
        );
        let body = read_usage(&client, &key_endpoint, "fixture-key")
            .await
            .unwrap();
        assert!(
            matches!(parse_openrouter(&body), Some(ProviderUsageReading::KeyCapRemaining(n)) if n.as_f64() == Some(17.5))
        );
        assert!(read().await.is_none());
        assert!(read().await.is_none());
        serve.await.unwrap();
    }

    #[test]
    fn restricts_account_credentials_to_the_official_origin() {
        assert!(official_origin(
            "https://api.deepseek.com/v1/chat/completions",
            "api.deepseek.com"
        ));
        assert!(official_origin(
            "https://openrouter.ai/api/v1/chat/completions",
            "openrouter.ai"
        ));
        assert!(!official_origin(
            "https://openrouter.ai.example.com/api/v1",
            "openrouter.ai"
        ));
        assert!(!official_origin(
            "http://127.0.0.1:8080/v1",
            "openrouter.ai"
        ));
        for route in [
            "http://api.deepseek.com",
            "https://api.deepseek.com:8443",
            "https://api.deepseek.com.example.net",
            "https://alice@api.deepseek.com",
            "http://desktop:8080",
        ] {
            assert!(!official_origin(route, "api.deepseek.com"), "{route}");
        }
    }

    #[test]
    fn openrouter_reports_key_cap_or_spend_without_inventing_account_credit() {
        let parse = |data| parse_openrouter(format!(r#"{{"data":{data}}}"#).as_bytes());
        let reading = parse(
            r#"{"limit":50,"limit_remaining":17.5,"usage":100,"effective_budget":{"remaining_usd":3}}"#,
        );
        assert!(
            matches!(reading, Some(ProviderUsageReading::KeyCapRemaining(n)) if n.as_f64() == Some(17.5))
        );
        let exhausted = parse(r#"{"limit":50,"limit_remaining":-0.25,"usage":51}"#);
        assert!(
            matches!(exhausted, Some(ProviderUsageReading::KeyCapRemaining(n)) if n.as_f64() == Some(-0.25))
        );
        let uncapped = parse(r#"{"limit":null,"limit_remaining":0,"usage":2.75}"#);
        assert!(
            matches!(uncapped, Some(ProviderUsageReading::KeySpend(n)) if n.as_f64() == Some(2.75))
        );
        assert!(parse(r#"{"limit":null,"credits":500}"#).is_none());
        assert!(parse(r#"{"limit":20,"limit_remaining":"untrusted","usage":-1}"#).is_none());
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
