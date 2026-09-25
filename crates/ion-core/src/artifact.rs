//! Session-owned auxiliary tool output. BlobRef is data, never publication authority.
//!
//! The database owns this resource lifetime (including its ownership lock). Escaped
//! publishers are weak, revocable capabilities. A publication scope holds the GC read
//! gate from before spooling through the queued Evidence transaction, not its waiter.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, OwnedRwLockReadGuard, RwLock};

use crate::store::{Ownership, StoreError};
use crate::{AttemptId, BlobRef, BlobStore, BlobStoreError, BlobStoreLimits, SessionId};

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("artifact publisher is no longer active or already published")]
    PublisherClosed,
    #[error("an artifact publication is already in progress for this attempt")]
    PublisherBusy,
    #[error(transparent)]
    Storage(#[from] BlobStoreError),
    #[error("artifact publication task failed: {0}")]
    Task(String),
}

/// Auxiliary loss does not change the committed result, effect, or Session health.
#[derive(Debug)]
pub enum ArtifactRead {
    Page {
        reference: BlobRef,
        offset: u64,
        bytes: Vec<u8>,
    },
    ContentUnavailable {
        reference: BlobRef,
        diagnostic: String,
    },
}

/// One finalized object per physical execution/reconciliation scope. This handle
/// cannot publish for another attempt, import refs, read history, or run GC.
#[derive(Debug, Clone)]
pub struct ArtifactPublisher {
    scope: Weak<AsyncMutex<PublicationState>>,
}

impl ArtifactPublisher {
    /// Stream on a blocking worker, enforcing Session quotas before durable publish.
    /// On failure the backend must retain terminal effect truth and report Incomplete
    /// output (for example OutputLoss::Quota), not an indeterminate execution.
    pub async fn publish<R: Read + Send + 'static>(
        &self,
        source: R,
    ) -> Result<BlobRef, ArtifactError> {
        let scope = self.scope.upgrade().ok_or(ArtifactError::PublisherClosed)?;
        // Do not let backend-owned, unpolled waiters block finish/revocation.
        let mut state = scope
            .try_lock_owned()
            .map_err(|_| ArtifactError::PublisherBusy)?;
        if state.closed.load(Ordering::Acquire) || state.reference.is_some() {
            return Err(ArtifactError::PublisherClosed);
        }
        let lease = state
            .lease
            .upgrade()
            .ok_or(ArtifactError::PublisherClosed)?;
        // Only admitted jobs retain the lease. Pending/escaped publisher futures
        // cannot extend namespace ownership after scope revocation.
        tokio::task::spawn_blocking(move || {
            let reference = lease.owner.store(true)?.publish(source)?;
            state.reference = Some(reference.clone());
            Ok(reference)
        })
        .await
        .map_err(|error| ArtifactError::Task(error.to_string()))?
    }

    #[cfg(test)]
    pub(crate) fn closed() -> Self {
        Self { scope: Weak::new() }
    }
}

#[derive(Debug)]
struct PublicationState {
    lease: Weak<PublicationLease>,
    reference: Option<BlobRef>,
    closed: Arc<AtomicBool>,
}

#[derive(Debug)]
struct PublicationLease {
    owner: Arc<SessionArtifacts>,
    attempt: AttemptId,
    _guard: OwnedRwLockReadGuard<()>,
}

pub(crate) struct PublicationScope {
    state: Arc<AsyncMutex<PublicationState>>,
    lease: Arc<PublicationLease>,
    closed: Arc<AtomicBool>,
}

impl Drop for PublicationScope {
    fn drop(&mut self) {
        // Revocation also covers panic/cancellation before finish; a publication
        // already running keeps its own gate until it finishes as an orphan.
        self.closed.store(true, Ordering::Release);
    }
}

impl PublicationScope {
    pub(crate) fn publisher(&self) -> ArtifactPublisher {
        ArtifactPublisher {
            scope: Arc::downgrade(&self.state),
        }
    }

    pub(crate) async fn finish(self) -> Option<PublishedBlob> {
        let mut state = self.state.lock().await;
        state.closed.store(true, Ordering::Release);
        let reference = state.reference.take()?;
        Some(PublishedBlob {
            lease: Arc::clone(&self.lease),
            reference,
        })
    }
}

