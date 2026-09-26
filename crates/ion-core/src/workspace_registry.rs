//! Host-owned coordination for cooperating local Unix writers, not confinement.
//!
//! All processes for a host user must use the same protected registry directory.
//! The host must keep that directory (and its ancestors) outside tool authority;
//! path checks cannot stop an unconfined shell or an external filesystem race.
//! Claims survive crashes, unknown outcomes and Session deletion. Only trusted
//! backend evidence may resolve them; dropping this handle has no semantic effect.
//!
//! Integration: `bind` supplies the frozen `WorkspaceBinding`; capture `revision`
//! during preparation, then `admit` before backend start. Retain the claim through
//! unknown outcomes and call `resolve` only after authenticating terminal backend
//! evidence. Success/error ToolResult status does not determine mutation certainty.
//! Records are limited to 16 KiB, binding count to 4096, and inspection pages to
//! 256 claims. Terminal history is retained without automatic pruning. Oversized
//! exact effects must be conservatively summarized before resolution.
//! Git discovery supports ordinary `.git` directories and linked-worktree
//! `gitdir`/`commondir` files, not environment-selected or bare repositories.
//! Non-Unix object binding fails closed. Filesystem identity uses device/inode and
//! creation time where available; this is not a race-free filesystem capability.

use crate::{AttemptId, ContentDigest, EffectSummary, InvocationId, SessionId, WorkspaceBinding};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;

#[cfg(all(test, unix))]
mod tests;

const MAX_RECORD: usize = 16 * 1024;
const MAX_BINDINGS: i64 = 4096;

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("workspace binding changed")]
    BindingChanged,
    #[error("registry namespace must be disjoint from every bound workspace and repository")]
    RegistryInWorkspace,
    #[error("conflicting unresolved claim")]
    Conflict,
    #[error("stale workspace/resource revision")]
    StaleRevision,
    #[error("record missing or evidence conflicts with retained evidence")]
    EvidenceConflict,
    #[error("registry capacity or record bound exceeded")]
    Capacity,
    #[error("unsupported registry format or filesystem platform")]
    Unsupported,
    #[error("invalid identity or resource claim")]
    Invalid,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
type Result<T> = std::result::Result<T, RegistryError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimKey {
    pub session: SessionId,
    pub invocation: InvocationId,
    pub attempt: AttemptId,
}

/// Repository claims include workspace files as well as shared Git metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceResources {
    Files,
    FilesAndRepository,
}

/// Both components must match at admission. Repository mutations invalidate all
/// bindings sharing that common directory, including otherwise isolated worktrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRevision {
    pub files: u64,
    pub repository: u64,
}

