//! Native bounded single-file replacement ToolBoundary.
//!
//! This is host-protected filesystem access, not an OS sandbox or race-free
//! compare-and-swap against uncooperative writers. The host must protect the
//! workspace namespace and registry from third-party concurrent renames.

use std::{
    fs::File,
    io::{Read, Write},
    os::{
        fd::OwnedFd,
        unix::fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use ion_ai::ToolSpec;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, fchmod, fstat, open, openat, renameat, statat};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use crate::{
    ApprovalState, BaseFact, ContentDigest, EgressRealm, LiveToolAuthority, PreparedAction,
    SemanticCompatibilityId, StartReceipt, ToolAttempt, ToolAttemptState, ToolAuthority,
    ToolBinding, ToolBindingId, ToolBoundary, ToolBoundaryError, ToolConcurrency, ToolExecution,
    ToolRecoveryPolicy, ToolResult, WorkspaceBinding,
    workspace_registry::{
        ClaimKey, RegistryError, RegistryReceipt, TerminalEvidence, WorkspaceRegistry,
        WorkspaceResources, WorkspaceRevision,
    },
};

/// Hard maximum for the complete source and replacement target.
pub const MAX_NATIVE_EDIT_BYTES: usize = 16 * 1024;

const MAX_PATH_BYTES: usize = 4096;
const IMPLEMENTATION_ID: &str = "native-edit-v1";
const AUTHORITY_ALLOW: u8 = 0;
const AUTHORITY_ASK: u8 = 1;
const AUTHORITY_DENY: u8 = 2;

#[cfg(test)]
const FAULT_AFTER_RENAME: u8 = 1;
#[cfg(test)]
const FAULT_AFTER_REGISTRY_COMMIT: u8 = 2;
#[cfg(test)]
const FAULT_RECOVERY_DIRECTORY_SYNC: u8 = 3;

/// Native exact-text editor bound to one frozen tool and workspace.
pub struct NativeEditBoundary {
    binding: ToolBinding,
    workspace: WorkspaceBinding,
    executor: SemanticCompatibilityId,
    root: File,
    root_identity: PhysicalIdentity,
    registry_directory: PathBuf,
    resources: WorkspaceResources,
    protected_paths: Vec<PathBuf>,
    max_file_bytes: usize,
    live_authority: Arc<AtomicU8>,
    permits: Arc<Semaphore>,
    #[cfg(test)]
    fault: Arc<AtomicU8>,
}

impl std::fmt::Debug for NativeEditBoundary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeEditBoundary")
            .field("binding", &self.binding.id)
            .field("workspace", &self.workspace.id)
            .field("resources", &self.resources)
            .field("max_file_bytes", &self.max_file_bytes)
            .finish_non_exhaustive()
    }
}

impl NativeEditBoundary {
    /// Authenticate a registry-owned binding and pin the root descriptor. The
    /// host must protect the namespace from external concurrent renames. Creation
    /// time is required so recovery can distinguish inode reuse; unsupported
    /// filesystems fail closed instead of exposing a weaker recovery proof.
    pub fn new(
        registry: &WorkspaceRegistry,
        workspace: WorkspaceBinding,
        max_file_bytes: usize,
    ) -> Result<Self, NativeEditError> {
        if !(1..=MAX_NATIVE_EDIT_BYTES).contains(&max_file_bytes) {
            return Err(NativeEditError::InvalidFileLimit);
        }
        if workspace.id.is_empty()
            || workspace.object_identity.is_empty()
            || workspace.canonical_root.len() > MAX_PATH_BYTES
        {
            return Err(NativeEditError::InvalidWorkspace);
        }
        registry.verify_current(&workspace)?;
        let executor = SemanticCompatibilityId::new(workspace.backend.clone())
            .map_err(|_| NativeEditError::InvalidWorkspace)?;
        let root = open_absolute_directory(&workspace.canonical_root)?;
        let root_identity = physical_identity(&root).map_err(|error| {
            if error.kind() == std::io::ErrorKind::Unsupported {
                NativeEditError::UnsupportedIdentity
            } else {
                NativeEditError::Io(error)
            }
        })?;
        let resources = registry.mutation_resources(&workspace)?;
        let protected_paths = registry.protected_mutation_paths(&workspace)?;
        if protected_paths
            .iter()
            .any(|admin| Path::new(&workspace.canonical_root).starts_with(admin))
        {
            return Err(NativeEditError::InvalidWorkspace);
        }
        registry.verify_current(&workspace)?;

        Ok(Self {
            binding: native_edit_binding().map_err(NativeEditError::InvalidBinding)?,
            workspace,
            executor,
            root,
            root_identity,
            registry_directory: registry.directory().to_path_buf(),
            resources,
            protected_paths,
            max_file_bytes,
            live_authority: Arc::new(AtomicU8::new(AUTHORITY_DENY)),
            permits: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            fault: Arc::new(AtomicU8::new(0)),
        })
    }

    /// Set current host policy. Admission is rechecked after durable registry
    /// claim and staging, immediately before the atomic replacement.
    pub fn set_live_authority(&self, authority: LiveToolAuthority) {
        let value = match authority {
            LiveToolAuthority::Allow => AUTHORITY_ALLOW,
            LiveToolAuthority::Ask => AUTHORITY_ASK,
            LiveToolAuthority::Deny => AUTHORITY_DENY,
        };
        self.live_authority.store(value, Ordering::SeqCst);
    }

    #[must_use]
    pub fn tool_binding(&self) -> &ToolBinding {
        &self.binding
    }

    #[must_use]
    pub fn workspace_binding(&self) -> &WorkspaceBinding {
        &self.workspace
    }

    fn ordinary_path(&self, path: &str) -> bool {
        valid_relative_path(path)
            && !self.protected_paths.iter().any(|admin| {
                Path::new(&self.workspace.canonical_root)
                    .join(path)
                    .starts_with(admin)
            })
    }

