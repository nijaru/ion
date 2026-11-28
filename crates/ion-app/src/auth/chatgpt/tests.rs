use super::*;
use aws_lc_rs::{
    rand::SystemRandom,
    rsa::{KeyPair as SigningKey, KeySize},
    signature::{KeyPair, RSA_PKCS1_SHA256},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

type Query = BTreeMap<String, String>;
pub(super) const CLIENT: &str = "oaiapp_ion_fixture";
pub(super) const SUBJECT: &str = "local-fixture-account";
pub(super) const DEADLINE: Duration = Duration::from_secs(5);

fn host() -> HostId {
    "urn:uuid:d9429cf7-34d8-4722-8e2a-db1fb53bbc8f"
        .parse()
        .unwrap()
}

async fn browser(location: &Url, request: &str, host: &str) -> String {
    let address = (location.host_str().unwrap(), location.port().unwrap());
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(format!("GET {request} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    timeout(DEADLINE, socket.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    response
}

fn location(query: &Query) -> (Url, String) {
    let url = Url::parse(&query["redirect_uri"]).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    assert_eq!(url.path(), "/auth/callback");
    let address = format!("127.0.0.1:{}", url.port().unwrap());
    (url, address)
}

pub(super) async fn deliver(auth: Authorization, client: Option<&str>) -> (CodeExchange, Query) {
    let query: Query = auth.url().query_pairs().into_owned().collect();
    let (url, address) = location(&query);
    let task = tokio::spawn(async move { auth.callback(&CancellationToken::new()).await });
    let client = client.map_or_else(String::new, |value| format!("&client_id={value}"));
    let target = format!(
        "/auth/callback?code=fixture-code&state={}{client}",
        query["state"]
    );
    let response = browser(&url, &target, &address).await;
    assert!(response.starts_with("HTTP/1.1 200"));
    assert!(response.contains("Authorization received") && !response.contains("fixture-code"));
    assert!(
        response.contains("Cache-Control: no-store")
            && response.contains("Referrer-Policy: no-referrer")
    );
    let exchange = timeout(DEADLINE, task).await.unwrap().unwrap().unwrap();
    assert!(
        TcpStream::connect(&address).await.is_err(),
        "callback listener still accepts requests"
    );
    (exchange, query)
}

pub(super) struct FixtureKey(SigningKey);

impl FixtureKey {
    pub(super) fn new() -> Self {
        Self(SigningKey::generate(KeySize::Rsa2048).unwrap())
    }

    pub(super) fn keys(&self) -> Value {
        // AWS-LC exposes the generated key as PKCS#1 DER. Read its two
        // components here solely to build the external JWKS fixture.
        let mut encoded = self.0.public_key().as_ref();
        let mut integers = der_field(&mut encoded, 0x30);
        let n = der_field(&mut integers, 0x02);
        let e = der_field(&mut integers, 0x02);
        assert!(encoded.is_empty() && integers.is_empty());
        json!({"keys":[{"kid":"fixture-key","use":"sig","alg":"RS256","kty":"RSA",
          "n":URL_SAFE_NO_PAD.encode(n.strip_prefix(&[0]).unwrap_or(n)),
          "e":URL_SAFE_NO_PAD.encode(e.strip_prefix(&[0]).unwrap_or(e))}]})
    }

    pub(super) fn sign(&self, header: &Value, claims: &Value) -> String {
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let mut signature = vec![0; self.0.public_modulus_len()];
        self.0
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signed.as_bytes(),
                &mut signature,
            )
            .unwrap();
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature))
    }
}

fn der_field<'a>(remaining: &mut &'a [u8], tag: u8) -> &'a [u8] {
    assert_eq!(remaining[0], tag);
    let mut length = usize::from(remaining[1]);
    let mut start = 2;
    if length >= 128 {
        start += length - 128;
        assert!(start <= 4, "fixture length fits two octets");
        length = remaining[2..start]
            .iter()
            .fold(0, |n, byte| n * 256 + usize::from(*byte));
    }
    let (value, rest) = remaining[start..].split_at(length);
    *remaining = rest;
    value
}

