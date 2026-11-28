//! Native custody for the local ChatGPT plan route. An account's issued client
//! ID selects one record; its verified subject cannot be replaced by another.
mod files;
#[cfg(test)]
mod tests;

use std::{
    fmt,
    fs::File,
    path::Path,
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{
    Authorization, CodeExchange, Grant, HostId, SignInError,
    callback::valid_client,
    identity,
    provider::{self, OPENAI, Provider, RefreshError, TokenSet, valid_token},
    visible_ascii,
};
use files::RecordFile;

const REFRESH_LEAD_TIME: u64 = 60;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Error)]
pub enum StoreError {
    #[error("invalid ChatGPT profile identifier")]
    ProfileId,
    #[error("ChatGPT profile is not known to this installation")]
    Missing,
    #[error("ChatGPT profile does not match this installation or account")]
    ProfileMismatch,
    #[error("finish or disconnect this ChatGPT profile before signing in again")]
    AlreadyConnected,
    #[error("this ChatGPT sign-in attempt was superseded; start a new one")]
    SignInSuperseded,
    #[error(transparent)]
    SignIn(#[from] SignInError),
    #[error("ChatGPT credential storage is unavailable or invalid")]
    Storage,
    #[error(
        "ChatGPT credential update could not be confirmed durable; inspect the profile before retrying"
    )]
    PublicationUnknown,
    #[error("another process is updating this ChatGPT profile")]
    Busy,
    #[error("ChatGPT credential operation cancelled")]
    Cancelled,
    #[error("sign in again to use this ChatGPT profile")]
    SignInRequired,
    #[error("ChatGPT refused this client registration")]
    InvalidClient,
    #[error("ChatGPT refresh outcome is unknown; sign in again without reusing the old token")]
    RefreshUnknown,
    #[error("the ChatGPT access token expired before renewal was permitted")]
    RefreshNotReady,
    #[error("ChatGPT refresh is retained but its identity service is unavailable")]
    VerificationPending,
    #[error("could not verify the refreshed ChatGPT identity; sign in again")]
    Identity,
}

/// Identifies an app registration on this host, not an email or a bearer token.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct ProfileId(String);

impl FromStr for ProfileId {
    type Err = StoreError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !valid_client(value) {
            return Err(StoreError::ProfileId);
        }
        Ok(Self(value.to_owned()))
    }
}

impl ProfileId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ConnectionStatus {
    SignedOut,
    Connected,
    NeedsSignIn,
    VerificationPending,
    RevocationPending,
    RemoteUnconfirmed,
}

/// Non-secret snapshot. `Connected` describes the local grant, not a probe of
/// provider availability; use `ProfileStore::access` before dispatch.
pub struct Profile {
    id: ProfileId,
    identity: Option<identity::Identity>,
    status: ConnectionStatus,
}

impl Profile {
    pub fn id(&self) -> &ProfileId {
        &self.id
    }
    pub fn subject(&self) -> Option<&str> {
        self.identity.as_ref().map(|id| id.sub.as_str())
    }
    pub fn email(&self) -> Option<&str> {
        self.identity.as_ref().and_then(|id| id.email.as_deref())
    }
    pub fn status(&self) -> ConnectionStatus {
        self.status
    }
}

/// Short-lived access for this exact account and the official ChatGPT plan
/// resource. The host must not cache it across requests or send it to another
/// origin. It does not confer authority to refresh or export tokens.
pub struct PlanAccess {
    token: String,
    expires_at: u64,
}

