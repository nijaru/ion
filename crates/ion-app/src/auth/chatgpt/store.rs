//! Native custody for the local ChatGPT plan route. An account's issued client
//! ID selects one record; its verified subject cannot be replaced by another.
mod files;
#[cfg(test)]
mod tests;

use std::{
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{
    Grant, HostId, Registration, SignInError,
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
    #[error("disconnect this ChatGPT profile before replacing its current grant")]
    AlreadyConnected,
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

impl fmt::Display for ProfileId {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(out)
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
    registration: Registration,
    status: ConnectionStatus,
}

impl Profile {
    pub fn id(&self) -> &ProfileId {
        &self.id
    }
    pub fn registration(&self) -> &Registration {
        &self.registration
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

/// OS-permission-protected native storage (0700 directory, 0600 files). Use a
/// dedicated absolute directory under a trusted, existing parent on a local
/// filesystem. This does not encrypt data at rest, protect it from other
/// processes running as this user, or erase historical disk blocks. Do not
/// share, restore from a stale snapshot, or sync this directory across hosts.
#[derive(Clone)]
pub struct ProfileStore {
    root: PathBuf,
}

impl ProfileStore {
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Get or establish the local host ID. Call this before starting a new
    /// dynamic registration, and persist the issued registration with
    /// `remember` before consuming the authorization code.
    pub async fn host_id(&self, stop: &CancellationToken) -> Result<HostId, StoreError> {
        let file = self.host_file(stop).await?;
        if let Some(id) = read_host(&file)? {
            return Ok(id);
        }
        check_stop(stop)?;
        let id = HostId::generate().map_err(|_| StoreError::Storage)?;
        file.write(&id.to_string())?;
        Ok(id)
    }

    pub async fn remember(
        &self,
        registration: &Registration,
        stop: &CancellationToken,
    ) -> Result<ProfileId, StoreError> {
        self.check_host(registration.host, stop).await?;
        let id: ProfileId = registration.client.parse()?;
        let file = self.profile_file(&id, stop).await?;
        if let Some(saved) = read_record(&file, &id, registration.host)? {
            check_subject(saved.identity.as_ref(), registration.identity.as_ref())?;
        } else {
            check_stop(stop)?;
            // A callback is enough to remember the issued client, not an
            // identity assertion. Only installing a signed grant pins sub.
            file.write(&Record::new(&id, registration.host))?;
        }
        Ok(id)
    }

    /// Install a verified grant as an explicit account action. This refuses
    /// to overwrite a connected grant or an interrupted sign-out. The host
    /// must not submit an authorization that the user already abandoned.
    pub async fn install(
        &self,
        grant: Grant,
        stop: &CancellationToken,
    ) -> Result<ProfileId, StoreError> {
        let registration = &grant.registration;
        self.check_host(registration.host, stop).await?;
        let id: ProfileId = registration.client.parse()?;
        let file = self.profile_file(&id, stop).await?;
        let mut record = read_record(&file, &id, registration.host)?.ok_or(StoreError::Missing)?;
        check_subject(record.identity.as_ref(), registration.identity.as_ref())?;
        let identity = grant.registration.identity.ok_or(StoreError::Identity)?;
        if matches!(record.state, State::Active(_) | State::Revoking { .. }) {
            return Err(StoreError::AlreadyConnected);
        }
        if grant.tokens.expires_at <= provider::unix_now().map_err(|_| StoreError::Storage)? {
            return Err(StoreError::SignInRequired);
        }
        check_stop(stop)?;
        record.identity = Some(identity);
        record.state = State::Active(Active {
            tokens: grant.tokens,
            id_token: grant.id_token,
        });
        file.write(&record)?;
        Ok(id)
    }

    pub async fn profile(
        &self,
        id: &ProfileId,
        stop: &CancellationToken,
    ) -> Result<Profile, StoreError> {
        let (_file, record, host) = self.lock_profile(id, stop).await?;
        Ok(Profile {
            id: id.clone(),
            registration: Registration {
                host,
                client: record.client_id,
                identity: record.identity,
            },
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
        let (file, mut record, _) = self.lock_profile(id, stop).await?;
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
                record.state = State::RefreshUnknown(active);
                file.write(&record)?;
                let State::RefreshUnknown(old) = &record.state else {
                    unreachable!("just fenced the grant");
                };
                let result = tokio::select! {
                    biased;
                    () = stop.cancelled() => return Err(StoreError::RefreshUnknown),
                    result = provider.refresh(&client, &record.client_id, &old.tokens) => result,
                };
                match result {
                    Ok((tokens, Some(id_token))) => {
                        // Already rotated. Commit the new material before I/O
                        // to verify its signed identity. It isn't usable yet.
                        record.state = State::Verifying(Active { tokens, id_token });
                        file.write(&record)?;
                        verify_rotation(file, record, provider, client, stop).await
                    }
                    Ok((tokens, None)) => {
                        // Without a replacement ID token the verified identity
                        // is unchanged; keep the original hint and replace the
                        // entire renewable set in one durable publication.
                        let State::RefreshUnknown(old) = record.state else {
                            unreachable!("owned fenced grant");
                        };
                        record.state = State::Active(Active {
                            tokens,
                            id_token: old.id_token,
                        });
                        file.write(&record)?;
                        release_access(record, stop)
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
            State::RefreshUnknown(_) => Err(StoreError::RefreshUnknown),
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
        let (file, mut record, _) = self.lock_profile(id, stop).await?;
        check_stop(stop)?;
        let refresh_token = match record.state {
            State::SignedOut => return Ok(Disconnect::AlreadySignedOut),
            State::LocalOnly => return Ok(Disconnect::RemoteUnconfirmed),
            State::RefreshUnknown(_) | State::InvalidRotation => {
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

    async fn host_file(&self, stop: &CancellationToken) -> Result<RecordFile, StoreError> {
        RecordFile::lock(&self.root, "installation", stop).await
    }

    async fn check_host(
        &self,
        expected: HostId,
        stop: &CancellationToken,
    ) -> Result<(), StoreError> {
        let file = self.host_file(stop).await?;
        if read_host(&file)? != Some(expected) {
            return Err(StoreError::ProfileMismatch);
        }
        Ok(())
    }

    async fn profile_file(
        &self,
        id: &ProfileId,
        stop: &CancellationToken,
    ) -> Result<RecordFile, StoreError> {
        RecordFile::lock(&self.root, &profile_name(id), stop).await
    }

    async fn lock_profile(
        &self,
        id: &ProfileId,
        stop: &CancellationToken,
    ) -> Result<(RecordFile, Record, HostId), StoreError> {
        let host = {
            let file = self.host_file(stop).await?;
            read_host(&file)?.ok_or(StoreError::Missing)?
        };
        let file = self.profile_file(id, stop).await?;
        let record = read_record(&file, id, host)?.ok_or(StoreError::Missing)?;
        Ok((file, record, host))
    }
}

async fn verify_rotation(
    file: RecordFile,
    mut record: Record,
    provider: &Provider<'_>,
    client: reqwest::Client,
    stop: &CancellationToken,
) -> Result<PlanAccess, StoreError> {
    let State::Verifying(active) = record.state else {
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
            record.state = State::Active(active);
            file.write(&record)?;
            release_access(record, stop)
        }
        Err(SignInError::Network) => Err(StoreError::VerificationPending),
        _ => {
            record.state = State::InvalidRotation;
            file.write(&record)?;
            Err(StoreError::Identity)
        }
    }
}

fn release_access(record: Record, stop: &CancellationToken) -> Result<PlanAccess, StoreError> {
    check_stop(stop)?;
    let State::Active(grant) = record.state else {
        unreachable!("only published grants can be released");
    };
    if grant.tokens.expires_at <= provider::unix_now().map_err(|_| StoreError::Storage)? {
        return Err(StoreError::SignInRequired);
    }
    Ok(access(grant.tokens))
}

fn access(tokens: TokenSet) -> PlanAccess {
    PlanAccess {
        token: tokens.access_token,
        expires_at: tokens.expires_at,
    }
}

fn profile_name(id: &ProfileId) -> String {
    // Opaque client IDs are case-sensitive even on a case-insensitive disk.
    format!("profile-{:x}", Sha256::digest(id.0.as_bytes()))
}

fn check_stop(stop: &CancellationToken) -> Result<(), StoreError> {
    if stop.is_cancelled() {
        Err(StoreError::Cancelled)
    } else {
        Ok(())
    }
}

fn read_host(file: &RecordFile) -> Result<Option<HostId>, StoreError> {
    file.read::<String>()?
        .map(|value| value.parse().map_err(|_| StoreError::Storage))
        .transpose()
}

fn check_subject(
    previous: Option<&identity::Identity>,
    next: Option<&identity::Identity>,
) -> Result<(), StoreError> {
    if previous
        .zip(next)
        .is_some_and(|(old, new)| old.sub != new.sub)
    {
        return Err(StoreError::ProfileMismatch);
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct Record {
    client_id: String,
    host_id: String,
    // The issuer is fixed by this store. Identity here is from a verified
    // grant and is not mutable via the profile/registration API.
    identity: Option<identity::Identity>,
    state: State,
}

impl Record {
    fn new(id: &ProfileId, host: HostId) -> Self {
        Self {
            client_id: id.0.clone(),
            host_id: host.to_string(),
            identity: None,
            state: State::SignedOut,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "status", content = "credentials", rename_all = "snake_case")]
enum State {
    SignedOut,
    LocalOnly,
    Active(Active),
    RefreshUnknown(Active),
    Verifying(Active),
    InvalidRotation,
    Revoking { refresh_token: String },
}

impl State {
    fn status(&self) -> ConnectionStatus {
        match self {
            Self::SignedOut => ConnectionStatus::SignedOut,
            Self::LocalOnly => ConnectionStatus::RemoteUnconfirmed,
            Self::Active(_) => ConnectionStatus::Connected,
            Self::RefreshUnknown(_) | Self::InvalidRotation => ConnectionStatus::NeedsSignIn,
            Self::Verifying(_) => ConnectionStatus::VerificationPending,
            Self::Revoking { .. } => ConnectionStatus::RevocationPending,
        }
    }
}

#[derive(Deserialize, Serialize)]
struct Active {
    tokens: TokenSet,
    id_token: String,
}

fn read_record(
    file: &RecordFile,
    id: &ProfileId,
    host: HostId,
) -> Result<Option<Record>, StoreError> {
    let Some(record) = file.read::<Record>()? else {
        return Ok(None);
    };
    if record.client_id != id.0 || record.host_id.parse::<HostId>().ok() != Some(host) {
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
        State::SignedOut | State::LocalOnly => true,
        State::InvalidRotation => record.identity.is_some(),
        State::Active(active) | State::RefreshUnknown(active) | State::Verifying(active) => {
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