    fn prepared_arguments(&self, action: &PreparedAction) -> Option<EditArguments> {
        if action.binding != self.binding.id
            || action.authority != ToolAuthority::WorkspaceMutation
            || action.egress != EgressRealm::Local
            || action.workspace_revision.is_none()
        {
            return None;
        }
        let arguments: EditArguments = serde_json::from_value(action.arguments.clone()).ok()?;
        if !self.ordinary_path(&arguments.path)
            || arguments.path.len() > MAX_PATH_BYTES
            || arguments.expected_content.len() > self.max_file_bytes
            || arguments.old_text.len() > self.max_file_bytes
            || arguments.new_text.len() > self.max_file_bytes
            || arguments.expected_digest != digest_text(&arguments.expected_content)
            || action.workspace_revision != Some(arguments.base_revision.files)
        {
            return None;
        }
        let replacement = replacement(&arguments, self.max_file_bytes).ok()?;
        let facts = vec![BaseFact {
            path: arguments.path.clone(),
            digest: ContentDigest::of_bytes(arguments.expected_content.as_bytes()),
        }];
        if action.base_facts != facts {
            return None;
        }
        let canonical = serde_json::to_value(&arguments).ok()?;
        let expected = PreparedAction::new(
            self.binding.id.clone(),
            canonical,
            EgressRealm::Local,
            ToolAuthority::WorkspaceMutation,
            Some(arguments.base_revision.files),
            facts,
        )
        .ok()?;
        (expected == *action && replacement.len() <= self.max_file_bytes).then_some(arguments)
    }

    fn sync_recovered_directory(&self, parent: &File) -> std::io::Result<()> {
        #[cfg(test)]
        if self
            .fault
            .compare_exchange(
                FAULT_RECOVERY_DIRECTORY_SYNC,
                0,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
        {
            return Err(std::io::Error::other(
                "injected recovery directory sync failure",
            ));
        }
        parent.sync_all()
    }

    fn workspace_is_current(&self, workspace: &WorkspaceBinding) -> bool {
        if workspace != &self.workspace
            || !matches!(
                open_absolute_directory(&self.workspace.canonical_root)
                    .and_then(|root| physical_identity(&root)),
                Ok(identity) if identity == self.root_identity
            )
        {
            return false;
        }
        WorkspaceRegistry::open(&self.registry_directory)
            .and_then(|registry| registry.verify_current(workspace))
            .is_ok()
    }

    #[cfg(test)]
    fn inject_fault(&self, fault: u8) {
        self.fault.store(fault, Ordering::SeqCst);
    }
}

impl ToolBoundary for NativeEditBoundary {
    fn binding(&self) -> ToolBinding {
        self.binding.clone()
    }

    fn executor(&self) -> SemanticCompatibilityId {
        self.executor.clone()
    }

    fn prepare(&self, arguments: Value) -> Result<PreparedAction, ToolBoundaryError> {
        let mut arguments: EditArguments =
            serde_json::from_value(arguments).map_err(|_| ToolBoundaryError::InvalidArguments)?;
        // The model supplies the exact base, not a redundant digest it cannot
        // reliably compute. Freeze the digest in the canonical prepared action.
        arguments.expected_digest = digest_text(&arguments.expected_content);
        if !self.ordinary_path(&arguments.path)
            || arguments.path.len() > MAX_PATH_BYTES
            || arguments.expected_content.len() > self.max_file_bytes
            || arguments.old_text.len() > self.max_file_bytes
            || arguments.new_text.len() > self.max_file_bytes
            || arguments.expected_digest != digest_text(&arguments.expected_content)
        {
            return Err(ToolBoundaryError::InvalidArguments);
        }
        replacement(&arguments, self.max_file_bytes)
            .map_err(|_| ToolBoundaryError::InvalidArguments)?;
        let facts = vec![BaseFact {
            path: arguments.path.clone(),
            digest: ContentDigest::of_bytes(arguments.expected_content.as_bytes()),
        }];
        let action = PreparedAction::new(
            self.binding.id.clone(),
            serde_json::to_value(arguments.clone())
                .map_err(|_| ToolBoundaryError::InvalidAction)?,
            EgressRealm::Local,
            ToolAuthority::WorkspaceMutation,
            Some(arguments.base_revision.files),
            facts,
        )
        .map_err(|_| ToolBoundaryError::InvalidAction)?;
        crate::tool_boundary::bounded(&action)?;
        Ok(action)
    }

    fn live_authority(
        &self,
        action: &PreparedAction,
        workspace: &WorkspaceBinding,
    ) -> LiveToolAuthority {
        if self.prepared_arguments(action).is_none() || !self.workspace_is_current(workspace) {
            return LiveToolAuthority::Deny;
        }
        decode_authority(self.live_authority.load(Ordering::SeqCst))
    }

    fn execute<'a>(
        &'a self,
        execution: ToolExecution,
        stop: CancellationToken,
    ) -> ion_ai::BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move {
            let Some(arguments) = self.prepared_arguments(&execution.action) else {
                return not_started("invalid or incompatible prepared edit action");
            };
            if execution.binding != self.binding
                || execution.workspace != self.workspace
                || execution.action.authority != ToolAuthority::WorkspaceMutation
                || !execution.action.permitted_by(&execution.ceiling)
            {
                return not_started("edit binding, workspace, or authority changed");
            }
            if stop.is_cancelled() {
                return not_started("edit cancelled before admission");
            }
            let permit = tokio::select! {
                biased;
                () = stop.cancelled() => return not_started("edit cancelled before admission"),
                permit = Arc::clone(&self.permits).acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(_) => return not_started("edit backend is shutting down"),
                },
            };
            if stop.is_cancelled() {
                return not_started("edit cancelled before admission");
            }
            let root = match self.root.try_clone() {
                Ok(root) => root,
                Err(_) => return not_started("workspace root is no longer available"),
            };
            let drop_cancellation = CancelWorkerOnDrop::new(stop.clone());
            let job = EditJob {
                execution,
                arguments,
                root,
                root_identity: self.root_identity,
                registry_directory: self.registry_directory.clone(),
                resources: self.resources,
                live_authority: Arc::clone(&self.live_authority),
                binding: self.binding.clone(),
                executor: self.executor.clone(),
                stop,
                max_file_bytes: self.max_file_bytes,
                _permit: permit,
                #[cfg(test)]
                fault: Arc::clone(&self.fault),
            };
            let result = tokio::task::spawn_blocking(move || run_edit(job)).await;
            drop_cancellation.disarm();
            match result {
                Ok(state) => state,
                Err(_) => indeterminate("edit worker ended without terminal evidence", None),
            }
        })
    }

    fn reconcile<'a>(
        &'a self,
        execution: ToolExecution,
        attempt: ToolAttempt,
    ) -> ion_ai::BoxFuture<'a, ToolAttemptState> {
        Box::pin(async move { reconcile_edit(self, execution, attempt).await })
    }
}

