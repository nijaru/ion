//! Native Sign in with ChatGPT for local open-source apps. This authorization
//! and credential store are separate from API-key resolution. The host owns
//! the system browser and keeps credentials outside the Session.
//!
//! [OpenAI registration contract](https://developers.openai.com/siwc/token-sharing-open-source/sign-in)

mod callback;
mod identity;
mod provider;
mod store;
#[cfg(test)]
mod tests;

use std::fmt;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use url::Url;
use uuid::Uuid;

pub use store::{
    ConnectionStatus, Disconnect, PendingSignIn, PlanAccess, Profile, ProfileId, ProfileStore,
    SignIn, StoreError,
};

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
struct HostId(Uuid);

impl HostId {
    fn generate() -> Result<Self, SignInError> {
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

struct Authorization {
    url: Url,
    listener: TcpListener,
    exchange: ExchangeBinding,
    state: String,
    client: Option<ProfileId>,
}

struct ExchangeBinding {
    redirect: String,
    verifier: String,
    nonce: String,
}

impl Authorization {
    async fn start(host: HostId, client: Option<&ProfileId>) -> Result<Self, SignInError> {
        let state = random_value()?;
        let nonce = random_value()?;
        let verifier = random_value()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let (listener, redirect) = callback::bind().await?;
        let mut url = Url::parse(AUTHORIZE).expect("fixed HTTPS authorization URL");
        url.query_pairs_mut().extend_pairs([
            (
                "client_id",
                client.map_or(DYNAMIC_CLIENT, ProfileId::as_str),
            ),
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
        if client.is_none() {
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
            client: client.cloned(),
        })
    }

    async fn callback(self, stop: &CancellationToken) -> Result<CodeExchange, SignInError> {
        let known_client = self.client.as_ref().map(ProfileId::as_str);
        let callback::Accepted { code, client } =
            callback::accept(&self.listener, &self.state, known_client, stop).await?;
        Ok(CodeExchange {
            code,
            binding: self.exchange,
            profile: client.parse().map_err(|_| SignInError::Callback)?,
        })
    }
}

/// A consumed browser callback. The code and PKCE verifier can be exchanged
/// at most once; dropping a wait never makes a possibly spent code reusable.
struct CodeExchange {
    code: String,
    binding: ExchangeBinding,
    profile: ProfileId,
}

impl CodeExchange {
    async fn exchange(
        self,
        provider: &provider::Provider<'_>,
        stop: &CancellationToken,
    ) -> Result<Grant, SignInError> {
        tokio::select! {
            biased;
            () = stop.cancelled() => Err(SignInError::Cancelled),
            result = self.request_grant(provider) => result,
        }
    }

    async fn request_grant(self, provider: &provider::Provider<'_>) -> Result<Grant, SignInError> {
        let client = provider::client()?;
        let reply = provider::form(
            &client,
            provider.token,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", self.profile.as_str()),
                ("code", &self.code),
                ("code_verifier", &self.binding.verifier),
                ("redirect_uri", &self.binding.redirect),
                ("resource", RESOURCE),
            ],
        )
        .send()
        .await
        .map_err(|_| SignInError::Network)?;
        if reply.status() != reqwest::StatusCode::OK {
            return Err(match reply.status().as_u16() {
                400 => {
                    match read_json::<provider::OAuthError>(reply, SignInError::ExchangeRejected)
                        .await
                    {
                        Ok(error) if error.error == "invalid_grant" => SignInError::CodeExpired,
                        _ => SignInError::ExchangeRejected,
                    }
                }
                429 | 500..=599 => SignInError::Network,
                _ => SignInError::ExchangeRejected,
            });
        }
        let issued_at = provider::unix_now()?;
        let reply: provider::TokenReply = read_json(reply, SignInError::TokenResponse).await?;
        let (tokens, id_token) = reply.into_tokens(issued_at, None)?;
        let id_token = id_token.ok_or(SignInError::Identity)?;
        let verified = provider
            .identity(
                &client,
                &id_token,
                self.profile.as_str(),
                Some(&self.binding.nonce),
            )
            .await?;
        Ok(Grant {
            identity: verified,
            tokens,
            id_token,
        })
    }
}

struct Grant {
    identity: identity::Identity,
    tokens: provider::TokenSet,
    id_token: String,
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
