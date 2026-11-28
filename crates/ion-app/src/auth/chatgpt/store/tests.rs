use super::*;
use crate::auth::chatgpt::{
    ISSUER, RESOURCE, SCOPES,
    tests::{
        CLIENT, DEADLINE, FixtureKey, Query, SUBJECT, claims, code_reply, deliver, header,
        read_request, reply, with_token_server,
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    process::Stdio,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::TcpListener,
    process::{Child, Command},
    time::timeout,
};

struct Fixture {
    dir: PathBuf,
    store: ProfileStore,
    stop: CancellationToken,
}

impl Fixture {
    async fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("ion-plan-fixture-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&dir).unwrap();
        let stop = CancellationToken::new();
        let store = ProfileStore::open(dir.join("vault"), &stop).await.unwrap();
        Self { store, dir, stop }
    }

    async fn install(&self, client: &str, remaining: u64, refresh_after: Option<u64>) -> ProfileId {
        let id = client.parse().unwrap();
        let file = self.store.profile_file(&id, &self.stop).await.unwrap();
        let mut record = Record::new(&id, self.store.host);
        record
            .activate(Grant {
                identity: identity::Identity {
                    sub: SUBJECT.into(),
                    email: Some("fixture@example.invalid".into()),
                },
                tokens: TokenSet {
                    access_token: format!("access:{client}"),
                    refresh_token: format!("refresh:{client}"),
                    scope: SCOPES.into(),
                    expires_at: provider::unix_now().unwrap() + remaining,
                    refresh_after,
                },
                id_token: "local-fixture-id".into(),
            })
            .unwrap();
        file.write(&record).unwrap();
        id
    }

    async fn callback(&self, profile: Option<&ProfileId>) -> (PendingSignIn, Query) {
        let sign_in = self.store.begin_sign_in(profile, &self.stop).await.unwrap();
        let client = profile.is_none().then_some(CLIENT);
        deliver(sign_in.url().clone(), sign_in.callback(&self.stop), client).await
    }

    fn root(&self) -> PathBuf {
        self.dir.join("vault")
    }

    async fn reopen(&self) -> ProfileStore {
        ProfileStore::open(self.root(), &self.stop).await.unwrap()
    }

    fn file(&self, id: &ProfileId) -> PathBuf {
        self.root().join(profile_name(id))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

struct Loopback {
    listener: TcpListener,
    token: String,
    jwks: String,
    discovery: String,
    revoke: String,
}

impl Loopback {
    async fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        Self {
            listener,
            token: format!("{base}/token"),
            jwks: format!("{base}/jwks"),
            discovery: format!("{base}/discovery"),
            revoke: format!("{base}/revoke"),
        }
    }

    fn provider(&self) -> Provider<'_> {
        Provider {
            token: &self.token,
            jwks: &self.jwks,
            discovery: &self.discovery,
        }
    }

    async fn request(&self, line: &str) -> (tokio::net::TcpStream, BTreeMap<String, String>) {
        let (socket, head, body) = read_request(&self.listener).await;
        assert_eq!(head.lines().next().unwrap(), line);
        (
            socket,
            url::form_urlencoded::parse(body.as_bytes())
                .into_owned()
                .collect(),
        )
    }

    async fn refresh(&self) -> tokio::net::TcpStream {
        let (socket, form) = self.request("POST /token HTTP/1.1").await;
        assert_eq!(
            form,
            BTreeMap::from([
                ("grant_type".into(), "refresh_token".into()),
                ("client_id".into(), CLIENT.into()),
                ("refresh_token".into(), format!("refresh:{CLIENT}")),
                ("resource".into(), RESOURCE.into()),
            ])
        );
        socket
    }

    async fn none(&self) {
        assert!(
            timeout(Duration::from_millis(25), self.listener.accept())
                .await
                .is_err()
        );
    }
}

fn rotated() -> Value {
    // Unchanged grants can omit scope and ID token on refresh.
    json!({"access_token":"replacement-access","refresh_token":"replacement-refresh","token_type":"Bearer","expires_in":3600})
}

#[tokio::test]
async fn remembers_a_callback_then_uses_only_the_verified_profile_after_reopen() {
    let fixture = Fixture::new().await;
    let (exchange, query) = fixture.callback(None).await;
    let id = exchange.profile_id().clone();
    let reopened = fixture.reopen().await;
    assert!(reopened.host == fixture.store.host);
    let pending = reopened.profile(&id, &fixture.stop).await.unwrap();
    assert_eq!(pending.subject(), None);
    assert_eq!(pending.status(), ConnectionStatus::SignedOut);
    assert!(matches!(
        reopened.access(&id, &fixture.stop).await,
        Err(StoreError::SignInRequired)
    ));
    let stop = &fixture.stop;
    let connected = with_token_server(query, &FixtureKey::new(), SUBJECT, async |provider| {
        exchange.finish_at(provider, stop).await
    })
    .await
    .unwrap();
    assert_eq!(connected, id);
    let profile = reopened.profile(&id, stop).await.unwrap();
    assert_eq!(profile.id(), &id);
    assert_eq!(profile.subject(), Some(SUBJECT));
    assert_eq!(profile.email(), Some("fixture@example.invalid"));
    assert_eq!(profile.status(), ConnectionStatus::Connected);
    assert!(matches!(
        reopened.begin_sign_in(Some(&id), stop).await,
        Err(StoreError::AlreadyConnected)
    ));
    let access = reopened.access(&id, stop).await.unwrap();
    assert_eq!(access.bearer(), "fixture-access");
    assert!(access.expires_at() > SystemTime::now());
    let other = fixture
        .install(&CLIENT.to_ascii_uppercase(), 3600, None)
        .await;
    assert_eq!(
        reopened.access(&other, stop).await.unwrap().bearer(),
        format!("access:{}", CLIENT.to_ascii_uppercase())
    );
    assert_eq!(
        fs::metadata(fixture.root()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for entry in fs::read_dir(fixture.root()).unwrap() {
        assert_eq!(
            entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let isolated_root = fixture.dir.join("other");
    let isolated = ProfileStore::open(&isolated_root, stop).await.unwrap();
    fs::copy(fixture.file(&id), isolated_root.join(profile_name(&id))).unwrap();
    assert!(matches!(
        isolated.profile(&id, stop).await,
        Err(StoreError::ProfileMismatch)
    ));
}

#[tokio::test]
async fn sign_out_fences_delayed_browser_and_callback_before_exchange() {
    let fixture = Fixture::new().await;
    let stop = &fixture.stop;
    let (first, _) = fixture.callback(None).await;
    let id = first.profile_id().clone();
    let in_browser = fixture.store.begin_sign_in(Some(&id), stop).await.unwrap();
    let reopened = fixture.reopen().await;
    assert_eq!(
        reopened.disconnect(&id, stop).await.unwrap(),
        Disconnect::AlreadySignedOut
    );
    let peer = Loopback::new().await;
    let (late, _) = deliver(in_browser.url().clone(), in_browser.callback(stop), None).await;
    for attempt in [first, late] {
        assert!(matches!(
            attempt.finish_at(&peer.provider(), stop).await,
            Err(StoreError::SignInSuperseded)
        ));
    }
    peer.none().await;
    assert_eq!(
        reopened.profile(&id, stop).await.unwrap().status(),
        ConnectionStatus::SignedOut
    );

    // A fresh browser round trip is still usable. It also invalidates any
    // sibling attempt that shared its epoch without spending that code.
    let (fresh, query) = fixture.callback(Some(&id)).await;
    let (sibling, _) = fixture.callback(Some(&id)).await;
    with_token_server(query, &FixtureKey::new(), SUBJECT, async |provider| {
        fresh.finish_at(provider, stop).await
    })
    .await
    .unwrap();
    assert!(matches!(
        sibling.finish_at(&peer.provider(), stop).await,
        Err(StoreError::SignInSuperseded)
    ));
    peer.none().await;
}

#[tokio::test]
async fn sign_out_after_dispatch_revokes_the_grant_before_returning() {
    let fixture = Fixture::new().await;
    let stop = &fixture.stop;
    let (exchange, query) = fixture.callback(None).await;
    let id = exchange.profile_id().clone();
    let reopened = fixture.reopen().await;
    let peer = Loopback::new().await;
    let provider = peer.provider();
    let key = FixtureKey::new();
    let (finished, ()) = tokio::join!(exchange.finish_at(&provider, stop), async {
        let (mut socket, _) = peer.request("POST /token HTTP/1.1").await;
        let sign_out = reopened.disconnect_at(&id, &provider, stop);
        tokio::pin!(sign_out);
        assert!(futures_util::poll!(&mut sign_out).is_pending());
        reply(
            &mut socket,
            "200 OK",
            "",
            &code_reply(&query, &key, SUBJECT),
        )
        .await;
        drop(socket);
        let (mut socket, _) = peer.request("GET /jwks HTTP/1.1").await;
        reply(&mut socket, "200 OK", "", &key.keys().to_string()).await;
        drop(socket);

        let (outcome, ()) = tokio::join!(sign_out, async {
            let (mut socket, _) = peer.request("GET /discovery HTTP/1.1").await;
            reply(
                &mut socket,
                "200 OK",
                "",
                &json!({"issuer":ISSUER,"revocation_endpoint":peer.revoke}).to_string(),
            )
            .await;
            drop(socket);
            let (mut socket, form) = peer.request("POST /revoke HTTP/1.1").await;
            assert_eq!(form["token"], "fixture-refresh");
            assert_eq!(form["client_id"], CLIENT);
            reply(&mut socket, "200 OK", "", "").await;
        });
        assert_eq!(outcome.unwrap(), Disconnect::Revoked);
    });
    assert_eq!(finished.unwrap(), id);
    assert!(matches!(
        reopened.access_at(&id, &provider, stop).await,
        Err(StoreError::SignInRequired)
    ));
}

#[tokio::test]
async fn abandoned_exchange_records_unknown_outcome_without_saving_secrets() {
    let fixture = Fixture::new().await;
    let (exchange, _) = fixture.callback(None).await;
    let id = exchange.profile_id().clone();
    let peer = Loopback::new().await;
    let provider = peer.provider();
    let stop = CancellationToken::new();
    let (result, ()) = tokio::join!(exchange.finish_at(&provider, &stop), async {
        let (_socket, form) = peer.request("POST /token HTTP/1.1").await;
        assert_eq!(form["grant_type"], "authorization_code");
        stop.cancel();
    });
    assert!(matches!(
        result,
        Err(StoreError::SignIn(SignInError::Cancelled))
    ));
    let saved = fs::read_to_string(fixture.file(&id)).unwrap();
    assert!(
        saved.contains("sign_in_unknown")
            && !saved.contains("credentials")
            && !saved.contains("fixture-code")
    );
    let store = fixture.reopen().await;
    assert_eq!(
        store.profile(&id, &fixture.stop).await.unwrap().status(),
        ConnectionStatus::NeedsSignIn
    );
    assert_eq!(
        store
            .disconnect_at(&id, &provider, &fixture.stop)
            .await
            .unwrap(),
        Disconnect::RemoteUnconfirmed
    );
    peer.none().await;
}

#[tokio::test]
async fn expired_code_keeps_the_registration_without_marking_sign_out_uncertain() {
    let fixture = Fixture::new().await;
    let stop = &fixture.stop;
    let (exchange, _) = fixture.callback(None).await;
    let id = exchange.profile_id().clone();
    let peer = Loopback::new().await;
    let provider = peer.provider();
    let (result, ()) = tokio::join!(exchange.finish_at(&provider, stop), async {
        let (mut socket, _) = peer.request("POST /token HTTP/1.1").await;
        reply(
            &mut socket,
            "400 Bad Request",
            "",
            r#"{"error":"invalid_grant"}"#,
        )
        .await;
    });
    assert!(matches!(
        result,
        Err(StoreError::SignIn(SignInError::CodeExpired))
    ));
    let next = fixture.store.begin_sign_in(Some(&id), stop).await.unwrap();
    assert!(
        next.url()
            .query_pairs()
            .any(|(key, value)| key == "client_id" && value == CLIENT)
    );
    assert_eq!(
        fixture
            .store
            .disconnect_at(&id, &provider, stop)
            .await
            .unwrap(),
        Disconnect::AlreadySignedOut
    );
    peer.none().await;
}

#[tokio::test]
async fn reauthorization_cannot_switch_the_verified_subject() {
    let fixture = Fixture::new().await;
    let id = fixture.install(CLIENT, 30, None).await;
    let peer = Loopback::new().await;
    let provider = peer.provider();
    let stop = &fixture.stop;
    let (result, ()) = tokio::join!(fixture.store.access_at(&id, &provider, stop), async {
        reply(
            &mut peer.refresh().await,
            "400 Bad Request",
            "",
            r#"{"error":"invalid_grant"}"#,
        )
        .await;
    });
    assert!(matches!(result, Err(StoreError::SignInRequired)));
    let (returning, query) = fixture.callback(Some(&id)).await;
    let result = with_token_server(
        query,
        &FixtureKey::new(),
        "different-account",
        async |provider| returning.finish_at(provider, stop).await,
    )
    .await;
    assert!(matches!(result, Err(StoreError::ProfileMismatch)));
    let profile = fixture.store.profile(&id, stop).await.unwrap();
    assert_eq!(profile.subject(), Some(SUBJECT));
    assert_eq!(profile.status(), ConnectionStatus::NeedsSignIn);
}

#[tokio::test]
async fn rejects_unsafe_profile_paths_without_reading_their_contents() {
    let fixture = Fixture::new().await;
    let id = fixture.install(CLIENT, 3600, None).await;
    for invalid in ["", "../installation", "a.lock", "dynamic_agent_client"] {
        assert_eq!(invalid.parse::<ProfileId>(), Err(StoreError::ProfileId));
    }
    let alias = fixture.dir.join("alias");
    symlink(fixture.root(), &alias).unwrap();
    assert!(matches!(
        ProfileStore::open(alias, &fixture.stop).await,
        Err(StoreError::Storage)
    ));
    let record = fixture.file(&id);
    let moved = fixture.dir.join("outside-original");
    fs::rename(&record, &moved).unwrap();
    symlink(&moved, &record).unwrap();
    assert!(matches!(
        fixture.store.profile(&id, &fixture.stop).await,
        Err(StoreError::Storage)
    ));
    fs::remove_file(&record).unwrap();
    fs::hard_link(&moved, &record).unwrap();
    assert!(matches!(
        fixture.store.profile(&id, &fixture.stop).await,
        Err(StoreError::Storage)
    ));
    fs::remove_file(&moved).unwrap();
    fs::set_permissions(&record, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        fixture.store.profile(&id, &fixture.stop).await,
        Err(StoreError::Storage)
    ));
}

#[tokio::test]
async fn open_store_resolves_fresh_access_without_locking_the_installation() {
    let fixture = Fixture::new().await;
    let id = fixture.install(CLIENT, 3600, None).await;
    let _installation = RecordFile::lock(&fixture.store.directory, "installation", &fixture.stop)
        .await
        .unwrap();
    let fresh = timeout(DEADLINE, fixture.store.access(&id, &fixture.stop))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fresh.bearer(), format!("access:{CLIENT}"));
}

#[tokio::test]
async fn honors_refresh_window_without_releasing_an_expired_token() {
    let fixture = Fixture::new().await;
    let peer = Loopback::new().await;
    let minimum = provider::unix_now().unwrap() + 120;
    let fresh = fixture.install(CLIENT, 45, Some(minimum)).await;
    assert_eq!(
        fixture
            .store
            .access_at(&fresh, &peer.provider(), &fixture.stop)
            .await
            .unwrap()
            .bearer(),
        format!("access:{CLIENT}")
    );
    let (file, mut record) = fixture
        .store
        .lock_profile(&fresh, &fixture.stop)
        .await
        .unwrap();
    let State::Active(active) = &mut record.state else {
        panic!("fixture must be active");
    };
    active.tokens.expires_at = provider::unix_now().unwrap();
    file.write(&record).unwrap();
    drop(file);
    assert!(matches!(
        fixture
            .store
            .access_at(&fresh, &peer.provider(), &fixture.stop)
            .await,
        Err(StoreError::RefreshNotReady)
    ));
    peer.none().await;
}

// Re-enter this test binary for an independent process and an actual kernel
// lock/kill boundary. Run only the helper, with a fixture-only loopback URL.
#[test]
fn profile_process_worker() {
    let Some(root) = std::env::var_os("ION_FIXTURE_PLAN_ROOT") else {
        return;
    };
    let token_url = std::env::var("ION_FIXTURE_PLAN_URL").unwrap();
    let parsed = url::Url::parse(&token_url).unwrap();
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    assert_eq!(parsed.scheme(), "http");
    assert_eq!(parsed.path(), "/token");
    let root = PathBuf::from(root);
    let id: ProfileId = CLIENT.parse().unwrap();
    if std::env::var_os("ION_FIXTURE_PLAN_EXPECT_LOCK").is_some() {
        let lease = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(format!("{}.lock", profile_name(&id))))
            .unwrap();
        assert!(matches!(
            lease.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        println!("saw peer lease held");
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let stop = CancellationToken::new();
            let store = ProfileStore::open(root, &stop).await.unwrap();
            let peer = Provider {
                token: &token_url,
                jwks: &token_url,
                discovery: &token_url,
            };
            let access = store.access_at(&id, &peer, &stop).await.unwrap();
            assert_eq!(access.bearer(), "replacement-access");
        });
}

fn spawn_worker(fixture: &Fixture, peer: &Loopback, read_lock: bool) -> Child {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "auth::chatgpt::store::tests::profile_process_worker",
            "--nocapture",
        ])
        .env("ION_FIXTURE_PLAN_ROOT", fixture.root())
        .env("ION_FIXTURE_PLAN_URL", &peer.token)
        .env_remove("ION_FIXTURE_PLAN_EXPECT_LOCK")
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if read_lock {
        command.env("ION_FIXTURE_PLAN_EXPECT_LOCK", "1");
    }
    command.spawn().unwrap()
}

async fn assert_exit(mut child: Child) {
    assert!(
        timeout(DEADLINE, child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn two_processes_share_one_committed_rotation() {
    let fixture = Fixture::new().await;
    let id = fixture.install(CLIENT, 30, None).await;
    let peer = Loopback::new().await;
    let one = spawn_worker(&fixture, &peer, false);
    let mut socket = peer.refresh().await;
    let mut two = spawn_worker(&fixture, &peer, true);
    let mut output = BufReader::new(two.stdout.take().unwrap()).lines();
    timeout(DEADLINE, async {
        while output
            .next_line()
            .await
            .unwrap()
            .expect("worker did not observe the lease")
            != "saw peer lease held"
        {}
    })
    .await
    .unwrap();
    reply(&mut socket, "200 OK", "", &rotated().to_string()).await;
    drop(socket);
    tokio::join!(assert_exit(one), assert_exit(two));
    peer.none().await;
    assert_eq!(
        fixture
            .reopen()
            .await
            .access(&id, &fixture.stop)
            .await
            .unwrap()
            .bearer(),
        "replacement-access"
    );
    let saved = fs::read_to_string(fixture.file(&id)).unwrap();
    assert!(saved.contains("replacement-refresh") && !saved.contains(&format!("refresh:{CLIENT}")));
}

#[tokio::test]
async fn process_loss_after_post_fences_the_spent_token_on_restart() {
    let fixture = Fixture::new().await;
    let id = fixture.install(CLIENT, 30, None).await;
    let peer = Loopback::new().await;
    let mut killed = spawn_worker(&fixture, &peer, false);
    let socket = peer.refresh().await;
    killed.start_kill().unwrap();
    assert!(
        !timeout(DEADLINE, killed.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    drop(socket);
    let saved = fs::read_to_string(fixture.file(&id)).unwrap();
    assert!(saved.contains("refresh_unknown"));
    for secret in ["refresh:", "access:", "local-fixture-id", "credentials"] {
        assert!(!saved.contains(secret), "fence retained {secret}");
    }
    let reopened = fixture.reopen().await;
    assert_eq!(
        reopened.profile(&id, &fixture.stop).await.unwrap().status(),
        ConnectionStatus::NeedsSignIn
    );
    assert!(matches!(
        reopened
            .access_at(&id, &peer.provider(), &fixture.stop)
            .await,
        Err(StoreError::RefreshUnknown)
    ));
    assert_eq!(
        reopened
            .disconnect_at(&id, &peer.provider(), &fixture.stop)
            .await
            .unwrap(),
        Disconnect::RemoteUnconfirmed
    );
    assert_eq!(
        fixture
            .reopen()
            .await
            .disconnect_at(&id, &peer.provider(), &fixture.stop)
            .await
            .unwrap(),
        Disconnect::RemoteUnconfirmed
    );
    assert!(
        !fs::read_to_string(fixture.file(&id))
            .unwrap()
            .contains("local-fixture-id")
    );
    peer.none().await;
}

#[tokio::test]
async fn staged_rotation_survives_key_service_failure_without_reposting() {
    let fixture = Fixture::new().await;
    let id = fixture.install(CLIENT, 30, None).await;
    let peer = Loopback::new().await;
    let key = FixtureKey::new();
    let response = signed_rotation(&key, SUBJECT);
    let provider = peer.provider();
    let (result, ()) = tokio::join!(
        fixture.store.access_at(&id, &provider, &fixture.stop),
        async {
            reply(
                &mut peer.refresh().await,
                "200 OK",
                "",
                &response.to_string(),
            )
            .await;
            let (mut socket, _) = peer.request("GET /jwks HTTP/1.1").await;
            reply(&mut socket, "503 Service Unavailable", "", "").await;
        }
    );
    assert!(matches!(result, Err(StoreError::VerificationPending)));
    let reopen = fixture.reopen().await;
    assert_eq!(
        reopen.profile(&id, &fixture.stop).await.unwrap().status(),
        ConnectionStatus::VerificationPending
    );
    let (result, ()) = tokio::join!(reopen.access_at(&id, &provider, &fixture.stop), async {
        let (mut socket, _) = peer.request("GET /jwks HTTP/1.1").await;
        reply(&mut socket, "200 OK", "", &key.keys().to_string()).await;
    });
    assert_eq!(result.unwrap().bearer(), "replacement-access");
    assert_eq!(
        reopen.profile(&id, &fixture.stop).await.unwrap().status(),
        ConnectionStatus::Connected
    );
    peer.none().await;
}

fn signed_rotation(key: &FixtureKey, subject: &str) -> Value {
    let mut verified_claims = claims("", provider::unix_now().unwrap(), subject);
    verified_claims.as_object_mut().unwrap().remove("nonce");
    let mut response = rotated();
    response["id_token"] = json!(key.sign(&header(), &verified_claims));
    response
}

#[tokio::test]
async fn rotation_cannot_switch_the_verified_subject() {
    let fixture = Fixture::new().await;
    let id = fixture.install(CLIENT, 30, None).await;
    let peer = Loopback::new().await;
    let key = FixtureKey::new();
    let response = signed_rotation(&key, "other-account");
    let provider = peer.provider();
    let (result, ()) = tokio::join!(
        fixture.store.access_at(&id, &provider, &fixture.stop),
        async {
            reply(
                &mut peer.refresh().await,
                "200 OK",
                "",
                &response.to_string(),
            )
            .await;
            let (mut socket, _) = peer.request("GET /jwks HTTP/1.1").await;
            reply(&mut socket, "200 OK", "", &key.keys().to_string()).await;
        }
    );
    assert!(matches!(result, Err(StoreError::Identity)));
    let store = fixture.reopen().await;
    let profile = store.profile(&id, &fixture.stop).await.unwrap();
    assert_eq!(profile.subject(), Some(SUBJECT));
    assert_eq!(profile.status(), ConnectionStatus::NeedsSignIn);
    assert!(matches!(
        store.access_at(&id, &provider, &fixture.stop).await,
        Err(StoreError::SignInRequired)
    ));
    assert_eq!(
        store
            .disconnect_at(&id, &provider, &fixture.stop)
            .await
            .unwrap(),
        Disconnect::RemoteUnconfirmed
    );
    peer.none().await;
}

#[tokio::test]
async fn terminal_refresh_clears_secrets_but_retains_the_correct_registration() {
    let fixture = Fixture::new().await;
    let id = fixture.install(CLIENT, 30, None).await;
    let peer = Loopback::new().await;
    let provider = peer.provider();
    let (result, ()) = tokio::join!(
        fixture.store.access_at(&id, &provider, &fixture.stop),
        async {
            reply(
                &mut peer.refresh().await,
                "400 Bad Request",
                "",
                r#"{"error":"refresh_token_reused","error_description":"must not be reported"}"#,
            )
            .await;
        }
    );
    assert!(matches!(result, Err(StoreError::SignInRequired)));
    let reopen = fixture.reopen().await;
    let profile = reopen.profile(&id, &fixture.stop).await.unwrap();
    assert_eq!(profile.status(), ConnectionStatus::SignedOut);
    assert_eq!(profile.subject(), Some(SUBJECT));
    assert_eq!(profile.id().as_str(), CLIENT);
    assert!(matches!(
        reopen.access_at(&id, &provider, &fixture.stop).await,
        Err(StoreError::SignInRequired)
    ));
    assert!(
        !fs::read_to_string(fixture.file(&id))
            .unwrap()
            .contains("refresh:")
    );
    peer.none().await;
}

#[tokio::test]
async fn revokes_only_the_selected_profile_and_reports_when_revocation_is_unconfirmed() {
    let fixture = Fixture::new().await;
    let id = fixture.install(CLIENT, 3600, None).await;
    let id_two = fixture.install("oaiapp_second_fixture", 3600, None).await;
    let peer = Loopback::new().await;
    let provider = peer.provider();
    let (result, ()) = tokio::join!(
        fixture.store.disconnect_at(&id, &provider, &fixture.stop),
        async {
            let (mut socket, _) = peer.request("GET /discovery HTTP/1.1").await;
            reply(
                &mut socket,
                "200 OK",
                "",
                &json!({"issuer":ISSUER,"revocation_endpoint":peer.revoke}).to_string(),
            )
            .await;
            drop(socket);
            for status in ["500 Internal Server Error", "200 OK"] {
                let (mut socket, form) = peer.request("POST /revoke HTTP/1.1").await;
                assert_eq!(
                    form,
                    BTreeMap::from([
                        ("client_id".into(), CLIENT.into()),
                        ("token_type_hint".into(), "refresh_token".into()),
                        ("token".into(), format!("refresh:{CLIENT}"))
                    ])
                );
                reply(&mut socket, status, "", "").await;
            }
        }
    );
    assert_eq!(result.unwrap(), Disconnect::Revoked);
    let reopened = fixture.reopen().await;
    assert_eq!(
        reopened
            .disconnect_at(&id, &provider, &fixture.stop)
            .await
            .unwrap(),
        Disconnect::AlreadySignedOut
    );
    assert!(
        !fs::read_to_string(fixture.file(&id))
            .unwrap()
            .contains("refresh:")
    );
    assert_eq!(
        reopened
            .profile(&id_two, &fixture.stop)
            .await
            .unwrap()
            .status(),
        ConnectionStatus::Connected
    );
    let rejected_destination = Loopback::new().await;
    let (result, ()) = tokio::join!(
        fixture
            .store
            .disconnect_at(&id_two, &provider, &fixture.stop),
        async {
            let (mut socket, _) = peer.request("GET /discovery HTTP/1.1").await;
            reply(
                &mut socket,
                "200 OK",
                "",
                &json!({"issuer":ISSUER,"revocation_endpoint":rejected_destination.revoke})
                    .to_string(),
            )
            .await;
        }
    );
    assert_eq!(result.unwrap(), Disconnect::RemoteUnconfirmed);
    assert_eq!(
        reopened
            .profile(&id_two, &fixture.stop)
            .await
            .unwrap()
            .status(),
        ConnectionStatus::RemoteUnconfirmed
    );
    assert!(
        !fs::read_to_string(fixture.file(&id_two))
            .unwrap()
            .contains("refresh:")
    );
    rejected_destination.none().await;
}

#[tokio::test]
async fn dropped_sign_out_cannot_reenable_access_and_can_finish_revocation() {
    let fixture = Fixture::new().await;
    let id = fixture.install(CLIENT, 3600, None).await;
    let peer = Loopback::new().await;
    let provider = peer.provider();
    let metadata = json!({"issuer":ISSUER,"revocation_endpoint":peer.revoke}).to_string();
    let receive = || async {
        let (mut socket, _) = peer.request("GET /discovery HTTP/1.1").await;
        reply(&mut socket, "200 OK", "", &metadata).await;
        drop(socket);
        let (socket, form) = peer.request("POST /revoke HTTP/1.1").await;
        (socket, form)
    };
    let before = {
        let pending = fixture.store.disconnect_at(&id, &provider, &fixture.stop);
        tokio::pin!(pending);
        // Drop the actual SDK future after the peer sees the revocation.
        tokio::select! {
            outcome = &mut pending => panic!("unexpected disconnect completion: {outcome:?}"),
            (_, form) = receive() => form,
        }
    };
    let store = fixture.reopen().await;
    assert_eq!(
        store.profile(&id, &fixture.stop).await.unwrap().status(),
        ConnectionStatus::RevocationPending
    );
    assert!(matches!(
        store.access_at(&id, &provider, &fixture.stop).await,
        Err(StoreError::SignInRequired)
    ));
    let (result, ()) = tokio::join!(store.disconnect_at(&id, &provider, &fixture.stop), async {
        let (mut socket, form) = receive().await;
        assert_eq!(form, before);
        reply(&mut socket, "200 OK", "", "").await;
    });
    assert_eq!(result.unwrap(), Disconnect::Revoked);
    assert_eq!(
        store.profile(&id, &fixture.stop).await.unwrap().status(),
        ConnectionStatus::SignedOut
    );
}