/// Construct the frozen declaration for the native `edit` tool.
pub fn native_edit_binding() -> Result<ToolBinding, crate::ConfigError> {
    ToolBinding::new(
        ToolBindingId::new("edit")?,
        ToolSpec {
            name: "edit".into(),
            description: "Replace one exact occurrence in a regular file of at most 16 KiB under the bound workspace root. Provide the complete expected file and workspace_revision from read.".into(),
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["path", "expected_content", "workspace_revision", "old_text", "new_text"],
                "properties": {
                    "path": {"type": "string", "minLength": 1, "maxLength": MAX_PATH_BYTES},
                    "expected_content": {"type": "string", "maxLength": MAX_NATIVE_EDIT_BYTES},
                    "workspace_revision": {
                        "type": "object", "additionalProperties": false,
                        "required": ["files", "repository"],
                        "properties": {
                            "files": {"type": "integer", "minimum": 0, "maximum": i64::MAX},
                            "repository": {"type": "integer", "minimum": 0, "maximum": i64::MAX}
                        }
                    },
                    "old_text": {"type": "string", "minLength": 1, "maxLength": MAX_NATIVE_EDIT_BYTES},
                    "new_text": {"type": "string", "maxLength": MAX_NATIVE_EDIT_BYTES}
                }
            }),
        },
        SemanticCompatibilityId::new(IMPLEMENTATION_ID)?,
        ToolConcurrency::Serial,
        ToolRecoveryPolicy::NeverRepeat,
        EgressRealm::Local,
    )
}