/// Inline backend receipt identity, never a Session blob reference. The host
/// authenticates its meaning under the frozen backend before calling this API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryReceipt {
    pub backend: String,
    pub identity: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalEvidence {
    pub receipt: RegistryReceipt,
    pub effect: EffectSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceClaim {
    pub key: ClaimKey,
    pub binding: WorkspaceBinding,
    pub resources: WorkspaceResources,
    pub base: WorkspaceRevision,
    pub start: Option<RegistryReceipt>,
    pub terminal: Option<TerminalEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Object {
    path: PathBuf,
    device: u64,
    inode: u64,
    created: Option<std::time::SystemTime>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Descriptor {
    root: Object,
    // Frozen physical ancestry distinguishes an overlapping original subtree
    // from a new object later installed under the same pathname.
    parents: Vec<Object>,
    git: Option<Object>,
    common: Option<Object>,
    common_parents: Vec<Object>,
}
#[derive(Serialize, Deserialize)]
struct BindingRecord {
    binding: WorkspaceBinding,
    descriptor: Descriptor,
}

pub struct WorkspaceRegistry {
    connection: Connection,
    directory: PathBuf,
}

impl WorkspaceRegistry {
    /// Open existing host state or initialize an empty host directory. Never put
    /// this directory in a checkout; `bind` rejects such placement before use.
    pub fn open(directory: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(directory.as_ref())?;
        let directory = fs::canonicalize(directory)?;
        // A Git checkout is recognizable even before its first registry binding.
        // Non-Git roots are checked when bound, before any claim can be admitted.
        if directory
            .ancestors()
            .any(|p| p.join(".git").symlink_metadata().is_ok())
        {
            return Err(RegistryError::RegistryInWorkspace);
        }
        let mut connection = Connection::open(directory.join("workspace-registry.sqlite"))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let application: i64 = tx.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        if version == 0 && application == 0 {
            let tables: i64 =
                tx.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
            if tables != 0 {
                return Err(RegistryError::Unsupported);
            }
            tx.execute_batch("CREATE TABLE bindings(id TEXT PRIMARY KEY, record TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE repositories(id TEXT PRIMARY KEY, revision INTEGER NOT NULL DEFAULT 0);
                CREATE TABLE claims(key TEXT PRIMARY KEY, session TEXT NOT NULL, invocation INTEGER NOT NULL, active INTEGER NOT NULL, record TEXT NOT NULL);
                CREATE INDEX active_claims ON claims(active);
                PRAGMA application_id=1229934162; PRAGMA user_version=3;")?;
        } else if version != 3 || application != 1229934162 {
            return Err(RegistryError::Unsupported);
        }
        tx.commit()?;
        Ok(Self {
            connection,
            directory,
        })
    }

    /// Freeze a local object. Reusing an id never adopts a replacement object.
    /// Explicit adoption uses a new id and does not remove old claims.
    pub fn bind(
        &mut self,
        id: &str,
        root: impl AsRef<Path>,
        backend: &str,
    ) -> Result<WorkspaceBinding> {
        short(id)?;
        short(backend)?;
        let descriptor = describe(root.as_ref())?;
        outside(&self.directory, &descriptor)?;
        let binding = WorkspaceBinding {
            id: id.into(),
            canonical_root: path_text(&descriptor.root.path)?.into(),
            backend: backend.into(),
            object_identity: ContentDigest::of(&descriptor)?.to_string(),
        };
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(old) = load_binding(&tx, id)? {
            if old.binding != binding || old.descriptor != descriptor {
                return Err(RegistryError::BindingChanged);
            }
        } else {
            let count: i64 = tx.query_row("SELECT count(*) FROM bindings", [], |r| r.get(0))?;
            if count >= MAX_BINDINGS {
                return Err(RegistryError::Capacity);
            }
            tx.execute(
                "INSERT INTO bindings(id,record) VALUES(?1,?2)",
                params![
                    id,
                    encode(&BindingRecord {
                        binding: binding.clone(),
                        descriptor: descriptor.clone()
                    })?
                ],
            )?;
            if let Some(common) = &descriptor.common {
                tx.execute(
                    "INSERT OR IGNORE INTO repositories(id) VALUES(?1)",
                    [repository_key(common)?],
                )?;
            }
        }
        tx.commit()?;
        Ok(binding)
    }

    /// Authenticate a frozen binding against the registry and its current physical
    /// descriptor without admitting a mutation. This is a preflight, not a filesystem
    /// sandbox or a guarantee against later same-user namespace changes.
    pub fn verify_current(&self, binding: &WorkspaceBinding) -> Result<()> {
        let record = checked_binding(&self.connection, binding)?;
        let current = describe(Path::new(&binding.canonical_root))?;
        if current != record.descriptor {
            return Err(RegistryError::BindingChanged);
        }
        outside(&self.directory, &record.descriptor)?;
        Ok(())
    }

    pub fn revision(&self, binding: &WorkspaceBinding) -> Result<WorkspaceRevision> {
        let tx = self.connection.unchecked_transaction()?;
        let record = checked_binding(&tx, binding)?;
        let result = revision(&tx, &record)?;
        tx.commit()?;
        Ok(result)
    }

    /// Persist quarantine BEFORE backend start. A returned claim is not permission
    /// to bypass live policy/effect-gate checks. Repeated admission is rejected.
    pub fn admit(
        &mut self,
        binding: &WorkspaceBinding,
        key: ClaimKey,
        resources: WorkspaceResources,
        expected: WorkspaceRevision,
    ) -> Result<WorkspaceClaim> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = checked_binding(&tx, binding)?;
        outside(&self.directory, &record.descriptor)?;
        if describe(Path::new(&binding.canonical_root)).ok().as_ref() != Some(&record.descriptor) {
            return Err(RegistryError::BindingChanged);
        }
        if resources == WorkspaceResources::FilesAndRepository && record.descriptor.common.is_none()
        {
            return Err(RegistryError::Invalid);
        }
        if revision(&tx, &record)? != expected {
            return Err(RegistryError::StaleRevision);
        }
        let mut statement = tx.prepare("SELECT record FROM claims WHERE active=1")?;
        let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
        for row in rows {
            let claim: WorkspaceClaim = decode(&row?)?;
            let other = checked_binding(&tx, &claim.binding)?;
            if (claim.key.session == key.session && claim.key.invocation == key.invocation)
                || physical_overlap(&record.descriptor, &other.descriptor)
                || overlaps(&record.descriptor.root.path, &other.descriptor.root.path)
                || (resources == WorkspaceResources::FilesAndRepository
                    && claim.resources == resources
                    && record
                        .descriptor
                        .common
                        .as_ref()
                        .zip(other.descriptor.common.as_ref())
                        .is_some_and(|(a, b)| {
                            objects_overlap(
                                a,
                                &record.descriptor.common_parents,
                                b,
                                &other.descriptor.common_parents,
                            ) || overlaps(&a.path, &b.path)
                        }))
            {
                return Err(RegistryError::Conflict);
            }
        }
        drop(statement);
        let claim = WorkspaceClaim {
            key,
            binding: binding.clone(),
            resources,
            base: expected,
            start: None,
            terminal: None,
        };
        tx.execute(
            "INSERT INTO claims(key,session,invocation,active,record) VALUES(?1,?2,?3,1,?4)",
            params![
                encode(&key)?,
                key.session.to_string(),
                key.invocation.get(),
                encode(&claim)?
            ],
        )?;
        tx.commit()?;
        Ok(claim)
    }

    /// Inspect by attribution even when the Session and all blobs are gone.
    pub fn claim(&self, key: ClaimKey) -> Result<WorkspaceClaim> {
        load_claim(&self.connection, key)
    }

    /// Bounded orphan discovery. Cursor is the last returned serialized ClaimKey;
    /// use `claim_cursor` rather than interpreting it. No Session lookup occurs.
    pub fn unresolved(&self, after: Option<&str>, limit: usize) -> Result<Vec<WorkspaceClaim>> {
        if limit > 256 || after.is_some_and(|s| s.len() > 256) {
            return Err(RegistryError::Capacity);
        }
        let mut stmt = self
            .connection
            .prepare("SELECT record FROM claims WHERE active=1 AND key>?1 ORDER BY key LIMIT ?2")?;
        let rows = stmt.query_map(
            params![
                after.unwrap_or(""),
                i64::try_from(limit).map_err(|_| RegistryError::Capacity)?
            ],
            |r| r.get::<_, String>(0),
        )?;
        rows.map(|r| decode(&r?)).collect()
    }

    pub fn claim_cursor(key: ClaimKey) -> Result<String> {
        encode(&key)
    }

    pub fn record_start(&mut self, key: ClaimKey, receipt: RegistryReceipt) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut claim = load_claim(&tx, key)?;
        validate_receipt(&receipt, &claim.binding)?;
        if claim.start.as_ref() == Some(&receipt) {
            return Ok(());
        }
        if claim.start.is_some() || claim.terminal.is_some() {
            return Err(RegistryError::EvidenceConflict);
        }
        claim.start = Some(receipt);
        tx.execute(
            "UPDATE claims SET record=?2 WHERE key=?1",
            params![encode(&key)?, encode(&claim)?],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Trusted host attests terminal execution (or authoritative not-started with
    /// NoMutation). Unknown/accepted-unknown has no resolution API. Receipt effects
    /// are conservatively revision-advancing; no binding-specific interpretation.
    /// Exact duplicate evidence is idempotent; contradictory evidence is rejected.
    pub fn resolve(&mut self, key: ClaimKey, evidence: TerminalEvidence) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut claim = load_claim(&tx, key)?;
        validate_receipt(&evidence.receipt, &claim.binding)?;
        if let Some(old) = &claim.terminal {
            return if old == &evidence {
                Ok(())
            } else {
                Err(RegistryError::EvidenceConflict)
            };
        }
        let mutated = !matches!(evidence.effect, EffectSummary::NoMutation);
        claim.terminal = Some(evidence);
        let encoded = encode(&claim)?;
        if mutated {
            let record = checked_binding(&tx, &claim.binding)?;
            // Invalidate overlapping roots too, but never transfer revision to a
            // replacement at the same pathname with a different frozen identity.
            let mut stmt = tx.prepare("SELECT record FROM bindings")?;
            let records = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            drop(stmt);
            let mut repositories = BTreeSet::new();
            for value in records {
                let other: BindingRecord = decode(&value)?;
                if physical_overlap(&record.descriptor, &other.descriptor) {
                    advance(&tx, "bindings", &other.binding.id)?;
                }
                if claim.resources == WorkspaceResources::FilesAndRepository {
                    let common = record
                        .descriptor
                        .common
                        .as_ref()
                        .ok_or(RegistryError::Invalid)?;
                    if let Some(other_common) = &other.descriptor.common
                        && objects_overlap(
                            common,
                            &record.descriptor.common_parents,
                            other_common,
                            &other.descriptor.common_parents,
                        )
                    {
                        repositories.insert(repository_key(other_common)?);
                    }
                }
            }
            // Several worktree bindings can name the same repository resource.
            // Each physical resource advances exactly once per terminal claim.
            for repository in repositories {
                advance(&tx, "repositories", &repository)?;
            }
        }
        tx.execute(
            "UPDATE claims SET active=0,record=?2 WHERE key=?1",
            params![encode(&key)?, encoded],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn advance(connection: &Connection, table: &str, id: &str) -> Result<()> {
    let changed = connection.execute(
        &format!(
            "UPDATE {table} SET revision=revision+1 WHERE id=?1 AND revision<9223372036854775807"
        ),
        [id],
    )?;
    if changed != 1 {
        return Err(RegistryError::Capacity);
    }
    Ok(())
}
fn revision(connection: &Connection, record: &BindingRecord) -> Result<WorkspaceRevision> {
    let files: i64 = connection.query_row(
        "SELECT revision FROM bindings WHERE id=?1",
        [&record.binding.id],
        |r| r.get(0),
    )?;
    let repository: i64 = if let Some(common) = &record.descriptor.common {
        connection.query_row(
            "SELECT revision FROM repositories WHERE id=?1",
            [repository_key(common)?],
            |r| r.get(0),
        )?
    } else {
        0
    };
    Ok(WorkspaceRevision {
        files: u64::try_from(files).map_err(|_| RegistryError::Invalid)?,
        repository: u64::try_from(repository).map_err(|_| RegistryError::Invalid)?,
    })
}
fn load_binding(connection: &Connection, id: &str) -> Result<Option<BindingRecord>> {
    let value: Option<String> = connection
        .query_row("SELECT record FROM bindings WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()?;
    value.map(|s| decode(&s)).transpose()
}
fn checked_binding(connection: &Connection, binding: &WorkspaceBinding) -> Result<BindingRecord> {
    let record = load_binding(connection, &binding.id)?.ok_or(RegistryError::BindingChanged)?;
    if record.binding != *binding {
        return Err(RegistryError::BindingChanged);
    }
    Ok(record)
}
fn load_claim(connection: &Connection, key: ClaimKey) -> Result<WorkspaceClaim> {
    let value: Option<String> = connection
        .query_row(
            "SELECT record FROM claims WHERE key=?1",
            [encode(&key)?],
            |r| r.get(0),
        )
        .optional()?;
    decode(&value.ok_or(RegistryError::EvidenceConflict)?)
}
fn encode(value: &impl Serialize) -> Result<String> {
    struct Bounded(Vec<u8>);
    impl std::io::Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_RECORD - self.0.len() {
                return Err(std::io::Error::other("registry record bound"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = Bounded(Vec::new());
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        return Err(if error.is_io() {
            RegistryError::Capacity
        } else {
            error.into()
        });
    }
    String::from_utf8(writer.0).map_err(|_| RegistryError::Invalid)
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T> {
    if value.len() > MAX_RECORD {
        return Err(RegistryError::Capacity);
    }
    Ok(serde_json::from_str(value)?)
}
fn short(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 160 {
        return Err(RegistryError::Invalid);
    }
    Ok(())
}
fn validate_receipt(receipt: &RegistryReceipt, binding: &WorkspaceBinding) -> Result<()> {
    short(&receipt.identity)?;
    if receipt.backend != binding.backend {
        return Err(RegistryError::EvidenceConflict);
    }
    Ok(())
}
fn physical_overlap(a: &Descriptor, b: &Descriptor) -> bool {
    objects_overlap(&a.root, &a.parents, &b.root, &b.parents)
}
fn objects_overlap(a: &Object, a_parents: &[Object], b: &Object, b_parents: &[Object]) -> bool {
    same_object(a, b)
        || a_parents.iter().any(|parent| same_object(parent, b))
        || b_parents.iter().any(|parent| same_object(parent, a))
}
fn same_object(a: &Object, b: &Object) -> bool {
    (a.device, a.inode, a.created) == (b.device, b.inode, b.created)
}
fn repository_key(object: &Object) -> Result<String> {
    encode(&(object.device, object.inode, object.created))
}
fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn outside(directory: &Path, descriptor: &Descriptor) -> Result<()> {
    for object in std::iter::once(&descriptor.root)
        .chain(descriptor.git.iter())
        .chain(descriptor.common.iter())
    {
        if directory.starts_with(&object.path) || object.path.starts_with(directory) {
            return Err(RegistryError::RegistryInWorkspace);
        }
    }
    Ok(())
}
fn path_text(path: &Path) -> Result<&str> {
    path.to_str().ok_or(RegistryError::Invalid)
}
fn object(path: &Path) -> Result<Object> {
    let path = fs::canonicalize(path)?;
    let metadata = fs::metadata(&path)?;
    if !metadata.is_dir() {
        return Err(RegistryError::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(Object {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            created: metadata.created().ok(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = (path, metadata);
        Err(RegistryError::Unsupported)
    }
}
fn small_file(path: &Path) -> Result<String> {
    // Git marker reads are host preflight, before a tool's own leaf checks. Never
    // block on a static FIFO or follow a marker symlink into a device/host path.
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(RegistryError::Invalid);
    }
    #[cfg(unix)]
    let file = fs::File::from(
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    #[cfg(not(unix))]
    return Err(RegistryError::Unsupported);
    if !file.metadata()?.is_file() {
        return Err(RegistryError::Invalid);
    }
    let mut value = String::new();
    file.take((MAX_RECORD + 1) as u64)
        .read_to_string(&mut value)?;
    if value.len() > MAX_RECORD {
        return Err(RegistryError::Capacity);
    }
    Ok(value)
}
fn describe(root: &Path) -> Result<Descriptor> {
    let root = object(root)?;
    let parents = root
        .path
        .ancestors()
        .skip(1)
        .map(object)
        .collect::<Result<Vec<_>>>()?;
    let mut git = None;
    for ancestor in root.path.ancestors() {
        let marker = ancestor.join(".git");
        match fs::symlink_metadata(&marker) {
            Ok(metadata) => {
                if !metadata.is_dir() && !metadata.file_type().is_file() {
                    return Err(RegistryError::Invalid);
                }
                git = Some(if metadata.is_dir() {
                    object(&marker)?
                } else {
                    let text = small_file(&marker)?;
                    let target = text
                        .trim()
                        .strip_prefix("gitdir: ")
                        .ok_or(RegistryError::Invalid)?;
                    object(&ancestor.join(target))?
                });
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let common = if let Some(git) = &git {
        let marker = git.path.join("commondir");
        match small_file(&marker) {
            Ok(value) => Some(object(&git.path.join(value.trim()))?),
            Err(RegistryError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Some(git.clone())
            }
            Err(e) => return Err(e),
        }
    } else {
        None
    };
    let common_parents = common
        .as_ref()
        .map(|common| {
            common
                .path
                .ancestors()
                .skip(1)
                .map(object)
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(Descriptor {
        root,
        parents,
        git,
        common,
        common_parents,
    })
}
