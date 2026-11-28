//! Native Sign in with ChatGPT for local open-source apps. This is a one-shot
//! authorization flow, not an API-key resolver or a credential store. The host
//! owns the system browser and must keep credentials outside the Session.
//!
//! [OpenAI registration contract](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)

mod callback;
mod identity;
#[cfg(test)]
mod tests;

use std::{
    fmt,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
const JWKS: &str = "https://auth.openai.com/.well-known/jwks.json";
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const RESPONSE_LIMIT: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Error)]
pub enum SignInError {
    #[error("invalid ChatGPT installation identifier")]
    HostId,
    #[error("secure randomness is unavailable")]
    Random,
    #[error("could not open or receive on the local ChatGPT callback listener")]
    CallbackListener,
    #[error("timed out waiting for ChatGPT authorization")]
    CallbackTimeout,
    #[error("ChatGPT sign-in cancelled")]
    Cancelled,
    #[error("ChatGPT authorization was denied")]
    Denied,
    #[error("invalid ChatGPT authorization callback")]
    Callback,
    #[error(
        "the ChatGPT authorization code is no longer valid; sign in again with the same registration"
    )]
    CodeExpired,
    #[error("ChatGPT rejected the token exchange")]
    ExchangeRejected,
    #[error("ChatGPT sign-in service is unavailable")]
    Network,
    #[error("ChatGPT did not return valid credentials")]
    TokenResponse,
    #[error("could not verify ChatGPT identity for this registration")]
    Identity,
    #[error("ChatGPT plan usage was not granted")]
    Consent,
}

/// One ID per local installation. Persist it before registering a client and
/// reuse it on subsequent sign-ins. It is not a secret.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct HostId(Uuid);

impl HostId {
    pub fn generate() -> Result<Self, SignInError> {
        let bytes = super::random_bytes::<16>().map_err(|_| SignInError::Random)?;
        Ok(Self(uuid::Builder::from_random_bytes(bytes).into_uuid()))
    }
}

impl std::str::FromStr for HostId {
    type Err = SignInError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let id = value.parse::<Uuid>().map_err(|_| SignInError::HostId)?;
        if id.get_version_num() != 4 || id.get_variant() != uuid::Variant::RFC4122 {
            return Err(SignInError::HostId);
        }
        Ok(Self(id))
    }
}

impl fmt::Display for HostId {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.urn().fmt(out)
    }
}

/// A successful callback can issue a client ID before its account is verified.
/// Keep that registration when retrying an expired authorization code.
#[derive(Clone)]
pub struct Registration {
    host: HostId,
    client: String,
    identity: Option<identity::Identity>,
}

impl Registration {
    pub fn client_id(&self) -> &str {
        &self.client
    }
    pub fn subject(&self) -> Option<&str> {
        self.identity.as_ref().map(|id| id.sub.as_str())
    }
    pub fn email(&self) -> Option<&str> {
        self.identity.as_ref().and_then(|id| id.email.as_deref())
    }
}

/// An HTTP loopback listener and its one pending browser transaction. Drop it
/// to close the listener. Open `url()` in the system browser, not an embedded
/// webview, and do not log the URL or callback request.
pub struct Authorization {
    url: Url,
    listener: TcpListener,
    exchange: ExchangeBinding,
    state: String,
    host: HostId,
    registration: Option<Registration>,
}

struct ExchangeBinding {
    redirect: String,
    verifier: String,
    nonce: String,
}

impl Authorization {
    pub async fn register(host: HostId) -> Result<Self, SignInError> {
        Self::start(host, None).await
    }

    pub async fn reauthorize(registration: &Registration) -> Result<Self, SignInError> {
        Self::start(registration.host, Some(registration.clone())).await
    }

    async fn start(host: HostId, registration: Option<Registration>) -> Result<Self, SignInError> {
        let state = random_value()?;
        let nonce = random_value()?;
        let verifier = random_value()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let (listener, redirect) = callback::bind().await?;
        let client = registration
            .as_ref()
            .map_or(DYNAMIC_CLIENT, Registration::client_id);
        let mut url = Url::parse(AUTHORIZE).expect("fixed HTTPS authorization URL");
        url.query_pairs_mut().extend_pairs([
            ("client_id", client),
            ("ext_agent_host_id", &host.to_string()),
            ("redirect_uri", &redirect),
            ("response_type", "code"),
            ("scope", SCOPES),
            ("resource", RESOURCE),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge),
        ]);
        if registration.is_none() {
            url.query_pairs_mut().append_pair("agent_name_hint", "Ion");
        }
        Ok(Self {
            url,
            listener,
            exchange: ExchangeBinding {
                redirect,
                verifier,
                nonce,
            },
            state,
            host,
            registration,
        })
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Accept exactly one callback for this browser transaction. A callback with
    /// the wrong origin, path, or state does not settle it.
    pub async fn callback(self, stop: &CancellationToken) -> Result<CodeExchange, SignInError> {
        let known_client = self.registration.as_ref().map(Registration::client_id);
        let callback::Accepted { code, client } =
            callback::accept(&self.listener, &self.state, known_client, stop).await?;
        let registration = self.registration.unwrap_or(Registration {
            host: self.host,
            client,
            identity: None,
        });
        Ok(CodeExchange {
            code,
            binding: self.exchange,
            registration,
        })
    }
}

/// A consumed browser callback. The code and PKCE verifier can be exchanged
/// at most once; dropping a wait never makes a possibly spent code reusable.
pub struct CodeExchange {
    code: String,
    binding: ExchangeBinding,
    registration: Registration,
}