/// Constructor and frozen-workspace validation errors.
#[derive(Debug, Error)]
pub enum NativeEditError {
    #[error("invalid native edit workspace binding")]
    InvalidWorkspace,
    #[error("native edit file limit must be between 1 and 16384 bytes")]
    InvalidFileLimit,
    #[error("filesystem does not provide trustworthy creation-time identity")]
    UnsupportedIdentity,
    #[error("failed to construct the native edit binding: {0}")]
    InvalidBinding(#[source] crate::ConfigError),
    #[error("workspace registry binding is not current: {0}")]
    Registry(#[from] RegistryError),
    #[error("failed to open canonical workspace root: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EditArguments {
    path: String,
    expected_content: String,
    #[serde(default)]
    expected_digest: String,
    #[serde(rename = "workspace_revision")]
    base_revision: EditRevision,
    old_text: String,
    new_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EditRevision {
    files: u64,
    repository: u64,
}

impl From<WorkspaceRevision> for EditRevision {
    fn from(value: WorkspaceRevision) -> Self {
        Self {
            files: value.files,
            repository: value.repository,
        }
    }
}

impl From<EditRevision> for WorkspaceRevision {
    fn from(value: EditRevision) -> Self {
        Self {
            files: value.files,
            repository: value.repository,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PhysicalIdentity {
    device: u64,
    inode: u64,
    birth_seconds: u64,
    birth_nanos: u32,
}

struct EditJob {
    execution: ToolExecution,
    arguments: EditArguments,
    root: File,
    root_identity: PhysicalIdentity,
    registry_directory: PathBuf,
    resources: WorkspaceResources,
    live_authority: Arc<AtomicU8>,
    binding: ToolBinding,
    executor: SemanticCompatibilityId,
    stop: CancellationToken,
    max_file_bytes: usize,
    _permit: tokio::sync::OwnedSemaphorePermit,
    #[cfg(test)]
    fault: Arc<AtomicU8>,
}

fn run_edit(job: EditJob) -> ToolAttemptState {
    if job.stop.is_cancelled() {
        return not_started("edit cancelled before admission");
    }
    match open_absolute_directory(&job.execution.workspace.canonical_root)
        .and_then(|root| physical_identity(&root))
    {
        Ok(identity) if identity == job.root_identity => {}
        _ => return not_started("workspace root identity changed before edit admission"),
    }
    let mut registry = match WorkspaceRegistry::open(&job.registry_directory) {
        Ok(registry) => registry,
        Err(_) => return not_started("workspace registry is unavailable"),
    };
    if registry.verify_current(&job.execution.workspace).is_err() {
        return not_started("workspace binding changed before edit admission");
    }
    let current_revision = match registry.revision(&job.execution.workspace) {
        Ok(revision) => revision,
        Err(_) => return not_started("workspace revision is unavailable"),
    };
    if EditRevision::from(current_revision) != job.arguments.base_revision
        || current_revision.files != job.execution.action.workspace_revision.unwrap_or(u64::MAX)
    {
        return not_started("workspace base revision is stale");
    }
    if !matches!(
        registry.mutation_resources(&job.execution.workspace),
        Ok(resources) if resources == job.resources
    ) {
        return not_started("workspace resource scope changed");
    }
    let replacement = match replacement(&job.arguments, job.max_file_bytes) {
        Ok(replacement) => replacement,
        Err(_) => return not_started("edit does not describe one bounded exact replacement"),
    };
    if !result_fits(
        &job.arguments.path,
        &replacement,
        job.execution.output_limit,
    ) {
        return not_started("edit result exceeds its persisted output bound");
    }
    let (parent, leaf) = match open_relative_parent(&job.root, &job.arguments.path) {
        Ok(parent) => parent,
        Err(_) => return not_started("edit target parent is unavailable"),
    };
    let (base_identity, base_mode, base_bytes) =
        match read_target(&parent, &leaf, job.max_file_bytes) {
            Ok(target) => target,
            Err(_) => return not_started("edit target is not a bounded regular file"),
        };
    if base_bytes != job.arguments.expected_content.as_bytes()
        || digest_text_bytes(&base_bytes) != job.arguments.expected_digest
    {
        return not_started("edit target content differs from the prepared base");
    }
    let key = claim_key(&job.execution);
    if !job.admission_allowed(job.live_authority.load(Ordering::SeqCst)) {
        return not_started("live edit authority denied before admission");
    }
    if job.stop.is_cancelled() {
        return not_started("edit cancelled before admission");
    }
    if registry
        .admit(
            &job.execution.workspace,
            key,
            job.resources,
            current_revision,
        )
        .is_err()
    {
        return not_started("workspace claim or revision admission failed");
    }

    // From here on the durable claim is quarantine. Any failure keeps it active
    // unless authenticated terminal evidence proves what happened.
    if job.stop.is_cancelled() || !job.admission_allowed(job.live_authority.load(Ordering::SeqCst))
    {
        return indeterminate(
            "edit authority denied after durable workspace admission",
            None,
        );
    }
    let temporary = temporary_leaf(key);
    let mut staged = match create_temporary(&parent, &temporary) {
        Ok(file) => file,
        Err(_) => return indeterminate("staging file could not be created", None),
    };
    if staged.write_all(&replacement).is_err()
        || fchmod(
            &staged,
            Mode::from_bits_truncate(u16::try_from(base_mode).expect("file mode is masked")),
        )
        .is_err()
        || staged.sync_all().is_err()
    {
        return indeterminate("staging file could not be durably written", None);
    }
    let staged_identity = match physical_identity(&staged) {
        Ok(identity) => identity,
        Err(_) => return indeterminate("staging file lacks physical identity proof", None),
    };
    if staged_identity.device != base_identity.device || parent.sync_all().is_err() {
        return indeterminate("staging file identity or directory sync failed", None);
    }
    let start = match registry_receipt(
        &job.execution.workspace.backend,
        job.execution.action.digest,
        staged_identity,
    ) {
        Ok(receipt) => receipt,
        Err(_) => return indeterminate("staging receipt exceeds its durable bound", None),
    };
    if registry.record_start(key, start.clone()).is_err() {
        return indeterminate("durable start receipt could not be confirmed", Some(&start));
    }

    if job.stop.is_cancelled() || !job.admission_allowed(job.live_authority.load(Ordering::SeqCst))
    {
        return indeterminate(
            "live edit authority revoked after durable start",
            Some(&start),
        );
    }
    match read_target(&parent, &leaf, job.max_file_bytes) {
        Ok((identity, _, content))
            if identity == base_identity
                && content == job.arguments.expected_content.as_bytes() => {}
        _ => {
            return indeterminate("edit target changed after durable start", Some(&start));
        }
    }
    if job.stop.is_cancelled() || !job.admission_allowed(job.live_authority.load(Ordering::SeqCst))
    {
        return indeterminate(
            "live edit authority denied at effect admission",
            Some(&start),
        );
    }
    if renameat(&parent, &temporary, &parent, &leaf).is_err() {
        return indeterminate("atomic replacement outcome is unknown", Some(&start));
    }
    if parent.sync_all().is_err() {
        return indeterminate("replacement directory sync failed", Some(&start));
    }
    #[cfg(test)]
    trip_fault(&job.fault, FAULT_AFTER_RENAME);

    let effect = known_change(&job.arguments.path);
    if registry
        .resolve(
            key,
            TerminalEvidence {
                receipt: start.clone(),
                effect: effect.clone(),
            },
        )
        .is_err()
    {
        return indeterminate("workspace registry resolution is uncertain", Some(&start));
    }
    #[cfg(test)]
    trip_fault(&job.fault, FAULT_AFTER_REGISTRY_COMMIT);

    settled_edit(&job.arguments, &replacement, effect, &start)
}

async fn reconcile_edit(
    boundary: &NativeEditBoundary,
    execution: ToolExecution,
    attempt: ToolAttempt,
) -> ToolAttemptState {
    let Some(arguments) = boundary.prepared_arguments(&execution.action) else {
        return indeterminate("persisted prepared edit action is invalid", None);
    };
    if execution.binding != boundary.binding
        || execution.workspace != boundary.workspace
        || execution.action.authority != ToolAuthority::WorkspaceMutation
    {
        return indeterminate("edit reconciliation binding changed", None);
    }
    let key = claim_key(&execution);
    let mut registry = match WorkspaceRegistry::open(&boundary.registry_directory) {
        Ok(registry) => registry,
        Err(_) => {
            let saved = saved_receipt(&attempt);
            return indeterminate("workspace registry is unavailable", saved.as_ref());
        }
    };
    let claim = match registry.claim(key) {
        Ok(claim) => claim,
        Err(_) => {
            let saved = saved_receipt(&attempt);
            return indeterminate("durable edit claim is unavailable", saved.as_ref());
        }
    };
    if claim.binding != execution.workspace
        || claim.resources != boundary.resources
        || claim.base != arguments.base_revision.into()
    {
        return indeterminate(
            "durable edit claim does not match the prepared action",
            None,
        );
    }
    let Some(start) = claim.start.clone() else {
        let saved = saved_receipt(&attempt);
        return indeterminate(
            "edit has no durable physical start identity",
            saved.as_ref(),
        );
    };
    if saved_receipt(&attempt).is_some_and(|saved| saved != start) {
        return indeterminate("Session and host edit receipts conflict", Some(&start));
    }
    let Some(staged_identity) = parse_registry_receipt(&start, &execution.action.digest) else {
        return indeterminate("durable staged-file identity is invalid", Some(&start));
    };
    let replacement = match replacement(&arguments, boundary.max_file_bytes) {
        Ok(replacement) => replacement,
        Err(_) => return indeterminate("persisted replacement is invalid", Some(&start)),
    };
    if !result_fits(&arguments.path, &replacement, execution.output_limit) {
        return indeterminate(
            "edit result exceeds its persisted output bound",
            Some(&start),
        );
    }
    let effect = known_change(&arguments.path);
    if let Some(terminal) = &claim.terminal {
        if terminal.receipt != start || terminal.effect != effect {
            return indeterminate(
                "terminal host evidence conflicts with the edit",
                Some(&start),
            );
        }
        // Once committed, host evidence is authoritative even if a later
        // cooperating edit has changed the current target.
        return settled_edit(&arguments, &replacement, effect, &start);
    }
    let (parent, leaf) = match open_relative_parent(&boundary.root, &arguments.path) {
        Ok(parent) => parent,
        Err(_) => return indeterminate("edit target parent is unavailable", Some(&start)),
    };
    if !target_matches_staged(
        &parent,
        &leaf,
        staged_identity,
        &replacement,
        boundary.max_file_bytes,
    ) {
        // This includes restored base content and missing/mismatched physical
        // evidence. Never resolve, retry, or infer NoMutation from either case.
        return indeterminate(
            "replacement identity or exact content is unproven",
            Some(&start),
        );
    }
    // A visible rename is not proof of a durable directory entry. In particular
    // the original post-rename fsync may have failed before process loss.
    if boundary.sync_recovered_directory(&parent).is_err() {
        return indeterminate("replacement directory sync is uncertain", Some(&start));
    }
    if registry
        .resolve(
            key,
            TerminalEvidence {
                receipt: start.clone(),
                effect: effect.clone(),
            },
        )
        .is_err()
    {
        return indeterminate("workspace registry resolution is uncertain", Some(&start));
    }
    settled_edit(&arguments, &replacement, effect, &start)
}

fn target_matches_staged(
    parent: &File,
    leaf: &str,
    staged: PhysicalIdentity,
    replacement: &[u8],
    max_file_bytes: usize,
) -> bool {
    read_target(parent, leaf, max_file_bytes).is_ok_and(|(identity, _, content)| {
        identity == staged
            && content == replacement
            && digest_text_bytes(&content) == ContentDigest::of_bytes(replacement).to_string()
    })
}

fn result_fits(path: &str, replacement: &[u8], output_limit: usize) -> bool {
    let result = ToolResult {
        value: json!({
            "path": path,
            "bytes": replacement.len(),
            "digest": ContentDigest::of_bytes(replacement).to_string(),
        }),
        is_error: false,
        capture: crate::OutputCapture::CompleteInline,
    };
    serde_json::to_vec(&result)
        .is_ok_and(|bytes| bytes.len() <= output_limit.min(crate::MAX_TOOL_RECORD_BYTES))
}

fn settled_edit(
    arguments: &EditArguments,
    replacement: &[u8],
    effect: crate::EffectSummary,
    start: &RegistryReceipt,
) -> ToolAttemptState {
    let result = ToolResult {
        value: json!({
            "path": arguments.path,
            "bytes": replacement.len(),
            "digest": ContentDigest::of_bytes(replacement).to_string(),
        }),
        is_error: false,
        capture: crate::OutputCapture::CompleteInline,
    };
    ToolAttemptState::Settled {
        result,
        effect,
        retryable: false,
        receipt: Some(session_receipt(start)),
    }
}

fn session_receipt(receipt: &RegistryReceipt) -> StartReceipt {
    StartReceipt {
        kind: IMPLEMENTATION_ID.to_owned(),
        data: serde_json::to_value(receipt).unwrap_or(Value::Null),
    }
}

fn saved_receipt(attempt: &ToolAttempt) -> Option<RegistryReceipt> {
    let receipt = match &attempt.state {
        ToolAttemptState::IntentCommitted { start_receipt }
        | ToolAttemptState::Indeterminate {
            receipt: start_receipt,
            ..
        }
        | ToolAttemptState::Settled {
            receipt: start_receipt,
            ..
        } => start_receipt.as_ref()?,
        ToolAttemptState::NotStarted { .. } => return None,
    };
    if receipt.kind != IMPLEMENTATION_ID {
        return None;
    }
    serde_json::from_value(receipt.data.clone()).ok()
}

fn registry_receipt(
    backend: &str,
    action_digest: ContentDigest,
    identity: PhysicalIdentity,
) -> Result<RegistryReceipt, ()> {
    let encoded = format!(
        "1:{}:{:016x}:{:016x}:{}:{:09}",
        action_digest,
        identity.device,
        identity.inode,
        identity.birth_seconds,
        identity.birth_nanos,
    );
    if encoded.len() > 160 {
        return Err(());
    }
    Ok(RegistryReceipt {
        backend: backend.to_owned(),
        identity: encoded,
    })
}

fn parse_registry_receipt(
    receipt: &RegistryReceipt,
    action_digest: &ContentDigest,
) -> Option<PhysicalIdentity> {
    if receipt.identity.len() > 160 {
        return None;
    }
    let mut fields = receipt.identity.split(':');
    if fields.next()? != "1" || fields.next()? != action_digest.to_string() {
        return None;
    }
    let identity = PhysicalIdentity {
        device: u64::from_str_radix(fields.next()?, 16).ok()?,
        inode: u64::from_str_radix(fields.next()?, 16).ok()?,
        birth_seconds: fields.next()?.parse().ok()?,
        birth_nanos: fields.next()?.parse().ok()?,
    };
    (fields.next().is_none() && identity.birth_nanos < 1_000_000_000).then_some(identity)
}

fn claim_key(execution: &ToolExecution) -> ClaimKey {
    ClaimKey {
        session: execution.session,
        invocation: execution.invocation,
        attempt: execution.attempt,
    }
}

fn temporary_leaf(key: ClaimKey) -> String {
    format!(
        ".ion-edit-{}-{}-{}",
        key.session,
        key.invocation.get(),
        key.attempt.get()
    )
}

fn known_change(path: &str) -> crate::EffectSummary {
    crate::EffectSummary::KnownChanges {
        paths: vec![path.to_owned()],
    }
}

fn replacement(arguments: &EditArguments, maximum: usize) -> Result<Vec<u8>, ()> {
    if arguments.old_text.is_empty()
        || arguments.old_text == arguments.new_text
        || arguments.expected_content.len() > maximum
        || arguments.expected_digest != digest_text(&arguments.expected_content)
        || arguments.expected_content.len() > maximum
    {
        return Err(());
    }
    let mut found = None;
    for (index, _) in arguments.expected_content.char_indices() {
        if arguments.expected_content[index..].starts_with(&arguments.old_text)
            && found.replace(index).is_some()
        {
            return Err(());
        }
    }
    let start = found.ok_or(())?;
    let end = start.checked_add(arguments.old_text.len()).ok_or(())?;
    let target_len = arguments
        .expected_content
        .len()
        .checked_sub(arguments.old_text.len())
        .and_then(|length| length.checked_add(arguments.new_text.len()))
        .filter(|length| *length <= maximum)
        .ok_or(())?;
    if arguments.expected_content[start..end] != arguments.old_text {
        return Err(());
    }
    let mut target = String::with_capacity(target_len);
    target.push_str(&arguments.expected_content[..start]);
    target.push_str(&arguments.new_text);
    target.push_str(&arguments.expected_content[end..]);
    Ok(target.into_bytes())
}

fn digest_text(text: &str) -> String {
    ContentDigest::of_bytes(text.as_bytes()).to_string()
}

fn digest_text_bytes(text: &[u8]) -> String {
    ContentDigest::of_bytes(text).to_string()
}

fn valid_relative_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains('\0')
        && path.split('/').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.eq_ignore_ascii_case(".git")
        })
}

fn open_absolute_directory(path: &str) -> std::io::Result<File> {
    if path.len() > MAX_PATH_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace root path is too long",
        ));
    }
    let relative = path.strip_prefix('/').ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace root is not absolute",
        )
    })?;
    let mut current = File::from(open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if relative.is_empty() {
        return Ok(current);
    }
    for component in relative.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace root is not canonical",
            ));
        }
        current = File::from(openat(
            &current,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
    }
    Ok(current)
}