/// Non-serializable, non-cloneable publication evidence. Only finish can construct it.
pub(crate) struct PublishedBlob {
    lease: Arc<PublicationLease>,
    reference: BlobRef,
}

impl PublishedBlob {
    pub(crate) fn reference(&self) -> &BlobRef {
        &self.reference
    }

    pub(crate) fn matches(
        &self,
        owner: &Arc<SessionArtifacts>,
        attempt: AttemptId,
        reference: &BlobRef,
    ) -> bool {
        Arc::ptr_eq(&self.lease.owner, owner)
            && self.lease.attempt == attempt
            && self.reference == *reference
    }
}

pub(crate) struct SessionArtifacts {
    namespace: PathBuf,
    limits: BlobStoreLimits,
    store: Mutex<Option<Arc<BlobStore>>>,
    pub(crate) gate: Arc<RwLock<()>>,
    // Publication/read/GC jobs retain exclusive Session ownership after a dropped
    // waiter or database shutdown. Dormant SessionHandles/publishers do not.
    _ownership: Ownership,
}

impl std::fmt::Debug for SessionArtifacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionArtifacts").finish_non_exhaustive()
    }
}

impl SessionArtifacts {
    pub(crate) fn new(
        database: &Path,
        session: SessionId,
        limits: BlobStoreLimits,
        ownership: Ownership,
    ) -> Result<Arc<Self>, StoreError> {
        let database =
            std::fs::canonicalize(database).map_err(|e| StoreError::Io(e.to_string()))?;
        let mut name = database.as_os_str().to_os_string();
        name.push(format!(".blobs-{session}"));
        Ok(Arc::new(Self {
            namespace: PathBuf::from(name),
            limits,
            store: Mutex::new(None),
            gate: Arc::new(RwLock::new(())),
            _ownership: ownership,
        }))
    }

    pub(crate) fn scope(
        self: &Arc<Self>,
        attempt: AttemptId,
        guard: OwnedRwLockReadGuard<()>,
    ) -> PublicationScope {
        let closed = Arc::new(AtomicBool::new(false));
        let lease = Arc::new(PublicationLease {
            owner: Arc::clone(self),
            attempt,
            _guard: guard,
        });
        PublicationScope {
            state: Arc::new(AsyncMutex::new(PublicationState {
                lease: Arc::downgrade(&lease),
                reference: None,
                closed: Arc::clone(&closed),
            })),
            lease,
            closed,
        }
    }

    /// No filesystem inspection or creation at Session open. Only explicit artifact
    /// operations initialize the namespace, adjacent to the host-owned database.
    pub(crate) fn store(&self, create: bool) -> Result<Arc<BlobStore>, BlobStoreError> {
        let mut slot = self
            .store
            .lock()
            .map_err(|_| BlobStoreError::StatePoisoned)?;
        if slot.is_none() {
            if create {
                crate::blob::ensure_session_namespace(&self.namespace)?;
            }
            *slot = Some(Arc::new(BlobStore::open(&self.namespace, self.limits)?));
        }
        // Only initialization holds this mutex. A slow publication source must not
        // block reads of unrelated immutable objects (or the database command thread).
        Ok(Arc::clone(slot.as_ref().expect("initialized blob store")))
    }

    pub(crate) fn read(
        &self,
        reference: BlobRef,
        offset: u64,
        max_length: usize,
    ) -> Result<ArtifactRead, StoreError> {
        match self
            .store(false)
            .and_then(|store| store.read_range(&reference, offset, max_length))
        {
            Ok(bytes) => Ok(ArtifactRead::Page {
                reference,
                offset,
                bytes,
            }),
            Err(
                error
                @ (BlobStoreError::InvalidRange { .. } | BlobStoreError::QuotaExceeded { .. }),
            ) => Err(StoreError::InvalidRequest(error.to_string())),
            Err(error) => Ok(ArtifactRead::ContentUnavailable {
                reference,
                diagnostic: error.to_string(),
            }),
        }
    }

    pub(crate) fn namespace_exists(&self) -> Result<bool, StoreError> {
        match std::fs::symlink_metadata(&self.namespace) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(StoreError::Io(error.to_string())),
        }
    }
}
