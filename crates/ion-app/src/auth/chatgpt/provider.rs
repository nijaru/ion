//! Pinned OpenAI transport. A rotating refresh is never automatically retried;
//! revoking a particular token is idempotent and has its own retry policy.
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use url::Url;

use super::{ISSUER, JWKS, RESOURCE, SignInError, TOKEN, identity, read_json, visible_ascii};

pub(super) const DISCOVERY: &str = "https://auth.openai.com/.well-known/openid-configuration";

pub(super) struct Provider<'a> {
    pub(super) token: &'a str,
    pub(super) jwks: &'a str,
    pub(super) discovery: &'a str,
}

pub(super) const OPENAI: Provider<'static> = Provider {
    token: TOKEN,
    jwks: JWKS,
    discovery: DISCOVERY,
};

#[derive(Deserialize)]
pub(super) struct OAuthError {
    pub(super) error: String,
}

pub(super) enum RefreshError {
    Unusable,
    InvalidClient,
    Unknown,
}

impl Provider<'_> {
    pub(super) async fn identity(
        &self,
        client: &reqwest::Client,
        signed: &str,
        client_id: &str,
        nonce: Option<&str>,
    ) -> Result<identity::Identity, SignInError> {
        let keys = client
            .get(self.jwks)
            .send()
            .await
            .map_err(|_| SignInError::Network)?;
        if keys.status() != reqwest::StatusCode::OK {
            return Err(SignInError::Network);
        }
        let keys = read_json(keys, SignInError::Network).await?;
        identity::verify(
            signed,
            keys,
            client_id,
            nonce,
            unix_now().map_err(|_| SignInError::Identity)?,
        )
    }

    pub(super) async fn refresh(
        &self,
        client: &reqwest::Client,
        client_id: &str,
        old: &TokenSet,
    ) -> Result<(TokenSet, Option<String>), RefreshError> {
        let reply = form(
            client,
            self.token,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", client_id),
                ("refresh_token", &old.refresh_token),
                ("resource", RESOURCE),
            ],
        )
        .send()
        .await
        .map_err(|_| RefreshError::Unknown)?;
        if reply.status() != reqwest::StatusCode::OK {
            if matches!(reply.status().as_u16(), 400 | 401)
                && let Ok(body) =
                    read_json::<OAuthError>(reply, SignInError::ExchangeRejected).await
            {
                match body.error.as_str() {
                    "invalid_grant"
                    | "invalid_refresh_token"
                    | "token_expired"
                    | "refresh_token_expired"
                    | "refresh_token_invalidated"
                    | "refresh_token_reused" => return Err(RefreshError::Unusable),
                    "invalid_client" => return Err(RefreshError::InvalidClient),
                    _ => {}
                }
            }
            return Err(RefreshError::Unknown);
        }
        let received = unix_now().map_err(|_| RefreshError::Unknown)?;
        let reply: TokenReply = read_json(reply, SignInError::TokenResponse)
            .await
            .map_err(|_| RefreshError::Unknown)?;
        let (tokens, id) = reply
            .into_tokens(received, Some(&old.scope))
            .map_err(|_| RefreshError::Unknown)?;
        if tokens.refresh_token == old.refresh_token {
            return Err(RefreshError::Unknown);
        }
        Ok((tokens, id))
    }

    pub(super) async fn revoke(
        &self,
        client: &reqwest::Client,
        client_id: &str,
        refresh: &str,
    ) -> bool {
        #[derive(Deserialize)]
        struct Metadata {
            issuer: String,
            revocation_endpoint: String,
        }

        let Ok(reply) = client.get(self.discovery).send().await else {
            return false;
        };
        if reply.status() != reqwest::StatusCode::OK {
            return false;
        }
        let Ok(metadata) = read_json::<Metadata>(reply, SignInError::Network).await else {
            return false;
        };
        let Ok(discovery) = Url::parse(self.discovery) else {
            return false;
        };
        let Ok(endpoint) = Url::parse(&metadata.revocation_endpoint) else {
            return false;
        };
        if metadata.issuer != ISSUER
            || endpoint.origin() != discovery.origin()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.fragment().is_some()
        {
            return false;
        }
        let fields = [
            ("token", refresh),
            ("token_type_hint", "refresh_token"),
            ("client_id", client_id),
        ];
        let send = || {
            form(client, endpoint.as_str(), &fields)
                .timeout(Duration::from_secs(10))
                .send()
        };
        match send().await {
            Ok(response) if response.status() == reqwest::StatusCode::OK => {
                empty_confirmation(response).await
            }
            Ok(response) if !response.status().is_server_error() => false,
            _ => {
                tokio::time::sleep(Duration::from_millis(300)).await;
                match send().await {
                    Ok(response) if response.status() == reqwest::StatusCode::OK => {
                        empty_confirmation(response).await
                    }
                    _ => false,
                }
            }
        }
    }
}