fn open_relative_parent(root: &File, path: &str) -> std::io::Result<(File, String)> {
    if !valid_relative_path(path) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid relative path",
        ));
    }
    let mut components = path.split('/').peekable();
    let mut parent = root.try_clone()?;
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            return Ok((parent, component.to_owned()));
        }
        parent = File::from(openat(
            &parent,
            component,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "empty relative path",
    ))
}

fn read_target(
    parent: &File,
    leaf: &str,
    maximum: usize,
) -> std::io::Result<(PhysicalIdentity, u32, Vec<u8>)> {
    let before = statat(parent, leaf, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target is not a regular file",
        ));
    }
    let fd: OwnedFd = openat(
        parent,
        leaf,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let file = File::from(fd);
    if FileType::from_raw_mode(fstat(&file)?.st_mode) != FileType::RegularFile {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target is not a regular file",
        ));
    }
    let metadata = file.metadata()?;
    let identity = physical_identity(&file)?;
    if identity.device != before.st_dev as u64 || identity.inode != before.st_ino as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "target changed during descriptor open",
        ));
    }
    let mut content = Vec::with_capacity(maximum.saturating_add(1));
    file.take(maximum.saturating_add(1) as u64)
        .read_to_end(&mut content)?;
    if content.len() > maximum || std::str::from_utf8(&content).is_err() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "target exceeds its text bound or is not UTF-8",
        ));
    }
    let mode = metadata.permissions().mode() & 0o7777;
    Ok((identity, mode, content))
}