pub(super) fn claims(nonce: &str, now: u64, subject: &str) -> Value {
    json!({"iss": ISSUER,"aud":CLIENT,"sub":subject,"email":"fixture@example.invalid",
        "exp":now+3600,"iat":now,"nonce":nonce})
}

pub(super) fn header() -> Value {
    json!({"alg":"RS256","kid":"fixture-key"})
}

pub(super) async fn read_request(listener: &TcpListener) -> (TcpStream, String, String) {
    let (mut socket, _) = timeout(DEADLINE, listener.accept()).await.unwrap().unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    let (head, offset, length) = loop {
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n != 0 && bytes.len() + n <= 32 * 1024);
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(i) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let head = std::str::from_utf8(&bytes[..i]).unwrap().to_owned();
            let length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
            break (head, i + 4, length);
        }
    };
    while bytes.len() - offset < length {
        let n = socket.read(&mut buffer).await.unwrap();
        assert!(n != 0 && bytes.len() + n <= 32 * 1024);
        bytes.extend_from_slice(&buffer[..n]);
    }
    let body = std::str::from_utf8(&bytes[offset..offset + length])
        .unwrap()
        .to_owned();
    (socket, head, body)
}

pub(super) async fn reply(socket: &mut TcpStream, status: &str, extra: &str, body: &str) {
    socket.write_all(format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: application/json\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
}

pub(super) async fn exchange_with_keys(
    exchange: CodeExchange,
    query: Query,
    key: &FixtureKey,
    subject: &str,
) -> Result<Grant, SignInError> {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let id = key.sign(&header(), &claims(&query["nonce"], now, subject));
    let response = json!({"access_token":"fixture-access","refresh_token":"fixture-refresh","token_type":"Bearer",
        "scope":SCOPES,"id_token":id,"expires_in":3600}).to_string();
    let keys = key.keys().to_string();
    let server = tokio::spawn(async move {
        let (mut socket, head, body) = read_request(&listener).await;
        assert!(head.starts_with("POST /token HTTP/1.1"));
        assert!(
            head.to_ascii_lowercase()
                .contains("content-type: application/x-www-form-urlencoded")
        );
        let sent: Query = url::form_urlencoded::parse(body.as_bytes())
            .into_owned()
            .collect();
        assert_eq!(sent.len(), 6);
        assert_eq!(sent["client_id"], CLIENT);
        assert_eq!(sent["grant_type"], "authorization_code");
        assert_eq!(sent["resource"], RESOURCE);
        assert_eq!(sent["redirect_uri"], query["redirect_uri"]);
        assert_eq!(sent["code"], "fixture-code");
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(sent["code_verifier"].as_bytes())),
            query["code_challenge"]
        );
        reply(&mut socket, "200 OK", "", &response).await;
        drop(socket);
        let (mut socket, head, body) = read_request(&listener).await;
        assert!(head.starts_with("GET /jwks HTTP/1.1") && body.is_empty());
        assert!(!head.to_ascii_lowercase().contains("authorization:"));
        reply(&mut socket, "200 OK", "", &keys).await;
    });
    let result = exchange
        .exchange_at(
            &format!("{endpoint}/token"),
            &format!("{endpoint}/jwks"),
            &CancellationToken::new(),
        )
        .await;
    server.await.unwrap();
    result
}