async fn empty_confirmation(mut response: reqwest::Response) -> bool {
    matches!(response.chunk().await, Ok(None))
}

pub(super) fn client() -> Result<reqwest::Client, SignInError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| SignInError::Network)
}

pub(super) fn form(
    client: &reqwest::Client,
    endpoint: &str,
    pairs: &[(&str, &str)],
) -> reqwest::RequestBuilder {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().copied())
        .finish();
    client
        .post(endpoint)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
}

pub(super) fn unix_now() -> Result<u64, SignInError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| SignInError::TokenResponse)
}

#[derive(Deserialize)]
pub(super) struct TokenReply {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default, deserialize_with = "present")]
    id_token: Option<String>,
    #[serde(default)]
    token_type: String,
    #[serde(default)]
    expires_in: u64,
    #[serde(default, deserialize_with = "present")]
    scope: Option<String>,
    earliest_refresh_at: Option<RefreshTime>,
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    value: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(value).map(Some)
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RefreshTime {
    Unix(u64),
    Text(String),
}

impl RefreshTime {
    fn unix(self) -> Result<u64, SignInError> {
        match self {
            Self::Unix(value) => Ok(value),
            Self::Text(value) => {
                if let Ok(seconds) = value.parse() {
                    return Ok(seconds);
                }
                let date = chrono::DateTime::parse_from_rfc3339(&value)
                    .map_err(|_| SignInError::TokenResponse)?;
                u64::try_from(date.timestamp())
                    .ok()
                    .and_then(|seconds| {
                        seconds.checked_add(u64::from(date.timestamp_subsec_nanos() != 0))
                    })
                    .ok_or(SignInError::TokenResponse)
            }
        }
    }
}

impl TokenReply {
    pub(super) fn into_tokens(
        self,
        received: u64,
        previous_scope: Option<&str>,
    ) -> Result<(TokenSet, Option<String>), SignInError> {
        let scope = self
            .scope
            .or_else(|| previous_scope.map(str::to_owned))
            .ok_or(SignInError::TokenResponse)?;
        if !has_plan_scope(&scope) {
            return Err(SignInError::Consent);
        }
        if !self.token_type.eq_ignore_ascii_case("Bearer")
            || self.expires_in == 0
            || self.expires_in > 86_400
            || self
                .id_token
                .as_deref()
                .is_some_and(|token| !valid_token(token))
        {
            return Err(SignInError::TokenResponse);
        }
        let tokens = TokenSet {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            scope,
            expires_at: received
                .checked_add(self.expires_in)
                .ok_or(SignInError::TokenResponse)?,
            refresh_after: self
                .earliest_refresh_at
                .map(RefreshTime::unix)
                .transpose()?,
        };
        if !tokens.valid() {
            return Err(SignInError::TokenResponse);
        }
        Ok((tokens, self.id_token))
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct TokenSet {
    pub(super) access_token: String,
    pub(super) refresh_token: String,
    pub(super) scope: String,
    pub(super) expires_at: u64,
    pub(super) refresh_after: Option<u64>,
}

impl TokenSet {
    pub(super) fn valid(&self) -> bool {
        valid_token(&self.access_token)
            && valid_token(&self.refresh_token)
            && self.scope.len() <= 4096
            && self
                .scope
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic())
            && has_plan_scope(&self.scope)
            && self.expires_at > 0
            && UNIX_EPOCH
                .checked_add(Duration::from_secs(self.expires_at))
                .is_some()
    }
}

pub(super) fn valid_token(token: &str) -> bool {
    visible_ascii(token, 32 * 1024)
}

fn has_plan_scope(scope: &str) -> bool {
    [
        "resource.invoke",
        "chatgpt.tokens.use.direct",
        "offline_access",
    ]
    .into_iter()
    .all(|required| {
        scope
            .split_ascii_whitespace()
            .any(|scope| scope == required)
    })
}