fn create_temporary(parent: &File, leaf: &str) -> std::io::Result<File> {
    let fd = openat(
        parent,
        leaf,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?;
    Ok(File::from(fd))
}

fn physical_identity(file: &File) -> std::io::Result<PhysicalIdentity> {
    let metadata = file.metadata()?;
    let birth = metadata.created().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "filesystem creation time is unavailable",
        )
    })?;
    let since_epoch = birth.duration_since(UNIX_EPOCH).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "filesystem creation time predates Unix epoch",
        )
    })?;
    Ok(PhysicalIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        birth_seconds: since_epoch.as_secs(),
        birth_nanos: since_epoch.subsec_nanos(),
    })
}

fn not_started(reason: &str) -> ToolAttemptState {
    ToolAttemptState::NotStarted {
        reason: reason.to_owned(),
    }
}

fn indeterminate(reason: &str, receipt: Option<&RegistryReceipt>) -> ToolAttemptState {
    ToolAttemptState::Indeterminate {
        reason: reason.to_owned(),
        receipt: receipt.map(session_receipt),
    }
}

fn decode_authority(value: u8) -> LiveToolAuthority {
    match value {
        AUTHORITY_ALLOW => LiveToolAuthority::Allow,
        AUTHORITY_ASK => LiveToolAuthority::Ask,
        _ => LiveToolAuthority::Deny,
    }
}

struct CancelWorkerOnDrop {
    stop: CancellationToken,
    armed: bool,
}

impl CancelWorkerOnDrop {
    fn new(stop: CancellationToken) -> Self {
        Self { stop, armed: true }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for CancelWorkerOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.stop.cancel();
        }
    }
}

