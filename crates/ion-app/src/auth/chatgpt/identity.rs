//! Verify signed ID tokens; access and refresh tokens remain opaque to Ion.
use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use super::{ISSUER, SignInError, visible_ascii};

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Identity {
    pub(super) sub: String,
    pub(super) email: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct KeySet {
    keys: Vec<Key>,
}

#[derive(Deserialize)]
struct Key {
    kid: Option<String>,
    kty: String,
    #[serde(rename = "use")]
    usage: Option<String>,
    alg: Option<String>,
    key_ops: Option<Vec<String>>,
    n: Option<String>,
    e: Option<String>,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: String,
    crit: Option<serde_json::Value>,
    b64: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    Single(String),
    List(Vec<String>),
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: Audience,
    azp: Option<String>,
    sub: String,
    email: Option<String>,
    exp: u64,
    iat: u64,
    nbf: Option<u64>,
    nonce: Option<String>,
}

pub(super) fn verify(
    token: &str,
    keyset: KeySet,
    client: &str,
    nonce: Option<&str>,
    now: u64,
) -> Result<Identity, SignInError> {
    let fail = SignInError::Identity;
    let (signed, signature) = token.rsplit_once('.').ok_or(fail)?;
    let (header, claims) = signed.split_once('.').ok_or(fail)?;
    let header: Header = serde_json::from_slice(&decode(header)?).map_err(|_| fail)?;
    if header.alg != "RS256"
        || !visible_ascii(&header.kid, 256)
        || header.crit.is_some()
        || header.b64 == Some(false)
    {
        return Err(fail);
    }
    // Only trust keys from this issuer's pinned JWKS endpoint, never key IDs
    // as URLs or key material supplied by the token itself.
    let mut matches = keyset
        .keys
        .into_iter()
        .filter(|key| key.kid.as_deref() == Some(&header.kid));
    let key = matches.next().ok_or(fail)?;
    if matches.next().is_some()
        || key.kty != "RSA"
        || key.usage.as_deref().is_some_and(|usage| usage != "sig")
        || key
            .alg
            .as_deref()
            .is_some_and(|algorithm| algorithm != "RS256")
        || key
            .key_ops
            .is_some_and(|ops| !ops.iter().any(|op| op == "verify"))
    {
        return Err(fail);
    }
    let n = decode(key.n.as_deref().ok_or(fail)?)?;
    let e = decode(key.e.as_deref().ok_or(fail)?)?;
    RsaPublicKeyComponents { n, e }
        .verify(
            &RSA_PKCS1_2048_8192_SHA256,
            signed.as_bytes(),
            &decode(signature)?,
        )
        .map_err(|_| fail)?;
    let claims: Claims = serde_json::from_slice(&decode(claims)?).map_err(|_| fail)?;
    // This public client requests an ID token for itself only. Reject tokens
    // audience-bound to any additional app, even if our ID is present.
    let audience_ok = match claims.aud {
        Audience::Single(aud) => aud == client,
        Audience::List(aud) => matches!(aud.as_slice(), [single] if single == client),
    };
    let latest_issuance = now.saturating_add(30);
    if claims.iss != ISSUER
        || !audience_ok
        || claims.azp.is_some_and(|party| party != client)
        || !visible_ascii(&claims.sub, 255)
        || claims.exp <= now
        || claims.exp <= claims.iat
        || claims.iat > latest_issuance
        || claims
            .nbf
            .is_some_and(|not_before| not_before > latest_issuance)
        || nonce.is_some_and(|expected| {
            !claims
                .nonce
                .as_deref()
                .is_some_and(|received| bool::from(expected.as_bytes().ct_eq(received.as_bytes())))
        })
    {
        return Err(fail);
    }
    // Display metadata is not used to identify the account or workspace.
    let email = claims
        .email
        .filter(|email| email.len() <= 320 && !email.chars().any(char::is_control));
    Ok(Identity {
        sub: claims.sub,
        email,
    })
}

fn decode(value: &str) -> Result<Vec<u8>, SignInError> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| SignInError::Identity)
}