#[tokio::test]
async fn native_sign_in_binds_the_code_pkce_issuer_and_account_to_one_registration() {
    assert_eq!(
        HostId::generate()
            .unwrap()
            .to_string()
            .parse::<HostId>()
            .unwrap()
            .to_string()
            .len(),
        45
    );
    let auth = Authorization::register(host()).await.unwrap();
    assert!(auth.url().as_str().starts_with(AUTHORIZE));
    let first: Query = auth.url().query_pairs().into_owned().collect();
    assert_eq!(first["client_id"], DYNAMIC_CLIENT);
    assert_eq!(first["ext_agent_host_id"], host().to_string());
    assert_eq!(first["agent_name_hint"], "Ion");
    assert_eq!(first["code_challenge_method"], "S256");
    assert_eq!(first["scope"], SCOPES);
    assert!(first["nonce"] != first["state"]);
    let key = FixtureKey::new();
    let (exchange, query) = deliver(auth, Some(CLIENT)).await;
    assert_eq!(exchange.registration().client_id(), CLIENT);
    assert_eq!(exchange.registration().subject(), None);
    let grant = exchange_with_keys(exchange, query, &key, SUBJECT)
        .await
        .unwrap();
    assert_eq!(grant.registration().subject(), Some(SUBJECT));
    assert_eq!(
        grant.registration().email(),
        Some("fixture@example.invalid")
    );
    assert_eq!(grant.access_token(), "fixture-access");
    assert_eq!(grant.refresh_token(), "fixture-refresh");
    assert!(
        grant
            .scopes()
            .any(|scope| scope == "chatgpt.tokens.use.direct")
    );
    assert!(grant.expires_at() > SystemTime::now());
    let (returning, next) = deliver(
        Authorization::reauthorize(grant.registration())
            .await
            .unwrap(),
        None,
    )
    .await;
    assert_eq!(next["client_id"], CLIENT);
    assert_eq!(next["ext_agent_host_id"], first["ext_agent_host_id"]);
    assert!(!next.contains_key("agent_name_hint"));
    for name in ["state", "nonce", "code_challenge"] {
        assert_ne!(first[name], next[name]);
    }
    assert!(matches!(
        exchange_with_keys(returning, next, &key, "different-account").await,
        Err(SignInError::Identity)
    ));
}

#[tokio::test]
async fn unrelated_callbacks_cannot_settle_attempt_and_denial_never_yields_a_code() {
    let auth = Authorization::register(host()).await.unwrap();
    let query: Query = auth.url().query_pairs().into_owned().collect();
    let (url, address) = location(&query);
    let attempt = tokio::spawn(async move { auth.callback(&CancellationToken::new()).await });
    let state = &query["state"];
    for (target, host) in [
        (
            format!("/auth/callback?error=access_denied&state={state}"),
            "example.invalid",
        ),
        (
            format!("/wrong?error=access_denied&state={state}"),
            &address,
        ),
        (
            format!("/auth/callback?code=wrong&client_id={CLIENT}&state={state}&state={state}"),
            &address,
        ),
        (
            "/auth/callback?error=access_denied&state=other".into(),
            &address,
        ),
    ] {
        assert!(
            browser(&url, &target, host)
                .await
                .starts_with("HTTP/1.1 400")
        );
        assert!(!attempt.is_finished());
    }
    assert!(
        browser(
            &url,
            &format!("/auth/callback?error=access_denied&state={state}"),
            &address
        )
        .await
        .starts_with("HTTP/1.1 200")
    );
    assert!(matches!(
        timeout(DEADLINE, attempt).await.unwrap().unwrap(),
        Err(SignInError::Denied)
    ));
    assert!(TcpStream::connect(address).await.is_err());
}