impl EditJob {
    fn admission_allowed(&self, policy: u8) -> bool {
        if self.execution.binding != self.binding
            || self.execution.action.authority != ToolAuthority::WorkspaceMutation
            || !self.execution.action.permitted_by(&self.execution.ceiling)
        {
            return false;
        }
        let approved = self.execution.approval.permits(
            &self.execution.action,
            &self.binding.implementation,
            &self.executor,
            &self.execution.workspace,
            now_unix_ms(),
        );
        match policy {
            AUTHORITY_ALLOW => {
                !matches!(
                    self.execution.approval,
                    ApprovalState::Pending | ApprovalState::Denied { .. }
                ) && (!matches!(self.execution.approval, ApprovalState::Approved { .. })
                    || approved)
            }
            AUTHORITY_ASK => approved,
            AUTHORITY_DENY => false,
            _ => false,
        }
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
fn trip_fault(fault: &AtomicU8, point: u8) {
    if fault
        .compare_exchange(point, 0, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        panic!("injected native-edit crash point {point}");
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::*;
    use crate::{AttemptId, AuthorityCeiling, InvocationId, SessionId};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("ion-native-edit-{}-{id}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        root: TestRoot,
        host: TestRoot,
        registry: WorkspaceRegistry,
        boundary: NativeEditBoundary,
    }

    impl Fixture {
        fn new(git: bool) -> Self {
            let root = TestRoot::new();
            if git {
                fs::create_dir(root.path().join(".git")).unwrap();
            }
            fs::write(root.path().join("file.txt"), "alpha beta alpha\n").unwrap();
            let host = TestRoot::new();
            let mut registry = WorkspaceRegistry::open(host.path()).unwrap();
            let workspace = registry
                .bind("test-workspace", root.path(), "local-v1")
                .unwrap();
            let boundary = NativeEditBoundary::new(&registry, workspace, 1024).unwrap();
            boundary.set_live_authority(LiveToolAuthority::Allow);
            Self {
                root,
                host,
                registry,
                boundary,
            }
        }

        fn arguments(&self, old_text: &str, new_text: &str) -> Value {
            let revision = self
                .registry
                .revision(self.boundary.workspace_binding())
                .unwrap();
            let content = fs::read_to_string(self.root.path().join("file.txt")).unwrap();
            json!({
                "path": "file.txt",
                "expected_content": content,
                "workspace_revision": revision,
                "old_text": old_text,
                "new_text": new_text,
            })
        }

        fn execution(&self, action: PreparedAction) -> ToolExecution {
            ToolExecution {
                session: SessionId::new(),
                invocation: InvocationId::new(1).unwrap(),
                attempt: AttemptId::new(1).unwrap(),
                effect_key: "native-edit-test".into(),
                binding: self.boundary.binding(),
                action,
                workspace: self.boundary.workspace.clone(),
                ceiling: AuthorityCeiling {
                    workspace_mutation: true,
                    unconfined_execution: false,
                    remote_tools: false,
                    egress_realms: vec![EgressRealm::Local],
                },
                approval: ApprovalState::NotRequired,
                output_limit: 4096,
                artifacts: crate::ArtifactPublisher::closed(),
            }
        }
    }

    fn attempt(execution: &ToolExecution, state: ToolAttemptState) -> ToolAttempt {
        ToolAttempt {
            id: execution.attempt,
            invocation: execution.invocation,
            ordinal: 1,
            generation: 0,
            executor: SemanticCompatibilityId::new("local-v1").unwrap(),
            progress: None,
            state,
        }
    }

    #[tokio::test]
    async fn edit_requires_exact_expected_content_and_one_occurrence() {
        let fixture = Fixture::new(false);
        let binding = fixture.boundary.binding();
        let arguments = fixture.arguments("alpha", "gamma");
        assert!(matches!(
            crate::tool_boundary::prepare_action(&fixture.boundary, &binding, arguments),
            Err(ToolBoundaryError::InvalidArguments)
        ));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha beta alpha\n"
        );
        let one = fixture.arguments("beta", "gamma");
        let action = fixture.boundary.prepare(one).unwrap();
        assert_eq!(
            action.arguments["expected_digest"],
            digest_text("alpha beta alpha\n")
        );
        assert_eq!(action.authority, ToolAuthority::WorkspaceMutation);
        assert_eq!(action.workspace_revision, Some(0));
        assert_eq!(action.base_facts.len(), 1);
        let state = fixture
            .boundary
            .execute(fixture.execution(action), CancellationToken::new())
            .await;
        assert!(matches!(state, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha gamma alpha\n"
        );
    }

    #[test]
    fn protected_git_metadata_cannot_be_prepared_for_mutation() {
        let fixture = Fixture::new(true);
        fs::write(
            fixture.root.path().join(".git/config"),
            "alpha beta alpha\n",
        )
        .unwrap();
        for path in [".git/config", ".GiT/config", "foo/.git/config"] {
            let mut arguments = fixture.arguments("beta", "gamma");
            arguments["path"] = json!(path);
            assert!(matches!(
                crate::tool_boundary::prepare_action(
                    &fixture.boundary,
                    &fixture.boundary.binding(),
                    arguments
                ),
                Err(ToolBoundaryError::InvalidArguments)
            ));
        }
    }

    #[test]
    fn redirected_git_and_common_admin_roots_cannot_be_edited_or_bound_as_ordinary_files() {
        let root = TestRoot::new();
        let host = TestRoot::new();
        fs::create_dir(root.path().join("admin")).unwrap();
        fs::create_dir(root.path().join("shared")).unwrap();
        fs::write(root.path().join(".git"), "gitdir: admin\n").unwrap();
        fs::write(root.path().join("admin/commondir"), "../shared\n").unwrap();
        fs::write(root.path().join("admin/config"), "alpha beta alpha\n").unwrap();
        fs::write(root.path().join("shared/HEAD"), "alpha beta alpha\n").unwrap();
        let mut registry = WorkspaceRegistry::open(host.path()).unwrap();
        let workspace = registry.bind("workspace", root.path(), "local-v1").unwrap();
        let revision = registry.revision(&workspace).unwrap();
        let boundary = NativeEditBoundary::new(&registry, workspace, 1024).unwrap();
        for path in ["admin/config", "shared/HEAD", ".git"] {
            let arguments = json!({
                "path": path,
                "expected_content": "alpha beta alpha\n",
                "workspace_revision": revision,
                "old_text": "beta",
                "new_text": "gamma",
            });
            assert!(matches!(
                crate::tool_boundary::prepare_action(&boundary, &boundary.binding(), arguments),
                Err(ToolBoundaryError::InvalidArguments)
            ));
        }
        let admin = registry
            .bind("admin", root.path().join("admin"), "local-v1")
            .unwrap();
        assert!(matches!(
            NativeEditBoundary::new(&registry, admin, 1024),
            Err(NativeEditError::InvalidWorkspace)
        ));
    }

    #[tokio::test]
    async fn approval_and_live_policy_are_rechecked_before_atomic_replacement() {
        let fixture = Fixture::new(false);
        fixture.boundary.set_live_authority(LiveToolAuthority::Ask);
        let action = fixture
            .boundary
            .prepare(fixture.arguments("beta", "gamma"))
            .unwrap();
        let mut execution = fixture.execution(action.clone());
        assert_eq!(
            fixture
                .boundary
                .live_authority(&action, &execution.workspace),
            LiveToolAuthority::Ask
        );
        assert!(matches!(
            fixture
                .boundary
                .execute(execution.clone(), CancellationToken::new())
                .await,
            ToolAttemptState::NotStarted { .. }
        ));
        execution.approval = ApprovalState::Approved {
            action_digest: action.digest,
            implementation: execution.binding.implementation.clone(),
            executor: fixture.boundary.executor(),
            workspace: execution.workspace.clone(),
            expires_at_unix_ms: i64::MAX,
        };
        fixture.boundary.set_live_authority(LiveToolAuthority::Deny);
        assert!(matches!(
            fixture
                .boundary
                .execute(execution, CancellationToken::new())
                .await,
            ToolAttemptState::NotStarted { .. }
        ));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha beta alpha\n"
        );
    }

    #[tokio::test]
    async fn crash_after_rename_before_registry_commit_reconciles_only_matching_inode_and_hash() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_RENAME);
        let uncertain = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(uncertain, ToolAttemptState::Indeterminate { .. }));
        let registry = WorkspaceRegistry::open(fixture.host.path()).unwrap();
        let claim = registry
            .claim(claim_key(&execution))
            .expect("durable start survives the fault");
        assert!(claim.start.is_some());
        assert!(claim.terminal.is_none());
        let uncertain_attempt = attempt(&execution, uncertain);
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), uncertain_attempt)
            .await;
        assert!(matches!(recovered, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha gamma alpha\n"
        );
        assert!(
            WorkspaceRegistry::open(fixture.host.path())
                .unwrap()
                .unresolved(None, 8)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn recovery_refuses_to_settle_visible_rename_until_directory_is_durable() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_RENAME);
        let uncertain = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(uncertain, ToolAttemptState::Indeterminate { .. }));
        fixture.boundary.inject_fault(FAULT_RECOVERY_DIRECTORY_SYNC);
        let again = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, uncertain))
            .await;
        assert!(matches!(again, ToolAttemptState::Indeterminate { .. }));
        let registry = WorkspaceRegistry::open(fixture.host.path()).unwrap();
        assert!(
            registry
                .claim(claim_key(&execution))
                .unwrap()
                .terminal
                .is_none()
        );
        assert_eq!(registry.revision(&execution.workspace).unwrap().files, 0);
        let settled = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, again))
            .await;
        assert!(matches!(settled, ToolAttemptState::Settled { .. }));
        assert_eq!(registry.revision(&execution.workspace).unwrap().files, 1);
    }

    #[tokio::test]
    async fn restored_base_or_wrong_hash_keeps_claim_unknown_without_retry() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_RENAME);
        let uncertain = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(uncertain, ToolAttemptState::Indeterminate { .. }));
        fs::write(fixture.root.path().join("file.txt"), "alpha beta alpha\n").unwrap();
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, uncertain))
            .await;
        assert!(matches!(recovered, ToolAttemptState::Indeterminate { .. }));
        assert_eq!(
            WorkspaceRegistry::open(fixture.host.path())
                .unwrap()
                .unresolved(None, 8)
                .unwrap()
                .len(),
            1
        );
        let retry = fixture
            .boundary
            .execute(execution, CancellationToken::new())
            .await;
        assert!(matches!(retry, ToolAttemptState::NotStarted { .. }));
    }

    #[tokio::test]
    async fn crash_after_registry_commit_before_session_evidence_reconciles_without_execute() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_REGISTRY_COMMIT);
        let uncertain = fixture
            .boundary
            .execute(execution.clone(), CancellationToken::new())
            .await;
        assert!(matches!(uncertain, ToolAttemptState::Indeterminate { .. }));
        let claim = WorkspaceRegistry::open(fixture.host.path())
            .unwrap()
            .claim(claim_key(&execution))
            .unwrap();
        assert!(claim.terminal.is_some());
        let recovered = fixture
            .boundary
            .reconcile(execution.clone(), attempt(&execution, uncertain))
            .await;
        assert!(matches!(recovered, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha gamma alpha\n"
        );
    }

    #[tokio::test]
    async fn committed_terminal_edit_reconciles_after_a_later_cooperating_mutation() {
        let fixture = Fixture::new(false);
        let first = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        fixture.boundary.inject_fault(FAULT_AFTER_REGISTRY_COMMIT);
        let lost_session_reply = fixture
            .boundary
            .execute(first.clone(), CancellationToken::new())
            .await;
        assert!(matches!(
            lost_session_reply,
            ToolAttemptState::Indeterminate { .. }
        ));

        let mut second = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("gamma", "delta"))
                .unwrap(),
        );
        second.invocation = InvocationId::new(2).unwrap();
        second.attempt = AttemptId::new(2).unwrap();
        let state = fixture
            .boundary
            .execute(second, CancellationToken::new())
            .await;
        assert!(matches!(state, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha delta alpha\n"
        );

        let recovered = fixture
            .boundary
            .reconcile(first.clone(), attempt(&first, lost_session_reply))
            .await;
        assert!(matches!(recovered, ToolAttemptState::Settled { .. }));
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "alpha delta alpha\n"
        );
    }

    #[test]
    fn resource_scope_distinguishes_git_metadata_from_non_git_roots() {
        let non_git = Fixture::new(false);
        assert_eq!(non_git.boundary.resources, WorkspaceResources::Files);
        let git = Fixture::new(true);
        assert_eq!(
            git.boundary.resources,
            WorkspaceResources::FilesAndRepository
        );
    }

    #[test]
    fn identity_binds_digest_and_creation_time_within_registry_limit() {
        let fixture = Fixture::new(false);
        let execution = fixture.execution(
            fixture
                .boundary
                .prepare(fixture.arguments("beta", "gamma"))
                .unwrap(),
        );
        let path = fixture.root.path().join("file.txt");
        let file = File::open(path).unwrap();
        let identity = physical_identity(&file).unwrap();
        let receipt = registry_receipt(
            &execution.workspace.backend,
            execution.action.digest,
            identity,
        )
        .unwrap();
        assert!(receipt.identity.len() <= 160);
        assert_eq!(
            parse_registry_receipt(&receipt, &execution.action.digest),
            Some(identity)
        );
        assert!(parse_registry_receipt(&receipt, &ContentDigest::of_bytes(b"wrong")).is_none());
    }
}