impl PlanAccess {
    pub fn bearer(&self) -> &str {
        &self.token
    }
    pub fn expires_at(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(self.expires_at)
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Disconnect {
    AlreadySignedOut,
    Revoked,
    /// Local credentials were cleared. Tell the user they can disconnect the
    /// app in ChatGPT Settings; a remote renewable session may still exist.
    RemoteUnconfirmed,
}

/// One browser authorization. Open `url()` in a system browser, not a webview,
/// and never log it or the callback. Dropping this value closes the listener.
pub struct SignIn {
    store: ProfileStore,
    browser: Authorization,
    epoch: Option<u64>,
}

impl SignIn {
    pub fn url(&self) -> &Url {
        &self.browser.url
    }

    /// Accept the state-bound callback and retain a newly issued registration
    /// before spending its code. No credentials have been granted yet.
    pub async fn callback(self, stop: &CancellationToken) -> Result<PendingSignIn, StoreError> {
        let exchange = self.browser.callback(stop).await?;
        self.store.accept_callback(exchange, self.epoch, stop).await
    }
}

/// A consumed browser callback, bound to its profile and sign-in epoch. The
/// code is private and can be exchanged only by consuming this value.
pub struct PendingSignIn {
    store: ProfileStore,
    exchange: CodeExchange,
    epoch: u64,
}

impl PendingSignIn {
    /// Keep this ID if completion fails. A retry uses the same registration
    /// with a fresh browser authorization and code.
    pub fn profile_id(&self) -> &ProfileId {
        &self.exchange.profile
    }

    pub async fn finish(self, stop: &CancellationToken) -> Result<ProfileId, StoreError> {
        self.finish_at(&OPENAI, stop).await
    }

    async fn finish_at(
        self,
        provider: &Provider<'_>,
        stop: &CancellationToken,
    ) -> Result<ProfileId, StoreError> {
        let id = self.exchange.profile.clone();
        let (file, mut record) = self.store.lock_profile(&id, stop).await?;
        if record.sign_in_epoch != self.epoch || !record.state.accepts_sign_in() {
            return Err(StoreError::SignInSuperseded);
        }
        // Do not hold a lease while the user is in the browser. Once the
        // callback arrives, the lease spans code consumption and publication
        // so a concurrent sign-out either fences this attempt or revokes it.
        record.advance_sign_in_epoch()?;
        check_stop(stop)?;
        // A lost exchange response can leave a remote grant even though no
        // local credentials were installed. Never report confirmed sign-out.
        let previous = std::mem::replace(&mut record.state, State::SignInUnknown);
        file.write(&record)?;
        let grant = match self.exchange.exchange(provider, stop).await {
            Err(error @ SignInError::CodeExpired) => {
                // The provider definitively refused this code. Keep the
                // advanced epoch but restore the previous credential status.
                record.state = previous;
                file.write(&record)?;
                return Err(error.into());
            }
            result => result?,
        };
        record.activate(grant)?;
        check_stop(stop)?;
        file.write(&record)?;
        Ok(id)
    }
}

/// OS-permission-protected native storage (0700 directory, 0600 files). Use a
/// dedicated absolute directory under a trusted, existing parent on a local
/// filesystem. This does not encrypt data at rest, protect it from other
/// processes running as this user, or erase historical disk blocks. Do not
/// replace it while open, restore stale snapshots, or sync it across hosts.
#[derive(Clone)]
pub struct ProfileStore {
    directory: Arc<File>,
    host: HostId,
}

impl ProfileStore {
    /// Open a native store and establish its stable installation ID if needed.
    /// Keep and reuse the handle for this installation.
    pub async fn open(
        root: impl AsRef<Path>,
        stop: &CancellationToken,
    ) -> Result<Self, StoreError> {
        check_stop(stop)?;
        let directory = files::open_root(root.as_ref())?;
        let host = {
            let file = RecordFile::lock(&directory, "installation", stop).await?;
            match file.read::<String>()? {
                Some(value) => value.parse().map_err(|_| StoreError::Storage)?,
                None => {
                    check_stop(stop)?;
                    let host = HostId::generate()?;
                    file.write(&host.to_string())?;
                    host
                }
            }
        };
        Ok(Self {
            directory: Arc::new(directory),
            host,
        })
    }

    /// Start reauthorization of a saved profile, or register a new account
    /// with `None`. Sign-out invalidates all outstanding attempts for that
    /// profile. A connected profile must be disconnected first.
    pub async fn begin_sign_in(
        &self,
        profile: Option<&ProfileId>,
        stop: &CancellationToken,
    ) -> Result<SignIn, StoreError> {
        let epoch = if let Some(id) = profile {
            let (_, record) = self.lock_profile(id, stop).await?;
            if !record.state.accepts_sign_in() {
                return Err(StoreError::AlreadyConnected);
            }
            Some(record.sign_in_epoch)
        } else {
            None
        };
        check_stop(stop)?;
        let browser = Authorization::start(self.host, profile).await?;
        check_stop(stop)?;
        Ok(SignIn {
            store: self.clone(),
            browser,
            epoch,
        })
    }

    async fn accept_callback(
        &self,
        exchange: CodeExchange,
        epoch: Option<u64>,
        stop: &CancellationToken,
    ) -> Result<PendingSignIn, StoreError> {
        let epoch = match epoch {
            Some(epoch) => epoch,
            None => {
                let id = &exchange.profile;
                let file = self.profile_file(id, stop).await?;
                if read_record(&file, id, self.host)?.is_some() {
                    // A new registration cannot attach to a saved client.
                    return Err(StoreError::ProfileMismatch);
                }
                check_stop(stop)?;
                let record = Record::new(id, self.host);
                file.write(&record)?;
                record.sign_in_epoch
            }
        };
        Ok(PendingSignIn {
            store: self.clone(),
            exchange,
            epoch,
        })
    }

    pub async fn profile(
        &self,
        id: &ProfileId,
        stop: &CancellationToken,
    ) -> Result<Profile, StoreError> {
        let (_file, record) = self.lock_profile(id, stop).await?;
        Ok(Profile {
            id: id.clone(),
            identity: record.identity,
            status: record.state.status(),
        })
    }

    /// Resolve again immediately before each model request. Does not hold the
    /// lease after returning; like any short-lived credential, the result may
    /// be revoked before dispatch. Never falls back to another payment route.
    pub async fn access(
        &self,
        id: &ProfileId,
        stop: &CancellationToken,
    ) -> Result<PlanAccess, StoreError> {
        self.access_at(id, &OPENAI, stop).await
    }

    async fn access_at(
        &self,
        id: &ProfileId,
        provider: &Provider<'_>,
        stop: &CancellationToken,
    ) -> Result<PlanAccess, StoreError> {
        let (file, mut record) = self.lock_profile(id, stop).await?;
        match record.state {
            State::Active(active) => {
                let now = provider::unix_now().map_err(|_| StoreError::Storage)?;
                let tokens = &active.tokens;
                let too_early = tokens.refresh_after.is_some_and(|after| now < after);
                if tokens.expires_at > now
                    && (tokens.expires_at > now.saturating_add(REFRESH_LEAD_TIME) || too_early)
                {
                    check_stop(stop)?;
                    return Ok(access(active.tokens));
                }
                if too_early {
                    return Err(StoreError::RefreshNotReady);
                }
                let client = provider::client().map_err(|_| StoreError::Storage)?;
                check_stop(stop)?;
                // Write the fence before dispatch. If drop, process loss, or a
                // lost reply follows, no process may use this token again.
                record.state = State::RefreshUnknown;
                file.write(&record)?;
                let result = tokio::select! {
                    biased;
                    () = stop.cancelled() => return Err(StoreError::RefreshUnknown),
                    result = provider.refresh(&client, &record.client_id, &active.tokens) => result,
                };
                match result {
                    Ok((tokens, Some(id_token))) => {
                        // Already rotated. Commit the new material before I/O
                        // to verify its signed identity. It isn't usable yet.
                        record.state = State::Verifying(CredentialSet { tokens, id_token });
                        file.write(&record)?;
                        verify_rotation(file, record, provider, client, stop).await
                    }
                    Ok((tokens, None)) => {
                        // Without a replacement ID token the verified identity
                        // is unchanged; keep the original hint and replace the
                        // entire renewable set in one durable publication.
                        record.publish(
                            &file,
                            CredentialSet {
                                tokens,
                                id_token: active.id_token,
                            },
                            stop,
                        )
                    }
                    Err(RefreshError::Unusable) => {
                        record.state = State::SignedOut;
                        file.write(&record)?;
                        Err(StoreError::SignInRequired)
                    }
                    Err(RefreshError::InvalidClient) => Err(StoreError::InvalidClient),
                    Err(RefreshError::Unknown) => Err(StoreError::RefreshUnknown),
                }
            }
            State::Verifying(_) => {
                let client = provider::client().map_err(|_| StoreError::VerificationPending)?;
                verify_rotation(file, record, provider, client, stop).await
            }
            State::RefreshUnknown => Err(StoreError::RefreshUnknown),
            _ => Err(StoreError::SignInRequired),
        }
    }

    /// Prevent new access before requesting revocation, and clear all local
    /// credentials even if remote revocation cannot be confirmed. The caller
    /// is responsible for stopping already-dispatched model requests.
    pub async fn disconnect(
        &self,
        id: &ProfileId,
        stop: &CancellationToken,
    ) -> Result<Disconnect, StoreError> {
        self.disconnect_at(id, &OPENAI, stop).await
    }

    async fn disconnect_at(
        &self,
        id: &ProfileId,
        provider: &Provider<'_>,
        stop: &CancellationToken,
    ) -> Result<Disconnect, StoreError> {
        let (file, mut record) = self.lock_profile(id, stop).await?;
        check_stop(stop)?;
        record.advance_sign_in_epoch()?;
        let refresh_token = match record.state {
            State::SignedOut => {
                file.write(&record)?;
                return Ok(Disconnect::AlreadySignedOut);
            }
            State::LocalOnly
            | State::SignInUnknown
            | State::RefreshUnknown
            | State::InvalidRotation => {
                // The old token may be spent, and an untrusted replacement
                // is not authority to touch another account's session.
                record.state = State::LocalOnly;
                file.write(&record)?;
                return Ok(Disconnect::RemoteUnconfirmed);
            }
            State::Active(grant) | State::Verifying(grant) => grant.tokens.refresh_token,
            State::Revoking { refresh_token } => refresh_token,
        };
        record.state = State::Revoking { refresh_token };
        file.write(&record)?;
        let State::Revoking { refresh_token } = &record.state else {
            unreachable!("just fenced sign-out");
        };
        let revoked = match provider::client() {
            Ok(client) => tokio::select! {
                biased;
                () = stop.cancelled() => false,
                revoked = provider.revoke(&client, &record.client_id, refresh_token) => revoked,
            },
            Err(_) => false,
        };
        record.state = if revoked {
            State::SignedOut
        } else {
            State::LocalOnly
        };
        file.write(&record)?;
        Ok(if revoked {
            Disconnect::Revoked
        } else {
            Disconnect::RemoteUnconfirmed
        })
    }

    async fn profile_file(
        &self,
        id: &ProfileId,
        stop: &CancellationToken,
    ) -> Result<RecordFile<'_>, StoreError> {
        RecordFile::lock(&self.directory, &profile_name(id), stop).await
    }

    async fn lock_profile(
        &self,
        id: &ProfileId,
        stop: &CancellationToken,
    ) -> Result<(RecordFile<'_>, Record), StoreError> {
        let file = self.profile_file(id, stop).await?;
        let record = read_record(&file, id, self.host)?.ok_or(StoreError::Missing)?;
        Ok((file, record))
    }
}

async fn verify_rotation(
    file: RecordFile<'_>,
    mut record: Record,
    provider: &Provider<'_>,
    client: reqwest::Client,
    stop: &CancellationToken,
) -> Result<PlanAccess, StoreError> {
    // Own the staged grant without cloning its secrets. The in-memory
    // record is unusable until verification succeeds; on I/O failure the
    // durable staging record is untouched and can be verified again.
    let State::Verifying(active) = std::mem::replace(&mut record.state, State::InvalidRotation)
    else {
        unreachable!("only staged rotations are verified");
    };
    let now = provider::unix_now().map_err(|_| StoreError::Storage)?;
    if active.tokens.expires_at <= now {
        record.state = State::InvalidRotation;
        file.write(&record)?;
        return Err(StoreError::SignInRequired);
    }
    let verified = tokio::select! {
        biased;
        () = stop.cancelled() => return Err(StoreError::Cancelled),
        result = provider.identity(&client, &active.id_token, &record.client_id, None) => result,
    };
    match verified {
        Ok(verified)
            if record
                .identity
                .as_ref()
                .is_some_and(|old| old.sub == verified.sub) =>
        {
            record.identity = Some(verified);
            record.publish(&file, active, stop)
        }
        Err(SignInError::Network) => Err(StoreError::VerificationPending),
        _ => {
            record.state = State::InvalidRotation;
            file.write(&record)?;
            Err(StoreError::Identity)
        }
    }
}

fn access(tokens: TokenSet) -> PlanAccess {
    PlanAccess {
        token: tokens.access_token,
        expires_at: tokens.expires_at,
    }
}

fn profile_name(id: &ProfileId) -> String {
    // Opaque client IDs are case-sensitive even on a case-insensitive disk.
    format!("profile-{:x}", Sha256::digest(id.as_str().as_bytes()))
}

fn check_stop(stop: &CancellationToken) -> Result<(), StoreError> {
    if stop.is_cancelled() {
        Err(StoreError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
struct Record {
    client_id: String,
    host_id: String,
    // The issuer is fixed by this store. Identity here is from a verified
    // grant and is not mutable via the profile API.
    identity: Option<identity::Identity>,
    // A browser attempt may finish only at the epoch in which it started.
    // Advancing before dispatch also fences competing codes after a lost reply.
    #[serde(default)]
    sign_in_epoch: u64,
    state: State,
}

impl Record {
    fn new(id: &ProfileId, host: HostId) -> Self {
        Self {
            client_id: id.as_str().to_owned(),
            host_id: host.to_string(),
            identity: None,
            sign_in_epoch: 0,
            state: State::SignedOut,
        }
    }

    fn advance_sign_in_epoch(&mut self) -> Result<(), StoreError> {
        self.sign_in_epoch = self
            .sign_in_epoch
            .checked_add(1)
            .ok_or(StoreError::Storage)?;
        Ok(())
    }

    fn activate(&mut self, grant: Grant) -> Result<(), StoreError> {
        if self
            .identity
            .as_ref()
            .is_some_and(|old| old.sub != grant.identity.sub)
        {
            return Err(StoreError::ProfileMismatch);
        }
        if grant.tokens.expires_at <= provider::unix_now().map_err(|_| StoreError::Storage)? {
            return Err(StoreError::SignInRequired);
        }
        self.identity = Some(grant.identity);
        self.state = State::Active(CredentialSet {
            tokens: grant.tokens,
            id_token: grant.id_token,
        });
        Ok(())
    }

    fn publish(
        mut self,
        file: &RecordFile<'_>,
        active: CredentialSet,
        stop: &CancellationToken,
    ) -> Result<PlanAccess, StoreError> {
        let access = PlanAccess {
            token: active.tokens.access_token.clone(),
            expires_at: active.tokens.expires_at,
        };
        self.state = State::Active(active);
        file.write(&self)?;
        check_stop(stop)?;
        if access.expires_at <= provider::unix_now().map_err(|_| StoreError::Storage)? {
            return Err(StoreError::SignInRequired);
        }
        Ok(access)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", content = "credentials", rename_all = "snake_case")]
enum State {
    SignedOut,
    LocalOnly,
    SignInUnknown,
    Active(CredentialSet),
    RefreshUnknown,
    Verifying(CredentialSet),
    InvalidRotation,
    Revoking { refresh_token: String },
}

impl State {
    fn accepts_sign_in(&self) -> bool {
        matches!(
            self,
            Self::SignedOut
                | Self::LocalOnly
                | Self::SignInUnknown
                | Self::RefreshUnknown
                | Self::InvalidRotation
        )
    }

    fn status(&self) -> ConnectionStatus {
        match self {
            Self::SignedOut => ConnectionStatus::SignedOut,
            Self::LocalOnly => ConnectionStatus::RemoteUnconfirmed,
            Self::Active(_) => ConnectionStatus::Connected,
            Self::SignInUnknown | Self::RefreshUnknown | Self::InvalidRotation => {
                ConnectionStatus::NeedsSignIn
            }
            Self::Verifying(_) => ConnectionStatus::VerificationPending,
            Self::Revoking { .. } => ConnectionStatus::RevocationPending,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct CredentialSet {
    tokens: TokenSet,
    id_token: String,
}

fn read_record(
    file: &RecordFile<'_>,
    id: &ProfileId,
    host: HostId,
) -> Result<Option<Record>, StoreError> {
    let Some(record) = file.read::<Record>()? else {
        return Ok(None);
    };
    if record.client_id != id.as_str() || record.host_id.parse::<HostId>().ok() != Some(host) {
        return Err(StoreError::ProfileMismatch);
    }
    if record.identity.as_ref().is_some_and(|identity| {
        !visible_ascii(&identity.sub, 255)
            || identity
                .email
                .as_ref()
                .is_some_and(|email| email.len() > 320 || email.chars().any(char::is_control))
    }) {
        return Err(StoreError::Storage);
    }
    let valid = match &record.state {
        State::SignedOut | State::LocalOnly | State::SignInUnknown => true,
        State::RefreshUnknown | State::InvalidRotation => record.identity.is_some(),
        State::Active(active) | State::Verifying(active) => {
            record.identity.is_some() && active.tokens.valid() && valid_token(&active.id_token)
        }
        State::Revoking { refresh_token } => {
            record.identity.is_some() && valid_token(refresh_token)
        }
    };
    if !valid {
        return Err(StoreError::Storage);
    }
    Ok(Some(record))
}