#[test]
fn signed_identity_checks_claims_and_pinned_keys() {
    let key = FixtureKey::new();
    let now = 1_800_000_000;
    let base = claims("pending-nonce", now, SUBJECT);
    let verify = |token: &str, keys: Value| {
        identity::verify(
            token,
            serde_json::from_value(keys).unwrap(),
            CLIENT,
            Some("pending-nonce"),
            now,
        )
    };
    let signed = key.sign(&header(), &base);
    assert_eq!(verify(&signed, key.keys()).unwrap().sub, SUBJECT);
    for (name, replacement) in [
        ("iss", json!("https://wrong.invalid")),
        ("aud", json!([CLIENT, "other"])),
        ("nonce", json!("different")),
        ("exp", json!(now)),
        ("nbf", json!(now + 120)),
        ("azp", json!("wrong-party")),
    ] {
        let mut changed = base.clone();
        changed[name] = replacement;
        assert!(
            matches!(
                verify(&key.sign(&header(), &changed), key.keys()),
                Err(SignInError::Identity)
            ),
            "{name}"
        );
    }
    let other = FixtureKey::new();
    assert!(matches!(
        verify(&signed, other.keys()),
        Err(SignInError::Identity)
    ));
    let entry = key.keys()["keys"][0].clone();
    assert!(matches!(
        verify(&signed, json!({"keys":[entry,entry]})),
        Err(SignInError::Identity)
    ));
    assert!(matches!(
        verify(
            &other.sign(&json!({"alg":"HS256","kid":"fixture-key"}), &base),
            other.keys()
        ),
        Err(SignInError::Identity)
    ));
}

#[tokio::test]
async fn cancelled_wait_closes_local_listener_and_token_failures_never_follow_redirects() {
    let auth = Authorization::register(host()).await.unwrap();
    let (_, address) = location(&auth.url().query_pairs().into_owned().collect());
    let stop = CancellationToken::new();
    stop.cancel();
    assert!(matches!(
        auth.callback(&stop).await,
        Err(SignInError::Cancelled)
    ));
    assert!(TcpStream::connect(address).await.is_err());

    for (status, body, expected) in [
        (
            "400 Bad Request",
            r#"{"error":"invalid_grant","error_description":"private"}"#,
            SignInError::CodeExpired,
        ),
        (
            "200 OK",
            r#"{"scope":"openid email profile"}"#,
            SignInError::Consent,
        ),
        ("307 Temporary Redirect", "", SignInError::ExchangeRejected),
    ] {
        let (exchange, _) =
            deliver(Authorization::register(host()).await.unwrap(), Some(CLIENT)).await;
        let issued = exchange.registration().clone();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let target = format!("Location: {endpoint}/must-not-follow\r\n");
        let server = tokio::spawn(async move {
            let (mut socket, head, _) = read_request(&listener).await;
            assert!(head.starts_with("POST /token HTTP/1.1"));
            reply(&mut socket, status, &target, body).await;
            listener
        });
        let result = exchange
            .exchange_at(
                &format!("{endpoint}/token"),
                &format!("{endpoint}/jwks"),
                &CancellationToken::new(),
            )
            .await;
        assert!(matches!(result, Err(error) if error == expected));
        let listener = server.await.unwrap();
        assert!(
            timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
        let returning = Authorization::reauthorize(&issued).await.unwrap();
        let query: Query = returning.url().query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], CLIENT);
    }
}

#[tokio::test]
async fn stop_after_token_post_drops_the_wait_without_replaying_the_code() {
    let (exchange, _) = deliver(Authorization::register(host()).await.unwrap(), Some(CLIENT)).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let (observed, seen) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, head, _) = read_request(&listener).await;
        assert!(head.starts_with("POST /token HTTP/1.1"));
        observed.send(()).unwrap();
        // Never acknowledge the exchange. A possibly consumed code cannot be
        // sent again just because this connection was closed.
        let mut trailing = Vec::new();
        timeout(DEADLINE, socket.read_to_end(&mut trailing))
            .await
            .unwrap()
            .unwrap();
        assert!(trailing.is_empty());
        listener
    });
    let stop = CancellationToken::new();
    let token = format!("{endpoint}/token");
    let jwks = format!("{endpoint}/jwks");
    let (result, ()) = tokio::join!(exchange.exchange_at(&token, &jwks, &stop), async {
        timeout(DEADLINE, seen).await.unwrap().unwrap();
        stop.cancel();
    });
    assert!(matches!(result, Err(SignInError::Cancelled)));
    let listener = server.await.unwrap();
    assert!(
        timeout(Duration::from_millis(10), listener.accept())
            .await
            .is_err()
    );
}