impl CodeExchange {
    pub fn registration(&self) -> &Registration {
        &self.registration
    }

    pub async fn exchange(self, stop: &CancellationToken) -> Result<Grant, SignInError> {
        self.exchange_at(TOKEN, JWKS, stop).await
    }

    // Test fixtures call this private boundary with loopback endpoints. The
    // production API intentionally has no token issuer or keyset URL override.
    async fn exchange_at(
        self,
        token_url: &str,
        jwks_url: &str,
        stop: &CancellationToken,
    ) -> Result<Grant, SignInError> {
        tokio::select! {
            biased;
            () = stop.cancelled() => Err(SignInError::Cancelled),
            result = self.request_grant(token_url, jwks_url) => result,
        }
    }

    async fn request_grant(
        mut self,
        token_url: &str,
        jwks_url: &str,
    ) -> Result<Grant, SignInError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|_| SignInError::Network)?;
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("grant_type", "authorization_code"),
                ("client_id", &self.registration.client),
                ("code", &self.code),
                ("code_verifier", &self.binding.verifier),
                ("redirect_uri", &self.binding.redirect),
                ("resource", RESOURCE),
            ])
            .finish();
        let reply = client
            .post(token_url)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|_| SignInError::Network)?;
        if reply.status() != reqwest::StatusCode::OK {
            return Err(match reply.status().as_u16() {
                400 => {
                    #[derive(Deserialize)]
                    struct ErrorBody {
                        error: String,
                    }
                    match read_json::<ErrorBody>(reply, SignInError::ExchangeRejected).await {
                        Ok(error) if error.error == "invalid_grant" => SignInError::CodeExpired,
                        _ => SignInError::ExchangeRejected,
                    }
                }
                429 | 500..=599 => SignInError::Network,
                _ => SignInError::ExchangeRejected,
            });
        }
        let issued_at = SystemTime::now();
        let tokens: Tokens = read_json(reply, SignInError::TokenResponse).await?;
        let expires_at = tokens.validate(issued_at)?;
        let keys = client
            .get(jwks_url)
            .send()
            .await
            .map_err(|_| SignInError::Network)?;
        if keys.status() != reqwest::StatusCode::OK {
            return Err(SignInError::Network);
        }
        let keys = read_json(keys, SignInError::Identity).await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SignInError::Identity)?
            .as_secs();
        let verified = identity::verify(
            &tokens.id_token,
            keys,
            &self.registration.client,
            &self.binding.nonce,
            now,
        )?;
        if self
            .registration
            .subject()
            .is_some_and(|subject| subject != verified.sub)
        {
            return Err(SignInError::Identity);
        }
        self.registration.identity = Some(verified);
        Ok(Grant {
            registration: self.registration,
            tokens,
            expires_at,
        })
    }
}

/// Verified identity and the explicitly granted plan credential. Deliberately
/// not `Debug` or serializable: a host must choose its own protected storage
/// policy. Never place these values in a Session, URL, or diagnostic.
pub struct Grant {
    registration: Registration,
    tokens: Tokens,
    expires_at: SystemTime,
}

impl Grant {
    pub fn registration(&self) -> &Registration {
        &self.registration
    }
    pub fn access_token(&self) -> &str {
        &self.tokens.access_token
    }
    pub fn refresh_token(&self) -> &str {
        &self.tokens.refresh_token
    }
    pub fn id_token(&self) -> &str {
        &self.tokens.id_token
    }
    pub fn scopes(&self) -> impl Iterator<Item = &str> {
        self.tokens.scope.split_ascii_whitespace()
    }
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }
}

#[derive(Deserialize)]
struct Tokens {
    // An identity-only response may omit all grant fields. Decide consent
    // from the response before requiring tokens; these defaults cannot pass.
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    id_token: String,
    #[serde(default)]
    token_type: String,
    #[serde(default)]
    expires_in: u64,
    scope: String,
}

impl Tokens {
    fn validate(&self, issued_at: SystemTime) -> Result<SystemTime, SignInError> {
        for required in [
            "resource.invoke",
            "chatgpt.tokens.use.direct",
            "offline_access",
        ] {
            if !self
                .scope
                .split_ascii_whitespace()
                .any(|scope| scope == required)
            {
                return Err(SignInError::Consent);
            }
        }
        if !self.token_type.eq_ignore_ascii_case("Bearer")
            || self.expires_in == 0
            || self.expires_in > 86_400
            || [&self.access_token, &self.refresh_token, &self.id_token]
                .iter()
                .any(|token| !visible_ascii(token, 32 * 1024))
        {
            return Err(SignInError::TokenResponse);
        }
        issued_at
            .checked_add(Duration::from_secs(self.expires_in))
            .ok_or(SignInError::TokenResponse)
    }
}

async fn read_json<T: DeserializeOwned>(
    mut reply: reqwest::Response,
    invalid: SignInError,
) -> Result<T, SignInError> {
    let mut data = Vec::new();
    while let Some(chunk) = reply.chunk().await.map_err(|_| SignInError::Network)? {
        if chunk.len() > RESPONSE_LIMIT - data.len() {
            return Err(invalid);
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).map_err(|_| invalid)
}

fn random_value() -> Result<String, SignInError> {
    super::random_bytes::<32>()
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|_| SignInError::Random)
}

fn visible_ascii(text: &str, maximum: usize) -> bool {
    !text.is_empty() && text.len() <= maximum && text.bytes().all(|byte| byte.is_ascii_graphic())
}
